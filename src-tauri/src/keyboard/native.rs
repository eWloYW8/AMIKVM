//! Recover legacy Sun keysyms absent from WebKit's DOM key-name table.
//! The webview still owns event delivery, focus, and its raw physical code.
#[cfg(target_os = "linux")]
use amikvm_core::input::routing::Modifiers;

#[cfg(target_os = "linux")]
pub async fn logical_key(
    app: &tauri::AppHandle,
    code: &str,
    modifiers: Option<Modifiers>,
) -> Option<&'static str> {
    use gtk::gdk;
    use tauri::Manager;
    let hardware = hardware_code(code)?;
    let app = app.clone();
    let handle = app.clone();
    let (reply, result) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let key = (|| {
            let display = gdk::Display::default()?;
            let keymap = gdk::Keymap::for_display(&display)?;
            let group = handle
                .state::<crate::commands::AppState>()
                .host_keyboard
                .group
                .lock()
                .ok()?
                .unwrap_or(0);
            let mut state = gdk::ModifierType::empty();
            if let Some(modifiers) = modifiers {
                if modifiers.shift {
                    state |= gdk::ModifierType::SHIFT_MASK;
                }
                if modifiers.ctrl {
                    state |= gdk::ModifierType::CONTROL_MASK;
                }
                if modifiers.alt {
                    state |= gdk::ModifierType::MOD1_MASK;
                }
                if modifiers.alt_graph {
                    state |= gdk::ModifierType::MOD5_MASK;
                }
            }
            let (keyval, _, _, _) =
                keymap.translate_keyboard_state(hardware, state, i32::from(group))?;
            match keyval {
                0x1005_ff72 => Some("Copy"),
                0x1005_ff74 => Some("Paste"),
                0x1005_ff75 => Some("Cut"),
                _ => None,
            }
        })();
        let _ = reply.send(key);
    })
    .ok()?;
    result.await.ok().flatten()
}

/// GTK's X11 and Wayland hardware keycodes use the evdev/XKB physical positions.
/// Only look up a recognized DOM position; never guess an unidentified source.
#[cfg(target_os = "linux")]
fn hardware_code(code: &str) -> Option<u32> {
    use amikvm_core::input::layout::{Layout, positions};
    const WRITING: [u32; 48] = [
        49, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33,
        34, 35, 51, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 94, 52, 53, 54, 55, 56, 57, 58, 59,
        60, 61,
    ];
    if let Some(index) = positions(Layout::Us)
        .iter()
        .position(|position| *position == code)
    {
        return Some(WRITING[index]);
    }
    if let Some(function) = code.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        return match function {
            1..=10 => Some(66 + function),
            11..=12 => Some(84 + function),
            13..=24 => Some(178 + function),
            _ => None,
        };
    }
    Some(match code {
        "Escape" => 9,
        "Backspace" => 22,
        "Tab" => 23,
        "Enter" => 36,
        "ControlLeft" => 37,
        "ShiftLeft" => 50,
        "ShiftRight" => 62,
        "NumpadMultiply" => 63,
        "AltLeft" => 64,
        "Space" => 65,
        "CapsLock" => 66,
        "NumLock" => 77,
        "ScrollLock" => 78,
        "Numpad7" => 79,
        "Numpad8" => 80,
        "Numpad9" => 81,
        "NumpadSubtract" => 82,
        "Numpad4" => 83,
        "Numpad5" => 84,
        "Numpad6" => 85,
        "NumpadAdd" => 86,
        "Numpad1" => 87,
        "Numpad2" => 88,
        "Numpad3" => 89,
        "Numpad0" => 90,
        "NumpadDecimal" => 91,
        "IntlRo" => 97,
        "KanaMode" => 101,
        "Convert" => 100,
        "NonConvert" => 102,
        "NumpadEnter" => 104,
        "ControlRight" => 105,
        "NumpadDivide" => 106,
        "PrintScreen" => 107,
        "AltRight" => 108,
        "Home" => 110,
        "ArrowUp" => 111,
        "PageUp" => 112,
        "ArrowLeft" => 113,
        "ArrowRight" => 114,
        "End" => 115,
        "ArrowDown" => 116,
        "PageDown" => 117,
        "Insert" => 118,
        "Delete" => 119,
        "AudioVolumeMute" => 121,
        "AudioVolumeDown" => 122,
        "AudioVolumeUp" => 123,
        "Pause" => 127,
        "NumpadComma" => 129,
        "NumpadEqual" => 125,
        "Lang1" => 130,
        "Lang2" => 131,
        "IntlYen" => 132,
        "MetaLeft" | "OSLeft" => 133,
        "MetaRight" | "OSRight" => 134,
        "ContextMenu" => 135,
        "Stop" | "BrowserStop" => 136,
        "Again" => 137,
        "Undo" => 139,
        "Copy" => 141,
        "Paste" => 143,
        "Find" => 144,
        "Cut" => 145,
        "Help" => 146,
        _ => return None,
    })
}
