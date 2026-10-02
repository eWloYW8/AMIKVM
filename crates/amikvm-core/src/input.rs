//! Browser events are data only. HID usage mapping and text input live in Rust.
use crate::{Error, Result, protocol};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub mod capture;
pub mod encryption;
pub mod layout;
pub mod macros;
pub mod mouse;
pub mod routing;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Focus {
        focused: bool,
    },
    Key {
        code: String,
        pressed: bool,
    },
    Pointer {
        buttons: u8,
        x: f64,
        y: f64,
        width: u32,
        height: u32,
        dx: f64,
        dy: f64,
        wheel: f64,
        #[serde(default)]
        capture: Option<uuid::Uuid>,
        #[serde(default)]
        entered: bool,
    },
    PointerCapture {
        token: uuid::Uuid,
        locked: bool,
        #[serde(default)]
        failed: bool,
    },
    Release,
    SoftKey {
        code: String,
        pressed: bool,
    },
    ReleaseAll,
}

#[derive(Default)]
pub struct Keyboard {
    modifiers: u8,
    keys: BTreeSet<u8>,
}

impl Keyboard {
    pub fn key(&mut self, code: &str, pressed: bool) -> Option<[u8; 8]> {
        let usage = usage(code)?;
        if (0xe0..=0xe7).contains(&usage) {
            let bit = 1 << (usage - 0xe0);
            if pressed {
                self.modifiers |= bit;
            } else {
                self.modifiers &= !bit;
            }
        } else if pressed {
            self.keys.insert(usage);
        } else {
            self.keys.remove(&usage);
        }
        Some(self.report())
    }
    pub fn report(&self) -> [u8; 8] {
        let mut report = [0; 8];
        report[0] = self.modifiers;
        if self.keys.len() > 6 {
            report[2..].fill(1);
        } else {
            for (i, key) in self.keys.iter().enumerate() {
                report[i + 2] = *key;
            }
        }
        report
    }
    pub fn clear(&mut self) {
        self.modifiers = 0;
        self.keys.clear();
    }
}

/// Physical focus loss must not release the software keyboard's latched modifiers.
#[derive(Default)]
pub struct State {
    physical: Keyboard,
    software: Keyboard,
}

impl State {
    pub fn key(&mut self, code: &str, pressed: bool) -> Option<[u8; 8]> {
        self.physical.key(code, pressed)?;
        Some(self.report())
    }
    pub fn soft_key(&mut self, code: &str, pressed: bool) -> Option<[u8; 8]> {
        self.software.key(code, pressed)?;
        Some(self.report())
    }
    pub fn toggle_modifier(&mut self, code: &str) -> Result<[u8; 8]> {
        let key = usage(code)
            .filter(|k| (0xe0..=0xe7).contains(k))
            .ok_or_else(|| Error::Invalid("只能保持修饰键".into()))?;
        let pressed = self.software.modifiers & (1 << (key - 0xe0)) != 0;
        self.software.key(code, !pressed);
        Ok(self.report())
    }
    pub fn software_keys(&self) -> Vec<u8> {
        let mut keys: Vec<_> = self.software.keys.iter().copied().collect();
        keys.extend((0xe0..=0xe7).filter(|k| self.software.modifiers & (1 << (k - 0xe0)) != 0));
        keys
    }
    pub fn report(&self) -> [u8; 8] {
        merge_reports(self.physical.report(), self.software.report())
    }
    pub fn release_physical(&mut self) -> [u8; 8] {
        self.physical.clear();
        self.report()
    }
    pub fn release_software(&mut self) -> [u8; 8] {
        self.software.clear();
        self.report()
    }
    pub fn clear(&mut self) {
        self.physical.clear();
        self.software.clear();
    }
}

pub fn merge_reports(a: [u8; 8], b: [u8; 8]) -> [u8; 8] {
    let mut out = [0; 8];
    out[0] = a[0] | b[0];
    let keys: BTreeSet<_> = a[2..]
        .iter()
        .chain(&b[2..])
        .copied()
        .filter(|k| *k != 0)
        .collect();
    if keys.len() > 6 || keys.contains(&1) {
        out[2..].fill(1);
    } else {
        for (slot, key) in out[2..].iter_mut().zip(keys) {
            *slot = key;
        }
    }
    out
}

pub fn usage(code: &str) -> Option<u8> {
    if code.len() == 4 && code.starts_with("Key") {
        let b = code.as_bytes()[3];
        if b.is_ascii_uppercase() {
            return Some(b - b'A' + 4);
        }
    }
    if code.len() == 6 && code.starts_with("Digit") {
        return match code.as_bytes()[5] {
            b'1'..=b'9' => Some(code.as_bytes()[5] - b'1' + 0x1e),
            b'0' => Some(0x27),
            _ => None,
        };
    }
    if let Some(n) = code.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
        if (1..=12).contains(&n) {
            return Some(0x3a + n - 1);
        }
        if (13..=24).contains(&n) {
            return Some(0x68 + n - 13);
        }
    }
    Some(match code {
        "Enter" => 0x28,
        "Escape" => 0x29,
        "Backspace" => 0x2a,
        "Tab" => 0x2b,
        "Space" => 0x2c,
        "Minus" => 0x2d,
        "Equal" => 0x2e,
        "BracketLeft" => 0x2f,
        "BracketRight" => 0x30,
        "Backslash" => 0x31,
        "IntlHash" => 0x32,
        "Semicolon" => 0x33,
        "Quote" => 0x34,
        "Backquote" => 0x35,
        "Comma" => 0x36,
        "Period" => 0x37,
        "Slash" => 0x38,
        "CapsLock" => 0x39,
        "PrintScreen" => 0x46,
        "ScrollLock" => 0x47,
        "Pause" => 0x48,
        "Insert" => 0x49,
        "Home" => 0x4a,
        "PageUp" => 0x4b,
        "Delete" => 0x4c,
        "End" => 0x4d,
        "PageDown" => 0x4e,
        "ArrowRight" => 0x4f,
        "ArrowLeft" => 0x50,
        "ArrowDown" => 0x51,
        "ArrowUp" => 0x52,
        "NumLock" => 0x53,
        "NumpadDivide" => 0x54,
        "NumpadMultiply" => 0x55,
        "NumpadSubtract" => 0x56,
        "NumpadAdd" => 0x57,
        "NumpadEnter" => 0x58,
        "Numpad1" => 0x59,
        "Numpad2" => 0x5a,
        "Numpad3" => 0x5b,
        "Numpad4" => 0x5c,
        "Numpad5" => 0x5d,
        "Numpad6" => 0x5e,
        "Numpad7" => 0x5f,
        "Numpad8" => 0x60,
        "Numpad9" => 0x61,
        "Numpad0" => 0x62,
        "NumpadDecimal" => 0x63,
        "IntlBackslash" => 0x64,
        "ContextMenu" => 0x65,
        "Power" => 0x66,
        "NumpadEqual" => 0x67,
        "IntlRo" => 0x87,
        "KanaMode" => 0x88,
        "IntlYen" => 0x89,
        "Convert" => 0x8a,
        "NonConvert" => 0x8b,
        "Lang1" => 0x90,
        "Lang2" => 0x91,
        "ControlLeft" => 0xe0,
        "ShiftLeft" => 0xe1,
        "AltLeft" => 0xe2,
        "MetaLeft" => 0xe3,
        "ControlRight" => 0xe4,
        "ShiftRight" => 0xe5,
        "AltRight" => 0xe6,
        "MetaRight" => 0xe7,
        "AudioVolumeMute" => 0x7f,
        "AudioVolumeUp" => 0x80,
        "AudioVolumeDown" => 0x81,
        _ => return None,
    })
}

fn report(modifiers: u8, keys: &[u8]) -> [u8; 8] {
    let mut r = [0; 8];
    r[0] = modifiers;
    for (slot, key) in r[2..].iter_mut().zip(keys) {
        *slot = *key;
    }
    r
}

pub fn shortcut(name: &str) -> Result<[u8; 8]> {
    Ok(match name {
        "ctrl_alt_del" => report(5, &[0x4c]),
        "alt_f2" => report(4, &[0x3b]),
        "ctrl_h" => report(1, &[0x0b]),
        "win" => report(8, &[]),
        "menu" => report(0, &[0x65]),
        "ctrl_alt_backspace" => report(5, &[0x2a]),
        "alt_tab" => report(4, &[0x2b]),
        "ctrl_escape" => report(1, &[0x29]),
        _ => return Err(Error::Invalid("Unknown shortcut".into())),
    })
}

pub fn mouse(event: &Event, absolute: bool) -> Result<Vec<Vec<u8>>> {
    let Event::Pointer {
        buttons,
        x,
        y,
        width,
        height,
        dx,
        dy,
        wheel,
        ..
    } = event
    else {
        return Err(Error::Invalid("Expected a pointer event".into()));
    };
    if !dx.is_finite() || !dy.is_finite() || !wheel.is_finite() {
        return Err(Error::Invalid("Invalid pointer movement".into()));
    }
    let scroll = if *wheel == 0.0 {
        0
    } else {
        (-wheel.signum()) as i8
    };
    if absolute {
        return Ok(vec![
            protocol::absolute_mouse(*buttons, *x, *y, *width, *height, scroll)?.to_vec(),
        ]);
    }
    let mut remaining_x = dx.clamp(-4096.0, 4096.0).round() as i32;
    let mut remaining_y = dy.clamp(-4096.0, 4096.0).round() as i32;
    let mut reports = vec![];
    loop {
        let x = remaining_x.clamp(-126, 126) as i8;
        let y = remaining_y.clamp(-126, 126) as i8;
        reports.push(vec![
            buttons & 7,
            x as u8,
            y as u8,
            if reports.is_empty() { scroll as u8 } else { 0 },
        ]);
        remaining_x -= x as i32;
        remaining_y -= y as i32;
        if remaining_x == 0 && remaining_y == 0 {
            break;
        }
    }
    Ok(reports)
}

pub fn parse_hex(text: &str) -> Result<Vec<u8>> {
    let bytes = text
        .split_whitespace()
        .map(|word| {
            let word = word
                .strip_prefix("0x")
                .or_else(|| word.strip_prefix("0X"))
                .unwrap_or(word);
            if word.is_empty() || word.len() > 2 {
                return Err(Error::Invalid("每个十六进制字节须为 00–FF".into()));
            }
            u8::from_str_radix(word, 16)
                .map_err(|_| Error::Invalid("请输入十六进制字节，以空格分隔".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    if bytes.len() < 2 || bytes.len() > 1024 {
        return Err(Error::Invalid("IPMI 命令须包含 2–1024 个字节".into()));
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextMode {
    Linux,
    Windows,
    WindowsWord,
    Macos,
    #[default]
    Us,
}
impl TextMode {
    pub fn parse(mode: &str) -> Result<Self> {
        match mode {
            "linux" => Ok(Self::Linux),
            "windows" => Ok(Self::Windows),
            "windows_word" => Ok(Self::WindowsWord),
            "macos" => Ok(Self::Macos),
            "us" => Ok(Self::Us),
            _ => Err(Error::Invalid("Unknown text input mode".into())),
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::WindowsWord => "windows_word",
            Self::Macos => "macos",
            Self::Us => "us",
        }
    }
}

#[derive(Default, Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextPhase {
    #[default]
    Idle,
    Running,
    Complete,
    Cancelled,
    Failed,
}
#[derive(Default, Clone, Serialize)]
pub struct TextStatus {
    pub phase: TextPhase,
    pub sent: usize,
    pub total: usize,
    pub error: Option<String>,
}
impl TextStatus {
    pub fn active(&self) -> bool {
        matches!(self.phase, TextPhase::Running)
    }
}

/// End offsets count complete character sequences, including their key releases.
pub struct TextPlan {
    pub reports: Vec<[u8; 8]>,
    pub ends: Vec<usize>,
}

fn ascii(c: char) -> Option<(u8, u8)> {
    Some(match c {
        'a'..='z' => (0, c as u8 - b'a' + 4),
        'A'..='Z' => (2, c as u8 - b'A' + 4),
        '1'..='9' => (0, c as u8 - b'1' + 0x1e),
        '0' => (0, 0x27),
        '\n' | '\r' => (0, 0x28),
        '\t' => (0, 0x2b),
        ' ' => (0, 0x2c),
        '-' => (0, 0x2d),
        '_' => (2, 0x2d),
        '=' => (0, 0x2e),
        '+' => (2, 0x2e),
        '[' => (0, 0x2f),
        '{' => (2, 0x2f),
        ']' => (0, 0x30),
        '}' => (2, 0x30),
        '\\' => (0, 0x31),
        '|' => (2, 0x31),
        ';' => (0, 0x33),
        ':' => (2, 0x33),
        '\'' => (0, 0x34),
        '"' => (2, 0x34),
        '`' => (0, 0x35),
        '~' => (2, 0x35),
        ',' => (0, 0x36),
        '<' => (2, 0x36),
        '.' => (0, 0x37),
        '>' => (2, 0x37),
        '/' => (0, 0x38),
        '?' => (2, 0x38),
        '!' => (2, 0x1e),
        '@' => (2, 0x1f),
        '#' => (2, 0x20),
        '$' => (2, 0x21),
        '%' => (2, 0x22),
        '^' => (2, 0x23),
        '&' => (2, 0x24),
        '*' => (2, 0x25),
        '(' => (2, 0x26),
        ')' => (2, 0x27),
        _ => return None,
    })
}

fn tap(out: &mut Vec<[u8; 8]>, modifiers: u8, key: u8) {
    out.push(report(modifiers, &[key]));
    out.push(report(modifiers, &[]));
}

/// Validate the complete string before issuing any input, so unsupported text is never truncated.
pub fn text_reports(text: &str, mode: TextMode) -> Result<Vec<[u8; 8]>> {
    Ok(text_plan(text, mode)?.reports)
}
pub fn text_plan(text: &str, mode: TextMode) -> Result<TextPlan> {
    if text.len() > 65536 {
        return Err(Error::Invalid("文本长度不能超过 64 KiB".into()));
    }
    let mut out = vec![[0; 8]];
    let mut ends = vec![];
    let text = text.replace("\r\n", "\n");
    for c in text.chars() {
        if c == '\n' || c == '\r' || c == '\t' {
            let (m, k) = ascii(c).unwrap();
            tap(&mut out, m, k);
            ends.push(out.len());
            continue;
        }
        match mode {
            TextMode::Us => {
                let (m, k) = ascii(c).ok_or_else(|| {
                    Error::Invalid("US 键盘无法直接输入此字符，请选择 Unicode 输入方式".into())
                })?;
                tap(&mut out, m, k);
                out.push([0; 8]);
            }
            TextMode::Linux => {
                tap(&mut out, 3, usage("KeyU").unwrap());
                out.push([0; 8]);
                for hex in format!("{:x}", c as u32).chars() {
                    let (m, k) = ascii(hex).unwrap();
                    tap(&mut out, m, k);
                }
                tap(&mut out, 0, 0x28);
            }
            TextMode::Windows => {
                if c as u32 > 0xffff {
                    return Err(Error::Invalid("Windows 十六进制键盘输入不支持此非 BMP 字符；该字符需要远程应用支持其他输入方式".into()));
                }
                out.push(report(4, &[]));
                tap(&mut out, 4, 0x57);
                for hex in format!("{:x}", c as u32).chars() {
                    let (_, k) = ascii(hex).unwrap();
                    tap(&mut out, 4, k);
                }
                out.push([0; 8]);
            }
            TextMode::WindowsWord => {
                // Word converts the selected Unicode scalar with Alt+X. Select
                // only this code so preceding hex characters cannot join it.
                let code = format!("{:x}", c as u32);
                for digit in code.chars() {
                    let (m, k) = ascii(digit).unwrap();
                    tap(&mut out, m, k);
                }
                for _ in code.chars() {
                    tap(&mut out, 2, 0x50);
                }
                out.push([0; 8]);
                tap(&mut out, 4, usage("KeyX").unwrap());
                out.push([0; 8]);
            }
            TextMode::Macos => {
                out.push(report(4, &[]));
                for unit in c.encode_utf16(&mut [0; 2]).iter() {
                    for hex in format!("{unit:04x}").chars() {
                        let (_, k) = ascii(hex).unwrap();
                        tap(&mut out, 4, k);
                    }
                }
                out.push([0; 8]);
            }
        }
        ends.push(out.len());
    }
    out.push([0; 8]);
    Ok(TextPlan { reports: out, ends })
}
