//! Restore toolkit key-up delivery when the active XKB group has no symbol.
//! Observe the existing GDK seat; never grab the keyboard or synthesize a press.
use amikvm_core::input::releases::Releases;
use glib::translate::*;
use gtk::{gdk, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    ffi::{c_char, c_int, c_void},
    fs::File,
    os::fd::FromRawFd,
    ptr,
    rc::Rc,
};

struct Bridge {
    active: Cell<bool>,
    window: glib::WeakRef<gtk::Window>,
    keys: RefCell<Releases<gdk::EventKey>>,
}
impl Bridge {
    fn deliver(&self, releases: Vec<(gdk::EventKey, u32)>) {
        if !self.active.get() {
            return;
        }
        let Some(window) = self.window.upgrade().filter(|w| w.is_active()) else {
            return;
        };
        let modifiers = gdk::Keymap::for_display(&window.display()).map(|k| k.modifier_state());
        for (mut event, time) in releases {
            if !self.active.get() || !window.is_active() {
                break;
            }
            // EventKey owns a gdk_event_copy, including window, device and
            // scancode metadata. Its key press/release storage has the same ABI.
            unsafe {
                let native = event.to_glib_none_mut().0;
                (*native).type_ = gdk::ffi::GDK_KEY_RELEASE;
                (*native).time = time;
                (*native).send_event = 1;
                if let Some(modifiers) = modifiers {
                    (*native).state = modifiers;
                }
                // Deliver before a subsequent press, preserving physical
                // cycle order even when several Wayland events arrive at once.
                gtk::ffi::gtk_main_do_event(native.cast());
            }
        }
    }
}

pub fn install(window: &gtk::Window) {
    let display = window.display();
    if !display.type_().name().contains("Wayland") {
        return;
    }
    for seat in display.list_seats() {
        attach(window, seat);
    }
    let weak = window.downgrade();
    display.connect_seat_added(move |_, seat| {
        if let Some(window) = weak.upgrade() {
            attach(&window, seat.clone());
        }
    });
}

fn attach(window: &gtk::Window, seat: gdk::Seat) {
    let bridge = Rc::new(Bridge {
        active: Cell::new(true),
        window: window.downgrade(),
        keys: RefCell::new(Releases::default()),
    });
    let observer = Rc::new(RefCell::new(Keyboard::new(&seat, bridge.clone())));
    let pressed = bridge.clone();
    let source = seat.clone();
    window.connect_key_press_event(move |_, event| {
        if pressed.active.get() && !event.is_send_event() && event.seat().as_ref() == Some(&source)
        {
            let releases = pressed.keys.borrow_mut().capture(
                event.hardware_keycode(),
                event.time(),
                event.clone(),
            );
            // Drop the tracker borrow before re-entering GTK's key-up handler.
            pressed.deliver(releases);
        }
        glib::Propagation::Proceed
    });
    let released = bridge.clone();
    let source = seat.clone();
    window.connect_key_release_event(move |_, event| {
        if released.active.get() && !event.is_send_event() && event.seat().as_ref() == Some(&source)
        {
            released
                .keys
                .borrow_mut()
                .delivered(event.hardware_keycode(), event.time());
        }
        glib::Propagation::Proceed
    });
    let unfocused = bridge.clone();
    window.connect_focus_out_event(move |_, _| {
        unfocused.keys.borrow_mut().clear();
        glib::Propagation::Proceed
    });
    let available = observer.clone();
    let incoming = bridge.clone();
    seat.connect_device_added(move |seat, _| {
        if incoming.active.get() && available.borrow().is_none() {
            *available.borrow_mut() = Keyboard::new(seat, incoming.clone());
        }
    });
    let removed = bridge.clone();
    seat.connect_device_removed(move |seat, _| {
        if !seat
            .capabilities()
            .contains(gdk::SeatCapabilities::KEYBOARD)
        {
            removed.keys.borrow_mut().clear();
        }
    });
    let removed = bridge.clone();
    let keyboard = observer.clone();
    window
        .display()
        .connect_seat_removed(move |_, removed_seat| {
            if removed_seat == &seat {
                removed.active.set(false);
                removed.keys.borrow_mut().clear();
                keyboard.borrow_mut().take();
            }
        });
    window.connect_destroy(move |_| {
        bridge.active.set(false);
        bridge.keys.borrow_mut().clear();
        observer.borrow_mut().take();
    });
}

#[repr(C)]
struct Interface {
    name: *const c_char,
    version: c_int,
    method_count: c_int,
    methods: *const c_void,
    event_count: c_int,
    events: *const c_void,
}
#[link(name = "gdk-3")]
unsafe extern "C" {
    fn gdk_wayland_seat_get_wl_seat(seat: *mut gdk::ffi::GdkSeat) -> *mut c_void;
}
#[link(name = "wayland-client")]
unsafe extern "C" {
    static wl_keyboard_interface: Interface;
    fn wl_proxy_get_version(proxy: *mut c_void) -> u32;
    fn wl_proxy_marshal_flags(
        proxy: *mut c_void,
        opcode: u32,
        interface: *const Interface,
        version: u32,
        flags: u32,
        ...
    ) -> *mut c_void;
    fn wl_proxy_add_listener(
        proxy: *mut c_void,
        listener: *const Listener,
        data: *mut c_void,
    ) -> c_int;
    fn wl_proxy_destroy(proxy: *mut c_void);
}
#[repr(C)]
struct Listener {
    keymap: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, c_int, u32),
    enter: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *mut c_void),
    leave: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void),
    key: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, u32, u32),
    modifiers: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, u32, u32, u32),
    repeat: unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, c_int),
}
struct Keyboard {
    proxy: *mut c_void,
    _bridge: Box<Rc<Bridge>>,
}
impl Keyboard {
    fn new(seat: &gdk::Seat, bridge: Rc<Bridge>) -> Option<Self> {
        if !seat
            .capabilities()
            .contains(gdk::SeatCapabilities::KEYBOARD)
        {
            return None;
        }
        // Borrow only GDK's wl_seat; the new wl_keyboard and its listener data
        // belong to this guard and share GDK's main-thread event queue.
        unsafe {
            let seat = gdk_wayland_seat_get_wl_seat(seat.to_glib_none().0);
            if seat.is_null() {
                return None;
            }
            let version = wl_proxy_get_version(seat).min(wl_keyboard_interface.version as u32);
            let proxy = wl_proxy_marshal_flags(
                seat,
                1, // wl_seat.get_keyboard
                ptr::addr_of!(wl_keyboard_interface),
                version,
                0,
                ptr::null::<c_void>(),
            );
            if proxy.is_null() {
                return None;
            }
            let mut observer = Self {
                proxy,
                _bridge: Box::new(bridge),
            };
            if wl_proxy_add_listener(
                proxy,
                &LISTENER,
                ptr::from_mut(observer._bridge.as_mut()).cast(),
            ) != 0
            {
                return None;
            }
            Some(observer)
        }
    }
}
impl Drop for Keyboard {
    fn drop(&mut self) {
        // The guard is retained by the GTK window and destroyed on its UI
        // thread while the GDK seat/display still exist. Listener data is freed
        // only after its owned proxy stops receiving events.
        unsafe {
            let version = wl_proxy_get_version(self.proxy);
            if version >= 3 {
                wl_proxy_marshal_flags(self.proxy, 0, ptr::null(), version, 1);
            } else {
                wl_proxy_destroy(self.proxy);
            }
        }
    }
}

unsafe extern "C" fn keymap(_: *mut c_void, _: *mut c_void, _: u32, fd: c_int, _: u32) {
    // Each observer owns its received FD; GDK alone interprets the keymap.
    if fd >= 0 {
        drop(unsafe { File::from_raw_fd(fd) });
    }
}
unsafe extern "C" fn enter(_: *mut c_void, _: *mut c_void, _: u32, _: *mut c_void, _: *mut c_void) {
}
unsafe extern "C" fn leave(data: *mut c_void, _: *mut c_void, _: u32, _: *mut c_void) {
    unsafe { &*data.cast::<Rc<Bridge>>() }
        .keys
        .borrow_mut()
        .clear();
}
unsafe extern "C" fn key(
    data: *mut c_void,
    _: *mut c_void,
    _: u32,
    time: u32,
    key: u32,
    state: u32,
) {
    let bridge = unsafe { &*data.cast::<Rc<Bridge>>() };
    let Some(hardware) = key.checked_add(8).and_then(|key| u16::try_from(key).ok()) else {
        return;
    };
    if state == 1 {
        bridge.keys.borrow_mut().pressed(hardware, time);
    } else if state == 0 {
        bridge.keys.borrow_mut().released(hardware, time);
        let bridge = bridge.clone();
        // GDK may still have a normal key-up queued. Wait for its event source
        // before filling only the releases it actually omitted.
        glib::idle_add_local_once(move || {
            let releases = bridge.keys.borrow_mut().drain();
            bridge.deliver(releases);
        });
    }
}
unsafe extern "C" fn modifiers(
    _: *mut c_void,
    _: *mut c_void,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
) {
}
unsafe extern "C" fn repeat(_: *mut c_void, _: *mut c_void, _: c_int, _: c_int) {}
static LISTENER: Listener = Listener {
    keymap,
    enter,
    leave,
    key,
    modifiers,
    repeat,
};
