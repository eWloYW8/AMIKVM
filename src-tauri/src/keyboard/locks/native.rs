//! Native lock state. Call on the UI thread, including Windows' message filter.
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
    pub fn install() -> Result<(), String> {
        Ok(())
    }
    pub fn uninstall() {}
}

#[cfg(target_os = "windows")]
mod platform {
    use super::Observation;
    use std::sync::atomic::{AtomicPtr, Ordering};
    use windows_sys::Win32::{
        Foundation::*,
        System::Threading::GetCurrentThreadId,
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    };
    const TAG: usize = 0x414d494b;
    static HOOK: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    unsafe extern "system" fn filter(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0
            && wparam == PM_REMOVE as usize
            && unsafe { GetMessageExtraInfo() } as usize == TAG
        {
            let message = unsafe { &mut *(lparam as *mut MSG) };
            if matches!(
                message.message,
                WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
            ) && matches!(message.wParam as u16, VK_CAPITAL | VK_NUMLOCK | VK_SCROLL)
            {
                // Keep OS toggle/LED updates; remove only our own messages before
                // WebView2 can send them back to the BMC as physical input.
                message.message = WM_NULL;
                message.wParam = 0;
                message.lParam = 0;
            }
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }
    pub fn install() -> Result<(), String> {
        let hook = unsafe {
            SetWindowsHookExW(
                WH_GETMESSAGE,
                Some(filter),
                std::ptr::null_mut(),
                GetCurrentThreadId(),
            )
        };
        if hook.is_null() {
            Err("无法安装本机锁定键消息过滤器".into())
        } else {
            HOOK.store(hook, Ordering::Release);
            Ok(())
        }
    }
    pub fn uninstall() {
        let hook = HOOK.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !hook.is_null() {
            unsafe {
                UnhookWindowsHookEx(hook);
            }
        }
    }
    pub fn read() -> Result<Observation, String> {
        let mut bits = 0;
        for (bit, key) in [(1, VK_NUMLOCK), (2, VK_CAPITAL), (4, VK_SCROLL)] {
            if unsafe { GetKeyState(key as i32) } & 1 != 0 {
                bits |= bit;
            }
        }
        Ok(Observation {
            bits,
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
        let mut inputs = Vec::new();
        for (bit, key) in [(1, VK_NUMLOCK), (2, VK_CAPITAL), (4, VK_SCROLL)] {
            if (current.bits ^ bits) & mask & bit == 0 {
                continue;
            }
            for flags in [0, KEYEVENTF_KEYUP] {
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
                unsafe {
                    SendInput(1, &inputs[sent], std::mem::size_of::<INPUT>() as i32);
                }
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
    pub fn install() -> Result<(), String> {
        Ok(())
    }
    pub fn uninstall() {}
}

pub use platform::{install, read, uninstall, write};
