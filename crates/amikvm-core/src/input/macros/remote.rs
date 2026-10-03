//! IVTP 40/41 user macros, from AddMacro.er/eu and USBKeyProcessorEnglish.
//! The BMC stores 20 slots of six big-endian (key code, location) pairs.
//! Numeric wire key codes are translated here; no JVM is used at runtime.
use super::{MAX_KEYS, MAX_MACROS, Macro, catalogue, key_label, validate_codes};
use crate::{
    Error, Result,
    input::{
        layout::{self, Layout},
        usage,
    },
    protocol,
};
use serde::Serialize;

pub const CONFIG_BYTES: usize = MAX_MACROS * MAX_KEYS * 8;
const SLOT_BYTES: usize = MAX_KEYS * 8;

#[derive(Clone, Debug, Serialize)]
pub struct RemoteMacro {
    pub slot: u8,
    pub name: String,
    pub codes: Vec<String>,
    pub supported: bool,
    #[serde(skip)]
    keys: Vec<(u32, u32)>,
}
impl RemoteMacro {
    pub fn report(&self) -> Result<[u8; 8]> {
        self.report_for_layout(None)
    }
    pub fn report_for_layout(&self, layout: Option<Layout>) -> Result<[u8; 8]> {
        if !self.supported {
            return Err(Error::Invalid("服务器组合键包含未支持的按键".into()));
        }
        // These are logical VK/location pairs, unlike local physical macros.
        // JViewer executes them through the currently selected key processor;
        // char=0 uses its cross-map, without synthesizing text or modifiers.
        let choices = catalogue();
        let mut codes = vec![];
        for &(key, location) in &self.keys {
            let mapped = if location == 1 {
                layout_key(layout, key)
            } else {
                key
            };
            let code = WIRE_KEYS
                .iter()
                .find(|&&(k, l, _)| k == mapped && l == location)
                // Keep a standard HID key available when a legacy layout's
                // mapped target is absent from its own usage table (JP F4).
                .or_else(|| {
                    WIRE_KEYS
                        .iter()
                        .find(|&&(k, l, _)| k == key && l == location)
                })
                .and_then(|&(_, _, hid)| choices.iter().find(|(code, _)| usage(code) == Some(hid)))
                .ok_or_else(|| Error::Invalid("服务器组合键包含未支持的按键".into()))?;
            if !codes.contains(&code.0) {
                codes.push(code.0.clone());
            }
        }
        Macro {
            id: uuid::Uuid::nil(),
            name: self.name.clone(),
            codes,
        }
        .report()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RemoteMacros {
    pub entries: Vec<RemoteMacro>,
    #[serde(skip)]
    bytes: Vec<u8>,
}
#[derive(Clone)]
pub struct RemoteMacroEdit {
    pub slot: u8,
    pub expected: Vec<u8>,
    pub codes: Option<Vec<String>>,
}
impl RemoteMacros {
    pub fn parse(body: &[u8]) -> Result<Self> {
        if body.len() < CONFIG_BYTES {
            return Err(Error::Protocol("服务器组合键配置不足 960 字节".into()));
        }
        let choices = catalogue();
        let mut entries = vec![];
        for (slot, bytes) in body[..CONFIG_BYTES].chunks_exact(SLOT_BYTES).enumerate() {
            let mut codes = vec![];
            let mut labels = vec![];
            let mut keys = vec![];
            let mut supported = true;
            for pair in bytes.chunks_exact(8) {
                let key = u32::from_be_bytes(pair[..4].try_into().unwrap());
                let location = u32::from_be_bytes(pair[4..].try_into().unwrap());
                if key == 0 || location == 0 {
                    continue;
                }
                keys.push((key, location));
                let code = display_usage(key, location)
                    .and_then(|hid| choices.iter().find(|(code, _)| usage(code) == Some(hid)));
                if let Some((code, _)) = code {
                    labels.push(
                        if WIRE_KEYS.iter().any(|&(k, l, _)| k == key && l == location) {
                            key_label(code)
                        } else {
                            format!("键码 {key}/{location}")
                        },
                    );
                    if !codes.contains(code) {
                        codes.push(code.clone());
                    }
                } else {
                    supported = false;
                    labels.push(format!("键码 {key}/{location}"));
                }
            }
            if !labels.is_empty() {
                entries.push(RemoteMacro {
                    slot: slot as u8,
                    name: labels.join(" + "),
                    codes,
                    supported,
                    keys,
                });
            }
        }
        Ok(Self {
            entries,
            bytes: body[..CONFIG_BYTES].to_vec(),
        })
    }
    pub fn get(&self, slot: u8) -> Result<&RemoteMacro> {
        self.entries
            .iter()
            .find(|m| m.slot == slot)
            .ok_or_else(|| Error::Invalid("服务器组合键不存在".into()))
    }
    pub fn vacant(&self) -> Option<u8> {
        (0..MAX_MACROS as u8).find(|slot| self.entries.iter().all(|m| m.slot != *slot))
    }
    pub fn slot_bytes(&self, slot: u8) -> Result<Vec<u8>> {
        let start = usize::from(slot) * SLOT_BYTES;
        self.bytes
            .get(start..start + SLOT_BYTES)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| Error::Invalid("服务器组合键位置无效".into()))
    }
    pub fn change(&self, edit: &RemoteMacroEdit) -> Result<Self> {
        if self.slot_bytes(edit.slot)? != edit.expected {
            return Err(Error::Invalid(
                "服务器组合键已变化，请重新打开编辑器".into(),
            ));
        }
        let mut slot = [0; SLOT_BYTES];
        if let Some(codes) = &edit.codes {
            validate_codes(codes)?;
            for (pair, code) in slot.chunks_exact_mut(8).zip(codes) {
                // Keep an existing alias when its choice is unchanged, so an
                // edit to another key cannot silently replace its logical VK.
                if let Some(existing) = self.bytes[usize::from(edit.slot) * SLOT_BYTES..]
                    [..SLOT_BYTES]
                    .chunks_exact(8)
                    .find(|pair| {
                        let key = u32::from_be_bytes(pair[..4].try_into().unwrap());
                        let location = u32::from_be_bytes(pair[4..].try_into().unwrap());
                        display_usage(key, location) == usage(code)
                    })
                {
                    pair.copy_from_slice(existing);
                    continue;
                }
                let (key, location) = wire_key(code).ok_or_else(|| {
                    Error::Invalid(format!("服务器组合键不支持按键：{}", key_label(code)))
                })?;
                pair[..4].copy_from_slice(&key.to_be_bytes());
                pair[4..].copy_from_slice(&location.to_be_bytes());
            }
        }
        let mut bytes = self.bytes.clone();
        let start = usize::from(edit.slot) * SLOT_BYTES;
        bytes[start..start + SLOT_BYTES].copy_from_slice(&slot);
        Self::parse(&bytes)
    }
    pub fn packet(&self) -> Result<Vec<u8>> {
        protocol::packet(41, 0, &self.bytes)
    }
}

fn display_usage(key: u32, location: u32) -> Option<u8> {
    let lookup = |key| {
        WIRE_KEYS
            .iter()
            .find(|&&(k, l, _)| k == key && l == location)
            .map(|&(_, _, hid)| hid)
    };
    lookup(key).or_else(|| {
        if location != 1 {
            return None;
        }
        layout::ALL
            .into_iter()
            .find_map(|layout| lookup(layout_key(Some(layout), key)))
    })
}

// AutoKeyboardLayout.aj and the char=0 AN maps in c/c, c/h, c/i, c/f.
fn layout_key(layout: Option<Layout>, key: u32) -> u32 {
    match (layout, key) {
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 65) => 81,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 90) => 87,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 87) => 90,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 81) => 65,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 77) => 59,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 44) => 77,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 59) => 44,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 513) => 46,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 522) => 45,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 130) => 91,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 515) => 93,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 151) => 92,
        (Some(Layout::Fr), 517) => 47,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 150) if cfg!(target_os = "linux") => 49,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 519) if cfg!(target_os = "linux") => 53,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 523) if cfg!(target_os = "linux") => 56,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 222) if cfg!(target_os = "linux") => 52,
        (Some(Layout::Fr | Layout::FrBe | Layout::NlBe), 45) if cfg!(target_os = "linux") => 54,
        (Some(Layout::FrBe), 61) => 47,
        (Some(Layout::FrBe), 45) => 61,
        (Some(Layout::FrBe), 135) => 91,
        (Some(Layout::De | Layout::DeCh), 89) => 90,
        (Some(Layout::De | Layout::DeCh), 90) => 89,
        (Some(Layout::De), 45) => 47,
        (Some(Layout::De), 47) => 45,
        (Some(Layout::De), 129) => 61,
        (Some(Layout::De), 521) => 93,
        (Some(Layout::De), 520) => 92,
        (Some(Layout::De), 130) => 192,
        (Some(Layout::DeCh), 128) => 61,
        (Some(Layout::DeCh), 135) => 93,
        (Some(Layout::DeCh), 515) => 92,
        (Some(Layout::DeCh), 130) => 61,
        (Some(Layout::TrF), 70) => 81,
        (Some(Layout::TrF), 71) => 87,
        (Some(Layout::TrF), 73) => 82,
        (Some(Layout::TrF), 79) => 84,
        (Some(Layout::TrF), 68) => 89,
        (Some(Layout::TrF), 82) => 85,
        (Some(Layout::TrF), 78) => 73,
        (Some(Layout::TrF), 72) => 79,
        (Some(Layout::TrF), 81) => 93,
        (Some(Layout::TrF), 87) => 91,
        (Some(Layout::TrF), 85) => 65,
        (Some(Layout::TrF), 69) => 68,
        (Some(Layout::TrF), 65) => 70,
        (Some(Layout::TrF), 84) => 72,
        (Some(Layout::TrF), 75) => 74,
        (Some(Layout::TrF), 77) => 75,
        (Some(Layout::TrF), 89) => 59,
        (Some(Layout::TrF), 74) => 90,
        (Some(Layout::TrF), 86) => 67,
        (Some(Layout::TrF), 67) => 86,
        (Some(Layout::TrF), 90) => 78,
        (Some(Layout::TrF), 83) => 77,
        (Some(Layout::TrF), 66) => 44,
        (Some(Layout::Jp | Layout::JpHira | Layout::JpKata), 91) => 93,
        (Some(Layout::Jp | Layout::JpHira | Layout::JpKata), 92) => 220,
        (Some(Layout::Jp | Layout::JpHira | Layout::JpKata), 93) => 92,
        (Some(Layout::Jp | Layout::JpHira | Layout::JpKata), 115) => 135,
        (Some(Layout::Jp | Layout::JpHira | Layout::JpKata), 514) => 61,
        (Some(Layout::Jp | Layout::JpHira | Layout::JpKata), 512) => 91,
        (Some(Layout::Jp | Layout::JpHira | Layout::JpKata), 513) => 222,
        (Some(Layout::Jp | Layout::JpHira | Layout::JpKata), 243 | 244) => 192,
        (Some(Layout::Jp | Layout::JpHira | Layout::JpKata), 263) => 65481,
        _ => key,
    }
}
fn wire_key(code: &str) -> Option<(u32, u32)> {
    let hid = usage(code)?;
    let location = if code.starts_with("Numpad") || code == "NumLock" {
        4
    } else if code.ends_with("Left") && hid >= 224 {
        2
    } else if code.ends_with("Right") && hid >= 224 {
        3
    } else {
        1
    };
    WIRE_KEYS
        .iter()
        .find(|&&(_, l, value)| value == hid && l == location)
        .or_else(|| WIRE_KEYS.iter().find(|&&(_, _, value)| value == hid))
        .map(|&(key, location, _)| (key, location))
}
pub fn remote_catalogue() -> Vec<(String, String)> {
    catalogue()
        .into_iter()
        .filter(|(code, _)| wire_key(code).is_some())
        .collect()
}

// Extracted from c/d.java AU/AV/AW/AX, with referenced constants resolved.
const WIRE_KEYS: &[(u32, u32, u8)] = &[
    (65, 1, 4),
    (66, 1, 5),
    (67, 1, 6),
    (68, 1, 7),
    (69, 1, 8),
    (70, 1, 9),
    (71, 1, 10),
    (72, 1, 11),
    (73, 1, 12),
    (74, 1, 13),
    (75, 1, 14),
    (76, 1, 15),
    (77, 1, 16),
    (78, 1, 17),
    (79, 1, 18),
    (80, 1, 19),
    (81, 1, 20),
    (82, 1, 21),
    (83, 1, 22),
    (84, 1, 23),
    (85, 1, 24),
    (86, 1, 25),
    (87, 1, 26),
    (88, 1, 27),
    (89, 1, 28),
    (90, 1, 29),
    (49, 1, 30),
    (50, 1, 31),
    (51, 1, 32),
    (52, 1, 33),
    (53, 1, 34),
    (54, 1, 35),
    (55, 1, 36),
    (56, 1, 37),
    (57, 1, 38),
    (48, 1, 39),
    (10, 1, 40),
    (27, 1, 41),
    (8, 1, 42),
    (9, 1, 43),
    (32, 1, 44),
    (45, 1, 45),
    (61, 1, 46),
    (91, 1, 47),
    (93, 1, 48),
    (92, 1, 49),
    (59, 1, 51),
    (222, 1, 52),
    (192, 1, 53),
    (44, 1, 54),
    (46, 1, 55),
    (47, 1, 56),
    (20, 1, 57),
    (112, 1, 58),
    (113, 1, 59),
    (114, 1, 60),
    (115, 1, 61),
    (116, 1, 62),
    (117, 1, 63),
    (118, 1, 64),
    (119, 1, 65),
    (120, 1, 66),
    (121, 1, 67),
    (122, 1, 68),
    (123, 1, 69),
    (154, 1, 70),
    (145, 1, 71),
    (19, 1, 72),
    (155, 1, 73),
    (36, 1, 74),
    (33, 1, 75),
    (127, 1, 76),
    (35, 1, 77),
    (34, 1, 78),
    (39, 1, 79),
    (37, 1, 80),
    (40, 1, 81),
    (38, 1, 82),
    (109, 1, 86),
    (144, 4, 83),
    (111, 4, 84),
    (106, 4, 85),
    (109, 4, 86),
    (107, 4, 87),
    (10, 4, 88),
    (97, 4, 89),
    (35, 4, 89),
    (98, 4, 90),
    (40, 4, 90),
    (225, 4, 90),
    (99, 4, 91),
    (34, 4, 91),
    (100, 4, 92),
    (37, 4, 92),
    (226, 4, 92),
    (101, 4, 93),
    (65368, 4, 93),
    (102, 4, 94),
    (39, 4, 94),
    (227, 4, 94),
    (103, 4, 95),
    (36, 4, 95),
    (104, 4, 96),
    (38, 4, 96),
    (224, 4, 96),
    (105, 4, 97),
    (33, 4, 97),
    (96, 4, 98),
    (155, 4, 98),
    (110, 4, 99),
    (127, 4, 76),
    (153, 1, 100),
    (61, 4, 103),
    (525, 1, 101),
    (61440, 1, 104),
    (61441, 1, 105),
    (61442, 1, 106),
    (61443, 1, 107),
    (61444, 1, 108),
    (61445, 1, 109),
    (61446, 1, 110),
    (61447, 1, 111),
    (61448, 1, 112),
    (61449, 1, 113),
    (61450, 1, 114),
    (61451, 1, 115),
    (156, 1, 117),
    (65480, 1, 120),
    (65481, 1, 121),
    (65483, 1, 122),
    (65489, 1, 123),
    (65485, 1, 124),
    (65487, 1, 125),
    (65488, 1, 126),
    (3, 1, 155),
    (12, 1, 156),
    (108, 1, 159),
    (226, 1, 100),
    (999, 1, 135),
    (240, 1, 136),
    (220, 1, 137),
    (28, 1, 138),
    (29, 1, 139),
    (243, 1, 53),
    (244, 1, 53),
    (242, 1, 57),
    (263, 1, 53),
    (17, 2, 224),
    (16, 2, 225),
    (18, 2, 226),
    (524, 2, 227),
    (17, 3, 228),
    (16, 3, 229),
    (18, 3, 230),
    (524, 3, 231),
];
