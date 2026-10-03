//! Host input-source discovery runs on the native UI thread; no JVM/JNI dependency.
use amikvm_core::input::layout::{self, Layout};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
pub mod locks;
pub mod native;

#[derive(Default, Clone, PartialEq)]
pub struct Snapshot {
    pub layout: Option<Layout>,
    pub native_name: Option<String>,
    pub notice: Option<String>,
    pub ambiguous: Vec<Layout>,
}
#[derive(Default)]
pub struct Host {
    pub snapshot: Mutex<Snapshot>,
    pub locks: Mutex<locks::State>,
    #[cfg(target_os = "linux")]
    pub group: Mutex<Option<u8>>,
}

fn refresh(app: &AppHandle) {
    let state = app.state::<crate::commands::AppState>();
    let detection = detect(&state.host_keyboard);
    if let Ok(mut current) = state.host_keyboard.snapshot.lock() {
        if *current != detection {
            *current = detection;
            let _ = app.emit("ui-changed", serde_json::json!({}));
        }
    }
}

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    locks::install(app);
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        if let Some(window) = app.get_webview_window("main") {
            let handle = app.clone();
            window
                .gtk_window()?
                .connect_key_press_event(move |_, event| {
                    if let Ok(mut group) = handle
                        .state::<crate::commands::AppState>()
                        .host_keyboard
                        .group
                        .lock()
                    {
                        *group = Some(event.group());
                    }
                    refresh(&handle);
                    gtk::glib::Propagation::Proceed
                });
        }
    }
    refresh(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if app
                .state::<crate::commands::AppState>()
                .shutting_down
                .load(std::sync::atomic::Ordering::Acquire)
            {
                break;
            }
            let handle = app.clone();
            if app.run_on_main_thread(move || refresh(&handle)).is_err() {
                break;
            }
        }
    });
    Ok(())
}

fn identified(layout: Option<Layout>, native_name: String) -> Snapshot {
    Snapshot {
        layout,
        native_name: Some(native_name),
        notice: if layout.is_none() {
            Some("未识别出原版支持的布局，请手动选择；实体按键仍按物理位置发送。".into())
        } else {
            None
        },
        ..Default::default()
    }
}

#[cfg(target_os = "windows")]
fn detect(_: &Host) -> Snapshot {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetKeyboardLayoutNameW(name: *mut u16) -> i32;
    }
    let mut name = [0u16; 9];
    // GetKeyboardLayoutNameW writes KL_NAMELENGTH UTF-16 units on the UI thread.
    if unsafe { GetKeyboardLayoutNameW(name.as_mut_ptr()) } == 0 {
        return Snapshot {
            notice: Some("无法读取当前 Windows 键盘布局".into()),
            ..Default::default()
        };
    }
    let name = String::from_utf16_lossy(&name[..8]);
    identified(layout::from_windows(&name), name)
}

#[cfg(target_os = "macos")]
fn detect(_: &Host) -> Snapshot {
    use std::ffi::{CStr, c_char, c_void};
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn TISCopyCurrentKeyboardLayoutInputSource() -> *const c_void;
        fn TISGetInputSourceProperty(source: *const c_void, key: *const c_void) -> *const c_void;
        static kTISPropertyInputSourceID: *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringGetCString(
            value: *const c_void,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(value: *const c_void);
    }
    // Copy retains the source; its property is borrowed and remains valid until release.
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return Snapshot {
                notice: Some("无法读取当前 macOS 键盘布局".into()),
                ..Default::default()
            };
        }
        let value = TISGetInputSourceProperty(source, kTISPropertyInputSourceID);
        let mut text = [0i8; 1024];
        let ok = !value.is_null()
            && CFStringGetCString(value, text.as_mut_ptr(), text.len() as isize, 0x08000100) != 0;
        let name = if ok {
            Some(CStr::from_ptr(text.as_ptr()).to_string_lossy().into_owned())
        } else {
            None
        };
        CFRelease(source);
        match name {
            Some(name) => identified(layout::from_macos(&name), name),
            None => Snapshot {
                notice: Some("无法读取当前 macOS 键盘布局标识".into()),
                ..Default::default()
            },
        }
    }
}

#[cfg(target_os = "linux")]
fn detect(host: &Host) -> Snapshot {
    use gtk::{gdk, glib::prelude::*};
    let Some(display) = gdk::Display::default() else {
        return Snapshot::default();
    };
    if display.type_().name().contains("X11") {
        return x11(&display).unwrap_or_else(|| Snapshot {
            notice: Some("无法读取 X11 键盘布局，请手动选择。".into()),
            ..Default::default()
        });
    }
    // GDK exposes Wayland groups on native key events but not a public source identifier.
    let Some(group) = host.group.lock().ok().and_then(|g| *g) else {
        return Snapshot {
            notice: Some("按下一个实体键后识别当前键盘字符；也可手动选择布局。".into()),
            ..Default::default()
        };
    };
    let Some(keymap) = gdk::Keymap::for_display(&display) else {
        return Snapshot::default();
    };
    let positions: std::collections::BTreeSet<_> = layout::positions(Layout::Us)
        .iter()
        .chain(layout::positions(Layout::Jp))
        .copied()
        .collect();
    let mut observed = vec![];
    for code in positions {
        let Some(keycode) = native::hardware_code(code) else {
            continue;
        };
        for shift in [false, true] {
            if let Some((value, _, _, _)) = keymap.translate_keyboard_state(
                keycode,
                if shift {
                    gdk::ModifierType::SHIFT_MASK
                } else {
                    gdk::ModifierType::empty()
                },
                group as i32,
            ) {
                if let Some(c) = gdk::keys::Key::from(value).to_unicode() {
                    if !c.is_control() {
                        observed.push((code, shift, c));
                    }
                }
            }
        }
    }
    let candidates = layout::from_glyphs(&observed);
    if candidates.len() == 1 {
        identified(Some(candidates[0]), format!("GDK 字符识别 · group {group}"))
    } else {
        Snapshot {
            native_name: Some(format!("GDK group {group}")),
            notice: candidates
                .is_empty()
                .then(|| "当前字符布局无法识别，请手动选择。".into()),
            ambiguous: candidates,
            ..Default::default()
        }
    }
}

#[cfg(target_os = "linux")]
fn x11(display: &gtk::gdk::Display) -> Option<Snapshot> {
    use gtk::glib::translate::ToGlibPtr;
    use std::{ptr, sync::OnceLock};
    use x11_dl::xlib;
    #[link(name = "gdk-3")]
    unsafe extern "C" {
        fn gdk_x11_display_get_xdisplay(
            display: *mut gtk::gdk::ffi::GdkDisplay,
        ) -> *mut xlib::Display;
    }
    // Match XKBstr.h exactly. x11-dl 2.21's XkbStateRec has a different field order.
    #[repr(C)]
    #[derive(Default)]
    struct State {
        group: u8,
        locked_group: u8,
        base_group: u16,
        latched_group: u16,
        mods: u8,
        base_mods: u8,
        latched_mods: u8,
        locked_mods: u8,
        compat_state: u8,
        grab_mods: u8,
        compat_grab_mods: u8,
        lookup_mods: u8,
        compat_lookup_mods: u8,
        ptr_buttons: u16,
    }
    static LIB: OnceLock<Option<xlib::Xlib>> = OnceLock::new();
    let lib = LIB.get_or_init(|| xlib::Xlib::open().ok()).as_ref()?;
    // Borrow GDK's display exclusively on its owning UI thread. Property storage is XFree-owned.
    unsafe {
        let display = gdk_x11_display_get_xdisplay(display.to_glib_none().0);
        if display.is_null() {
            return None;
        }
        let mut state = State::default();
        if (lib.XkbGetState)(display, 0x100, (&mut state as *mut State).cast()) != 0 {
            return None;
        }
        let atom = (lib.XInternAtom)(display, c"_XKB_RULES_NAMES".as_ptr(), 1);
        if atom == 0 {
            return None;
        }
        let (mut actual, mut format, mut count, mut remaining, mut data) =
            (0, 0, 0, 0, ptr::null_mut());
        let result = (lib.XGetWindowProperty)(
            display,
            (lib.XDefaultRootWindow)(display),
            atom,
            0,
            1024,
            0,
            xlib::XA_STRING,
            &mut actual,
            &mut format,
            &mut count,
            &mut remaining,
            &mut data,
        );
        if result != 0
            || data.is_null()
            || actual != xlib::XA_STRING
            || format != 8
            || remaining != 0
            || count > 4096
        {
            if !data.is_null() {
                (lib.XFree)(data.cast());
            }
            return None;
        }
        let bytes = std::slice::from_raw_parts(data, count as usize);
        let fields: Vec<String> = bytes
            .split(|b| *b == 0)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        (lib.XFree)(data.cast());
        let group = state.group as usize;
        let name = fields.get(2)?.split(',').nth(group)?;
        let variant = fields
            .get(3)
            .and_then(|v| v.split(',').nth(group))
            .unwrap_or("");
        Some(identified(
            layout::from_xkb(name, variant),
            if variant.is_empty() {
                name.to_owned()
            } else {
                format!("{name}({variant})")
            },
        ))
    }
}
