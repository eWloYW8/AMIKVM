//! JViewer user macros are simultaneous key chords (20 entries, six keys each).
//! Persistence and USB conversion are independent of the webview and JVM formats.
mod remote;
use super::{Keyboard, usage};
use crate::{Error, Result};
pub use remote::{RemoteMacro, RemoteMacroEdit, RemoteMacros, remote_catalogue};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, io::Write, path::PathBuf};
use uuid::Uuid;

pub const MAX_MACROS: usize = 20;
pub const MAX_KEYS: usize = 6;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Macro {
    pub id: Uuid,
    pub name: String,
    pub codes: Vec<String>,
}

impl Macro {
    pub fn report(&self) -> Result<[u8; 8]> {
        validate_codes(&self.codes)?;
        let mut keyboard = Keyboard::default();
        for code in &self.codes {
            keyboard.key(code, true);
        }
        Ok(keyboard.report())
    }
    pub fn description(&self) -> String {
        self.codes
            .iter()
            .map(|c| key_label(c))
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

fn validate_codes(codes: &[String]) -> Result<()> {
    if codes.is_empty() || codes.len() > MAX_KEYS {
        return Err(Error::Invalid("每个组合键须包含 1–6 个按键".into()));
    }
    let mut keys = HashSet::new();
    for code in codes {
        let key = usage(code).ok_or_else(|| Error::Invalid(format!("无法识别按键：{code}")))?;
        if !keys.insert(key) {
            return Err(Error::Invalid("组合键中不能重复同一个按键".into()));
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Data {
    version: u32,
    macros: Vec<Macro>,
}

pub struct Store {
    path: PathBuf,
    macros: Vec<Macro>,
}

impl Store {
    pub fn open(path: PathBuf) -> Result<Self> {
        let macros = match fs::read(&path) {
            Ok(bytes) => {
                let data: Data = serde_json::from_slice(&bytes)?;
                if data.version != 1 || data.macros.len() > MAX_MACROS {
                    return Err(Error::Invalid("用户组合键文件的版本或数量无效".into()));
                }
                let mut ids = HashSet::new();
                let mut names = HashSet::new();
                for m in &data.macros {
                    validate_name(&m.name)?;
                    validate_codes(&m.codes)?;
                    if !ids.insert(m.id) || !names.insert(m.name.as_str()) {
                        return Err(Error::Invalid("用户组合键文件中存在重复条目".into()));
                    }
                }
                data.macros
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        Ok(Self { path, macros })
    }
    pub fn list(&self) -> Vec<Macro> {
        self.macros.clone()
    }
    pub fn get(&self, id: Uuid) -> Result<Macro> {
        self.macros
            .iter()
            .find(|m| m.id == id)
            .cloned()
            .ok_or_else(|| Error::Invalid("用户组合键不存在".into()))
    }
    pub fn save(&mut self, id: Option<Uuid>, name: String, codes: Vec<String>) -> Result<Macro> {
        let name = name.trim().to_owned();
        validate_name(&name)?;
        validate_codes(&codes)?;
        if let Some(id) = id {
            self.get(id)?;
        } else if self.macros.len() >= MAX_MACROS {
            return Err(Error::Invalid("最多保存 20 个用户组合键".into()));
        }
        if self
            .macros
            .iter()
            .any(|m| Some(m.id) != id && m.name == name)
        {
            return Err(Error::Invalid("此组合键名称已经存在".into()));
        }
        let item = Macro {
            id: id.unwrap_or_else(Uuid::new_v4),
            name,
            codes,
        };
        let mut next = self.macros.clone();
        if let Some(existing) = next.iter_mut().find(|m| m.id == item.id) {
            *existing = item.clone();
        } else {
            next.push(item.clone());
        }
        self.persist(next)?;
        Ok(item)
    }
    pub fn remove(&mut self, id: Uuid) -> Result<()> {
        self.get(id)?;
        self.persist(self.macros.iter().filter(|m| m.id != id).cloned().collect())
    }
    fn persist(&mut self, macros: Vec<Macro>) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| Error::Invalid("组合键保存路径无效".into()))?;
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&serde_json::to_vec_pretty(&Data {
            version: 1,
            macros: macros.clone(),
        })?)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path).map_err(|e| e.error)?;
        self.macros = macros;
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(Error::Invalid(
            "组合键名称须为 1–80 个字符，且不能包含控制字符".into(),
        ));
    }
    Ok(())
}

pub fn key_label(code: &str) -> String {
    if let Some(letter) = code.strip_prefix("Key") {
        return letter.to_owned();
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        return digit.to_owned();
    }
    if let Some(digit) = code.strip_prefix("Numpad").filter(|s| s.len() == 1) {
        return format!("数字键盘 {digit}");
    }
    let label = match code {
        "ControlLeft" => "左 Ctrl",
        "ControlRight" => "右 Ctrl",
        "ShiftLeft" => "左 Shift",
        "ShiftRight" => "右 Shift",
        "AltLeft" => "左 Alt",
        "AltRight" => "右 Alt",
        "MetaLeft" => "左 Win / Command",
        "MetaRight" => "右 Win / Command",
        "Enter" => "Enter",
        "Escape" => "Esc",
        "Backspace" => "Backspace",
        "Tab" => "Tab",
        "Space" => "空格",
        "Minus" => "−",
        "Equal" => "=",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        "Backslash" => "\\",
        "IntlHash" => "#（国际键）",
        "Semicolon" => ";",
        "Quote" => "'",
        "Backquote" => "`",
        "Comma" => ",",
        "Period" => ".",
        "Slash" => "/",
        "CapsLock" => "Caps Lock",
        "PrintScreen" => "Print Screen",
        "ScrollLock" => "Scroll Lock",
        "Pause" => "Pause",
        "Insert" => "Insert",
        "Home" => "Home",
        "PageUp" => "Page Up",
        "Delete" => "Delete",
        "End" => "End",
        "PageDown" => "Page Down",
        "ArrowRight" => "→",
        "ArrowLeft" => "←",
        "ArrowDown" => "↓",
        "ArrowUp" => "↑",
        "NumLock" => "Num Lock",
        "NumpadDivide" => "数字键盘 /",
        "NumpadMultiply" => "数字键盘 *",
        "NumpadSubtract" => "数字键盘 −",
        "NumpadAdd" => "数字键盘 +",
        "NumpadEnter" => "数字键盘 Enter",
        "NumpadDecimal" => "数字键盘 .",
        "NumpadEqual" => "数字键盘 =",
        "IntlBackslash" => "国际键 \\ / |",
        "ContextMenu" => "菜单键",
        "Power" => "电源键",
        "IntlRo" => "日语 ろ",
        "KanaMode" => "かな",
        "IntlYen" => "日语 ¥",
        "Convert" => "変換",
        "NonConvert" => "無変換",
        "Lang1" => "语言键 1",
        "Lang2" => "语言键 2",
        "AudioVolumeMute" => "静音",
        "AudioVolumeUp" => "音量增加",
        "AudioVolumeDown" => "音量减少",
        _ => code,
    };
    label.to_owned()
}

/// Complete physical key catalogue used by the Rust-generated macro editor.
pub fn catalogue() -> Vec<(String, String)> {
    let mut codes: Vec<String> = [
        "ControlLeft",
        "ShiftLeft",
        "AltLeft",
        "MetaLeft",
        "ControlRight",
        "ShiftRight",
        "AltRight",
        "MetaRight",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    codes.extend(('A'..='Z').map(|c| format!("Key{c}")));
    codes.extend(('0'..='9').map(|c| format!("Digit{c}")));
    codes.extend((1..=24).map(|n| format!("F{n}")));
    codes.extend(
        [
            "Enter",
            "Escape",
            "Backspace",
            "Tab",
            "Space",
            "Minus",
            "Equal",
            "BracketLeft",
            "BracketRight",
            "Backslash",
            "IntlHash",
            "Semicolon",
            "Quote",
            "Backquote",
            "Comma",
            "Period",
            "Slash",
            "CapsLock",
            "PrintScreen",
            "ScrollLock",
            "Pause",
            "Insert",
            "Home",
            "PageUp",
            "Delete",
            "End",
            "PageDown",
            "ArrowRight",
            "ArrowLeft",
            "ArrowDown",
            "ArrowUp",
            "NumLock",
            "NumpadDivide",
            "NumpadMultiply",
            "NumpadSubtract",
            "NumpadAdd",
            "NumpadEnter",
            "NumpadDecimal",
            "NumpadEqual",
            "IntlBackslash",
            "ContextMenu",
            "Power",
            "Help",
            "Stop",
            "Again",
            "Undo",
            "Cut",
            "Copy",
            "Paste",
            "Find",
            "Cancel",
            "Clear",
            "Separator",
            "IntlRo",
            "KanaMode",
            "IntlYen",
            "Convert",
            "NonConvert",
            "Lang1",
            "Lang2",
            "AudioVolumeMute",
            "AudioVolumeUp",
            "AudioVolumeDown",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    codes.extend(('0'..='9').map(|c| format!("Numpad{c}")));
    codes
        .into_iter()
        .map(|c| {
            let label = key_label(&c);
            (c, label)
        })
        .collect()
}
