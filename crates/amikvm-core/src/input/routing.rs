//! JViewer's full-keyboard mode disables local menu shortcuts, not OS grabs.
use super::TextMode;
use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub full_keyboard: bool,
    pub easy_paste: bool,
    pub text_mode: TextMode,
}
#[derive(Deserialize)]
#[serde(tag = "setting", content = "value", rename_all = "snake_case")]
pub enum Setting {
    FullKeyboard(bool),
    EasyPaste(bool),
    TextMode(TextMode),
}
impl Options {
    pub fn apply(&mut self, setting: Setting) {
        match setting {
            Setting::FullKeyboard(value) => self.full_keyboard = value,
            Setting::EasyPaste(value) => self.easy_paste = value,
            Setting::TextMode(value) => self.text_mode = value,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    About,
    Log,
    Paste,
    Calibrate,
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
        "F1" if ctrl && !options.full_keyboard => Some(Action::About),
        "KeyT" if alt && !options.full_keyboard && mouse_mode == Some(1) => Some(Action::Calibrate),
        _ => None,
    }
}
