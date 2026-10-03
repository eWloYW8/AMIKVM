//! Native lock state. Windows observes locks before WebView2 input dispatch.
#[derive(Clone, Copy, Default)]
pub struct Observation {
    pub bits: u8,
    pub readable: u8,
    pub writable: u8,
}

#[cfg(target_os = "linux")]
mod platform {
    use super::Observation;
    use gtk::{gdk, glib::prelude::*, glib::translate::ToGlibPtr};
    use std::sync::OnceLock;
    use x11_dl::xlib;

    fn display() -> Result<(gdk::Display, *mut xlib::Display, &'static xlib::Xlib), String> {
        let display = gdk::Display::default().ok_or("无法读取本机锁定键状态")?;
        if !display.type_().name().contains("X11") {
            return Err("Wayland 不提供修改本机锁定键状态的公共接口".into());
        }
        unsafe extern "C" {
            fn gdk_x11_display_get_xdisplay(
                display: *mut gdk::ffi::GdkDisplay,
            ) -> *mut xlib::Display;
        }
        static XLIB: OnceLock<Option<xlib::Xlib>> = OnceLock::new();
        let api = XLIB
            .get_or_init(|| xlib::Xlib::open().ok())
            .as_ref()
            .ok_or("无法读取本机锁定键状态")?;
        let pointer: *mut gdk::ffi::GdkDisplay = display.to_glib_none().0;
        let raw = unsafe { gdk_x11_display_get_xdisplay(pointer) };
        if raw.is_null() {
            return Err("无法读取本机锁定键状态".into());
        }
        Ok((display, raw, api))
    }
    const NAMES: [(u8, &std::ffi::CStr); 3] =
        [(1, c"Num Lock"), (2, c"Caps Lock"), (4, c"Scroll Lock")];
    pub fn read() -> Result<Observation, String> {
        if let Some(display) = gdk::Display::default() {
            if !display.type_().name().contains("X11") {
                let map = gdk::Keymap::for_display(&display).ok_or("无法读取本机锁定键状态")?;
                return Ok(Observation {
                    bits: u8::from(map.is_num_locked())
                        | (u8::from(map.is_caps_locked()) << 1)
                        | (u8::from(map.is_scroll_locked()) << 2),
                    readable: 7,
                    writable: 0,
                });
            }
        }
        let (_display, raw, api) = display()?;
        let mut out = Observation::default();
        unsafe {
            for (bit, name) in NAMES {
                let atom = (api.XInternAtom)(raw, name.as_ptr(), 1);
                if atom == 0 {
                    continue;
                }
                let mut value = 0;
                if (api.XkbGetNamedIndicator)(
                    raw,
                    atom,
                    std::ptr::null_mut(),
                    &mut value,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) != 0
                {
                    out.readable |= bit;
                    out.writable |= bit;
                    if value != 0 {
                        out.bits |= bit;
                    }
                }
            }
        }
        if out.readable == 0 {
            Err("无法读取本机锁定键状态".into())
        } else {
            Ok(out)
        }
    }
    pub fn write(bits: u8, mask: u8) -> Result<(), String> {
        let (_display, raw, api) = display()?;
        unsafe {
            let mut affect = 0;
            let mut values = 0;
            for (bit, keysym) in [(1, 0xff7f), (2, 0xffe5), (4, 0xff14)] {
                if mask & bit == 0 {
                    continue;
                }
                // Num/Scroll are not guaranteed to use Mod2/Mod3 in a user's map.
                let modifiers = (api.XkbKeysymToModifiers)(raw, keysym);
                affect |= modifiers;
                if bits & bit != 0 {
                    values |= modifiers;
                }
                let name = NAMES.iter().find(|(b, _)| *b == bit).unwrap().1;
                let atom = (api.XInternAtom)(raw, name.as_ptr(), 1);
                if atom != 0
                    && (api.XkbSetNamedIndicator)(
                        raw,
                        atom,
                        1,
                        i32::from(bits & bit != 0),
                        0,
                        std::ptr::null_mut(),
                    ) == 0
                {
                    return Err("无法修改本机锁定键状态".into());
                }
            }
            if affect != 0 && (api.XkbLockModifiers)(raw, 0x100, affect, values) == 0 {
                return Err("无法修改本机锁定键状态".into());
            }
            (api.XSync)(raw, 0);
        }
        Ok(())
    }
    pub fn install(_app: &tauri::AppHandle) -> Result<(), String> {
        Ok(())
    }
    pub fn uninstall() {}
}

#[cfg(target_os = "windows")]
mod platform {
    use super::Observation;
    use std::sync::{
        Mutex,
        atomic::{AtomicPtr, AtomicU8, AtomicU32, Ordering},
    };
    use windows_sys::Win32::{
        Foundation::*,
        System::Threading::{GetCurrentProcessId, GetCurrentThreadId},
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    };
    const TAG: usize = 0x414d494b;
    static HOOK: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    static THREAD: AtomicU32 = AtomicU32::new(0);
    static BITS: AtomicU8 = AtomicU8::new(0);
    static DOWN: AtomicU8 = AtomicU8::new(0);
    static WORKER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
    thread_local! {
        static EVENTS: std::cell::RefCell<Option<tokio::sync::mpsc::UnboundedSender<(&'static str, bool)>>> = const { std::cell::RefCell::new(None) };
    }

    unsafe extern "system" fn observe(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // Do not block our injected keys: Windows still needs to toggle its
        // state and LEDs. Only native, untagged events may reach the remote HID.
        let result = unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
        if code != HC_ACTION as i32 || result != 0 {
            return result;
        }
        let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let (bit, name) = match event.vkCode as u16 {
            VK_NUMLOCK => (1, "NumLock"),
            VK_CAPITAL => (2, "CapsLock"),
            VK_SCROLL => (4, "ScrollLock"),
            _ => return result,
        };
        let pressed = match wparam as u32 {
            WM_KEYDOWN | WM_SYSKEYDOWN => true,
            WM_KEYUP | WM_SYSKEYUP => false,
            _ => return result,
        };
        let was_down = if pressed {
            DOWN.fetch_or(bit, Ordering::AcqRel) & bit != 0
        } else {
            DOWN.fetch_and(!bit, Ordering::AcqRel) & bit != 0
        };
        if pressed && !was_down {
            BITS.fetch_xor(bit, Ordering::AcqRel);
        }
        if event.dwExtraInfo == TAG || (pressed && was_down) {
            return result;
        }
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
        if pid == unsafe { GetCurrentProcessId() } {
            EVENTS.with(|events| {
                if let Some(events) = events.borrow().as_ref() {
                    let _ = events.send((name, pressed));
                }
            });
        }
        result
    }

    pub fn install(app: &tauri::AppHandle) -> Result<(), String> {
        let (events, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let app = app.clone();
        let (ready, startup) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("keyboard-locks".into())
            .spawn(move || {
                let mut bits = 0;
                let mut down = 0;
                for (bit, key) in [(1, VK_NUMLOCK), (2, VK_CAPITAL), (4, VK_SCROLL)] {
                    let state = unsafe { GetKeyState(key as i32) };
                    if state & 1 != 0 {
                        bits |= bit;
                    }
                    if state < 0 {
                        down |= bit;
                    }
                }
                BITS.store(bits, Ordering::Release);
                DOWN.store(down, Ordering::Release);
                let mut message: MSG = unsafe { std::mem::zeroed() };
                unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
                let hook = unsafe {
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(observe), std::ptr::null_mut(), 0)
                };
                if hook.is_null() {
                    let _ = ready.send(Err("无法安装本机锁定键消息过滤器".to_owned()));
                    return;
                }
                EVENTS.with(|sender| *sender.borrow_mut() = Some(events));
                THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
                HOOK.store(hook, Ordering::Release);
                let _ = ready.send(Ok(()));
                while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {}
                HOOK.store(std::ptr::null_mut(), Ordering::Release);
                THREAD.store(0, Ordering::Release);
                unsafe { UnhookWindowsHookEx(hook) };
                EVENTS.with(|sender| *sender.borrow_mut() = None);
            })
            .map_err(|_| "无法安装本机锁定键消息过滤器".to_owned())?;
        let result = startup
            .recv()
            .map_err(|_| "无法安装本机锁定键消息过滤器".to_owned())?;
        if let Err(error) = result {
            let _ = worker.join();
            return Err(error);
        }
        *WORKER.lock().map_err(|_| "无法安装本机锁定键消息过滤器")? = Some(worker);
        tauri::async_runtime::spawn(async move {
            // Preserve native press/release ordering independently of WebView2.
            while let Some((code, pressed)) = receive.recv().await {
                super::super::physical_event(&app, code, pressed).await;
            }
        });
        Ok(())
    }
    pub fn uninstall() {
        let thread = THREAD.load(Ordering::Acquire);
        if thread != 0 {
            unsafe { PostThreadMessageW(thread, WM_QUIT, 0, 0) };
        }
        if let Ok(mut worker) = WORKER.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
    pub fn handles_input() -> bool {
        !HOOK.load(Ordering::Acquire).is_null()
    }
    pub fn read() -> Result<Observation, String> {
        if !handles_input() {
            return Ok(Observation::default());
        }
        // GetKeyState on the Tauri UI thread can be stale when WebView2 owns
        // the input queue. The dedicated observer tracks actual transitions.
        Ok(Observation {
            bits: BITS.load(Ordering::Acquire),
            readable: 7,
            writable: if HOOK.load(Ordering::Acquire).is_null() {
                0
            } else {
                7
            },
        })
    }
    pub fn write(bits: u8, mask: u8) -> Result<(), String> {
        if HOOK.load(Ordering::Acquire).is_null() {
            return Err("无法安装本机锁定键消息过滤器".into());
        }
        let current = read()?;
        if DOWN.load(Ordering::Acquire) & mask & (current.bits ^ bits) != 0 {
            return Err("本机锁定键仍被按住".into());
        }
        let mut inputs = Vec::new();
        for (bit, key) in [(1, VK_NUMLOCK), (2, VK_CAPITAL), (4, VK_SCROLL)] {
            if (current.bits ^ bits) & mask & bit == 0 {
                continue;
            }
            let extended = if key == VK_NUMLOCK {
                KEYEVENTF_EXTENDEDKEY
            } else {
                0
            };
            for flags in [extended, extended | KEYEVENTF_KEYUP] {
                inputs.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: key,
                            wScan: 0,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: TAG,
                        },
                    },
                });
            }
        }
        if inputs.is_empty() {
            return Ok(());
        }
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        } as usize;
        if sent != inputs.len() {
            if sent % 2 == 1 {
                unsafe { SendInput(1, &inputs[sent], std::mem::size_of::<INPUT>() as i32) };
            }
            return Err("无法修改本机锁定键状态".into());
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::Observation;
    use std::ffi::{c_char, c_void};
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(port: u32, matching: *mut c_void) -> u32;
        fn IOServiceOpen(service: u32, task: u32, kind: u32, connection: *mut u32) -> i32;
        fn IOObjectRelease(object: u32) -> i32;
        fn IOServiceClose(connection: u32) -> i32;
        fn IOHIDGetModifierLockState(connection: u32, selector: i32, value: *mut bool) -> i32;
        fn IOHIDSetModifierLockState(connection: u32, selector: i32, value: bool) -> i32;
    }
    unsafe extern "C" {
        static mach_task_self_: u32;
    }
    struct Connection(u32);
    impl Drop for Connection {
        fn drop(&mut self) {
            unsafe {
                IOServiceClose(self.0);
            }
        }
    }
    fn open() -> Result<Connection, String> {
        unsafe {
            let matching = IOServiceMatching(c"IOHIDSystem".as_ptr());
            if matching.is_null() {
                return Err("无法读取本机锁定键状态".into());
            }
            let service = IOServiceGetMatchingService(0, matching);
            if service == 0 {
                return Err("无法读取本机锁定键状态".into());
            }
            let mut connection = 0;
            let result = IOServiceOpen(service, mach_task_self_, 1, &mut connection);
            IOObjectRelease(service);
            if result != 0 {
                return Err(format!("IOHIDSystem: 0x{:08x}", result as u32));
            }
            Ok(Connection(connection))
        }
    }
    pub fn read() -> Result<Observation, String> {
        let connection = open()?;
        let mut out = Observation::default();
        for (bit, selector) in [(1, 2), (2, 1)] {
            let mut value = false;
            if unsafe { IOHIDGetModifierLockState(connection.0, selector, &mut value) } == 0 {
                out.readable |= bit;
                out.writable |= bit;
                if value {
                    out.bits |= bit;
                }
            }
        }
        if out.readable == 0 {
            Err("无法读取本机锁定键状态".into())
        } else {
            Ok(out)
        }
    }
    pub fn write(bits: u8, mask: u8) -> Result<(), String> {
        let connection = open()?;
        for (bit, selector) in [(1, 2), (2, 1)] {
            if mask & bit == 0 {
                continue;
            }
            let result =
                unsafe { IOHIDSetModifierLockState(connection.0, selector, bits & bit != 0) };
            if result != 0 {
                return Err(format!(
                    "IOHIDSetModifierLockState: 0x{:08x}",
                    result as u32
                ));
            }
        }
        Ok(())
    }
    pub fn install(_app: &tauri::AppHandle) -> Result<(), String> {
        Ok(())
    }
    pub fn uninstall() {}
}

pub use platform::{install, read, uninstall, write};

#[cfg(target_os = "windows")]
pub use platform::handles_input;
