//! JViewer's full-keyboard mode disables local menu shortcuts, not OS grabs.
use super::TextMode;
use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub full_keyboard: bool,
    pub easy_paste: bool,
    pub host: Host,
    pub text_mode: TextMode,
}
#[derive(Deserialize)]
#[serde(tag = "setting", content = "value", rename_all = "snake_case")]
pub enum Setting {
    FullKeyboard(bool),
    EasyPaste(bool),
    Host(Host),
    TextMode(TextMode),
}
impl Options {
    pub fn apply(&mut self, setting: Setting) {
        match setting {
            Setting::FullKeyboard(value) => self.full_keyboard = value,
            Setting::EasyPaste(value) => self.easy_paste = value,
            Setting::Host(value) => self.host = value,
            Setting::TextMode(value) => self.text_mode = value,
        }
    }
}

/// The remote host, independent of the local platform and Unicode text mode.
#[derive(Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Host {
    #[default]
    Windows,
    Linux,
}
impl Host {
    pub fn id(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    About,
    Log,
    Paste,
    Calibrate,
    Cursor,
    Pause,
    Resume,
    Refresh,
    Capture,
    Fullscreen,
    HostDisplay,
}

/// Raw webview modifier flags remain available after a local action releases
/// remote keys, and while the remote keyboard is paused or view-only.
#[derive(Clone, Copy, Deserialize)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
    #[serde(default, rename = "altGraph")]
    pub alt_graph: bool,
    #[serde(default, rename = "capsLock")]
    pub caps_lock: bool,
}
impl Modifiers {
    pub fn bits(self) -> u8 {
        u8::from(self.ctrl)
            | (u8::from(self.shift) << 1)
            | (u8::from(self.alt) << 2)
            | (u8::from(self.meta) << 3)
    }
}
pub fn local(
    code: &str,
    modifiers: u8,
    options: Options,
    mouse_mode: Option<u8>,
) -> Option<Action> {
    let ctrl = modifiers & 0x11 != 0;
    let shift = modifiers & 0x22 != 0;
    let alt = modifiers & 0x44 != 0;
    match code {
        "KeyV" if ctrl && options.easy_paste => Some(Action::Paste),
        "KeyL" if ctrl && shift => Some(Action::Log),
        "KeyC" if alt && !ctrl && (!options.full_keyboard || mouse_mode == Some(3)) => {
            Some(Action::Cursor)
        }
        "F1" if ctrl && !options.full_keyboard => Some(Action::About),
        "KeyT" if alt && !ctrl && !options.full_keyboard && mouse_mode == Some(1) => {
            Some(Action::Calibrate)
        }
        "KeyP" if alt && !ctrl && !options.full_keyboard => Some(Action::Pause),
        "KeyR" if alt && !ctrl && !options.full_keyboard => Some(Action::Resume),
        "KeyE" if alt && !ctrl && !options.full_keyboard => Some(Action::Refresh),
        "KeyS" if alt && !ctrl && !options.full_keyboard => Some(Action::Capture),
        "KeyF" if alt && !ctrl && !options.full_keyboard => Some(Action::Fullscreen),
        "KeyN" if alt && !ctrl && !options.full_keyboard => Some(Action::HostDisplay),
        _ => None,
    }
}
