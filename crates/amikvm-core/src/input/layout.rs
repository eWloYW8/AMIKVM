//! Physical positions use DOM code -> USB HID, independent of localized characters.
//! The original viewer's complete virtual keyboard character layers are embedded.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Layout {
    #[serde(rename = "US")]
    Us,
    #[serde(rename = "GB")]
    Gb,
    #[serde(rename = "ES")]
    Es,
    #[serde(rename = "FR")]
    Fr,
    #[serde(rename = "DE")]
    De,
    #[serde(rename = "IT")]
    It,
    #[serde(rename = "DA")]
    Da,
    #[serde(rename = "FI")]
    Fi,
    #[serde(rename = "DE-CH")]
    DeCh,
    #[serde(rename = "NO")]
    No,
    #[serde(rename = "PT")]
    Pt,
    #[serde(rename = "SV")]
    Sv,
    #[serde(rename = "HE")]
    He,
    #[serde(rename = "FR-BE")]
    FrBe,
    #[serde(rename = "NL-BE")]
    NlBe,
    #[serde(rename = "RU")]
    Ru,
    #[serde(rename = "JP")]
    Jp,
    #[serde(rename = "TR_F")]
    TrF,
    #[serde(rename = "TR_Q")]
    TrQ,
    #[serde(rename = "JP_HIRA")]
    JpHira,
    #[serde(rename = "JP_KATA")]
    JpKata,
    #[serde(rename = "NL-NL")]
    NlNl,
}
pub const ALL: [Layout; 22] = [
    Layout::Us,
    Layout::Gb,
    Layout::Es,
    Layout::Fr,
    Layout::De,
    Layout::It,
    Layout::Da,
    Layout::Fi,
    Layout::DeCh,
    Layout::No,
    Layout::Pt,
    Layout::Sv,
    Layout::He,
    Layout::FrBe,
    Layout::NlBe,
    Layout::Ru,
    Layout::Jp,
    Layout::TrF,
    Layout::TrQ,
    Layout::JpHira,
    Layout::JpKata,
    Layout::NlNl,
];

impl Layout {
    pub fn id(self) -> &'static str {
        [
            "US", "GB", "ES", "FR", "DE", "IT", "DA", "FI", "DE-CH", "NO", "PT", "SV", "HE",
            "FR-BE", "NL-BE", "RU", "JP", "TR_F", "TR_Q", "JP_HIRA", "JP_KATA", "NL-NL",
        ][self.index()]
    }
    pub fn label(self) -> &'static str {
        [
            "English · US",
            "English · UK",
            "Español",
            "Français",
            "Deutsch",
            "Italiano",
            "Dansk",
            "Suomi",
            "Deutsch · Schweiz",
            "Norsk",
            "Português",
            "Svenska",
            "עברית",
            "Français · Belgique",
            "Nederlands · België",
            "Русский",
            "日本語 · ローマ字",
            "Türkçe · F",
            "Türkçe · Q",
            "日本語 · ひらがな",
            "日本語 · カタカナ",
            "Nederlands",
        ][self.index()]
    }
    fn index(self) -> usize {
        self as usize
    }
    pub fn parse(id: &str) -> Result<Self> {
        ALL.into_iter()
            .find(|l| l.id() == id)
            .ok_or_else(|| Error::Invalid("不支持的键盘布局".into()))
    }
    pub fn physical(self) -> bool {
        !matches!(self, Self::He | Self::Ru | Self::JpHira | Self::JpKata)
    }
    pub fn japanese(self) -> bool {
        matches!(self, Self::Jp | Self::JpHira | Self::JpKata)
    }
    pub fn caption(self, code: &str, shift: bool, caps: bool, alt_gr: bool) -> Option<String> {
        if self.japanese() && code == "Backquote" {
            return Some("半/全".into());
        }
        let position = positions(self).iter().position(|c| *c == code)?;
        let data = tables();
        let index = self.index();
        if alt_gr
            && !matches!(
                self,
                Self::Us | Self::Ru | Self::Jp | Self::JpHira | Self::JpKata
            )
        {
            let (text, slots) = if shift {
                (&data.shift_alt[index], &data.shift_alt_slots[index])
            } else {
                (&data.alt[index], &data.alt_slots[index])
            };
            let slot = slots
                .iter()
                .position(|slot| slot.parse::<usize>().ok() == Some(position + 16));
            return Some(
                slot.and_then(|p| text.chars().nth(p))
                    .unwrap_or(' ')
                    .to_string(),
            );
        }
        let text = match (shift, caps) {
            (false, false) => &data.normal[index],
            (true, false) => &data.shift[index],
            (false, true) => &data.caps[index],
            (true, true) => &data.shift_caps[index],
        };
        // The French shifted layer omits the first, non-printing key.
        if self == Self::Fr && shift {
            return Some(
                if position == 0 {
                    ' '
                } else {
                    text.chars().nth(position - 1).unwrap_or(' ')
                }
                .to_string(),
            );
        }
        // The original JP Shift+Caps table inserts a stray 'c' after '"'.
        // Skip it so all later labels retain their physical positions.
        let position = if self == Self::Jp && shift && caps && position >= 2 {
            position + 1
        } else {
            position
        };
        text.chars().nth(position).map(|c| c.to_string())
    }
    pub fn decimal(self) -> &'static str {
        if matches!(
            self,
            Self::Da
                | Self::Fi
                | Self::De
                | Self::No
                | Self::Sv
                | Self::TrF
                | Self::TrQ
                | Self::NlNl
        ) {
            ","
        } else {
            "."
        }
    }
}

pub fn positions(layout: Layout) -> &'static [&'static str; 48] {
    if layout.japanese() {
        &[
            "Digit1",
            "Digit2",
            "Digit3",
            "Digit4",
            "Digit5",
            "Digit6",
            "Digit7",
            "Digit8",
            "Digit9",
            "Digit0",
            "Minus",
            "Equal",
            "KeyQ",
            "KeyW",
            "KeyE",
            "KeyR",
            "KeyT",
            "KeyY",
            "KeyU",
            "KeyI",
            "KeyO",
            "KeyP",
            "BracketLeft",
            "BracketRight",
            "Backslash",
            "KeyA",
            "KeyS",
            "KeyD",
            "KeyF",
            "KeyG",
            "KeyH",
            "KeyJ",
            "KeyK",
            "KeyL",
            "Semicolon",
            "Quote",
            "IntlYen",
            "KeyZ",
            "KeyX",
            "KeyC",
            "KeyV",
            "KeyB",
            "KeyN",
            "KeyM",
            "Comma",
            "Period",
            "Slash",
            "IntlRo",
        ]
    } else {
        &[
            "Backquote",
            "Digit1",
            "Digit2",
            "Digit3",
            "Digit4",
            "Digit5",
            "Digit6",
            "Digit7",
            "Digit8",
            "Digit9",
            "Digit0",
            "Minus",
            "Equal",
            "KeyQ",
            "KeyW",
            "KeyE",
            "KeyR",
            "KeyT",
            "KeyY",
            "KeyU",
            "KeyI",
            "KeyO",
            "KeyP",
            "BracketLeft",
            "BracketRight",
            "Backslash",
            "KeyA",
            "KeyS",
            "KeyD",
            "KeyF",
            "KeyG",
            "KeyH",
            "KeyJ",
            "KeyK",
            "KeyL",
            "Semicolon",
            "Quote",
            "IntlBackslash",
            "KeyZ",
            "KeyX",
            "KeyC",
            "KeyV",
            "KeyB",
            "KeyN",
            "KeyM",
            "Comma",
            "Period",
            "Slash",
        ]
    }
}

#[derive(Deserialize)]
struct Tables {
    #[serde(rename = "As")]
    normal: Vec<String>,
    #[serde(rename = "At")]
    shift: Vec<String>,
    #[serde(rename = "Au")]
    caps: Vec<String>,
    #[serde(rename = "Av")]
    shift_caps: Vec<String>,
    #[serde(rename = "Aw")]
    alt: Vec<String>,
    #[serde(rename = "Ax")]
    alt_slots: Vec<Vec<String>>,
    #[serde(rename = "Ay")]
    shift_alt: Vec<String>,
    #[serde(rename = "Az")]
    shift_alt_slots: Vec<Vec<String>>,
}
fn tables() -> &'static Tables {
    static DATA: OnceLock<Tables> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(include_str!("layout-data.json")).expect("bundled keyboard layers")
    })
}

/// Match native normal/Shift glyphs by physical position, including JIS keys.
/// Identical supported tables stay ambiguous rather than choosing arbitrarily.
pub fn from_glyphs(observed: &[(&str, bool, char)]) -> Vec<Layout> {
    let mut glyphs = std::collections::BTreeMap::new();
    for &(code, shift, glyph) in observed {
        if glyph.is_control() {
            continue;
        }
        if glyphs
            .insert((code, shift), glyph)
            .is_some_and(|old| old != glyph)
        {
            return vec![];
        }
    }
    ALL.into_iter()
        .filter(|layout| layout.physical())
        .filter(|layout| {
            let mut total = 0;
            let mut matching = 0;
            for (&(code, shift), &glyph) in &glyphs {
                if !positions(*layout).contains(&code) {
                    continue;
                }
                let Some(caption) = layout.caption(code, shift, false, false) else {
                    continue;
                };
                let mut characters = caption.chars();
                let expected = characters.next();
                if expected.is_none() || characters.next().is_some() {
                    continue;
                }
                total += 1;
                matching += usize::from(expected == Some(glyph));
            }
            total >= 60 && matching * 100 >= total * 95
        })
        .collect()
}

pub fn from_windows(id: &str) -> Option<Layout> {
    Some(match id.to_ascii_uppercase().as_str() {
        "00000409" => Layout::Us,
        "00000809" => Layout::Gb,
        "0000040A" | "0001040A" => Layout::Es,
        "0000040C" => Layout::Fr,
        "00000407" => Layout::De,
        "00000410" => Layout::It,
        "00000406" => Layout::Da,
        "0000040B" => Layout::Fi,
        "00000807" => Layout::DeCh,
        "00000414" => Layout::No,
        "00000816" => Layout::Pt,
        "0000041D" => Layout::Sv,
        "0000080C" => Layout::FrBe,
        "00000813" => Layout::NlBe,
        "00000413" => Layout::NlNl,
        "00000411" => Layout::Jp,
        "0001041F" => Layout::TrF,
        "0000041F" => Layout::TrQ,
        _ => return None,
    })
}
pub fn from_xkb(name: &str, variant: &str) -> Option<Layout> {
    Some(match (name, variant) {
        ("us", "" | "basic") => Layout::Us,
        ("gb", "" | "basic") => Layout::Gb,
        ("es", "" | "basic" | "nodeadkeys") => Layout::Es,
        ("fr", "" | "basic" | "nodeadkeys") => Layout::Fr,
        ("de", "" | "basic" | "nodeadkeys") => Layout::De,
        ("it", "" | "basic" | "nodeadkeys") => Layout::It,
        ("dk", "" | "basic" | "nodeadkeys") => Layout::Da,
        ("fi", "" | "basic" | "classic" | "nodeadkeys") => Layout::Fi,
        ("ch", "" | "basic" | "de" | "de_nodeadkeys") => Layout::DeCh,
        ("no", "" | "basic" | "nodeadkeys") => Layout::No,
        ("pt", "" | "basic" | "nodeadkeys") => Layout::Pt,
        ("se", "" | "basic" | "nodeadkeys") => Layout::Sv,
        ("be", "" | "basic" | "nodeadkeys") => Layout::FrBe,
        ("nl", "" | "basic") => Layout::NlNl,
        ("jp", "" | "basic" | "106" | "kana") => Layout::Jp,
        ("tr", "f") => Layout::TrF,
        ("tr", "" | "basic") => Layout::TrQ,
        _ => return None,
    })
}
pub fn from_macos(id: &str) -> Option<Layout> {
    let name = id.strip_prefix("com.apple.keylayout.")?;
    Some(match name {
        "US" | "ABC" => Layout::Us,
        "British" | "British-PC" => Layout::Gb,
        "Spanish" | "Spanish-ISO" => Layout::Es,
        "French" | "French-PC" => Layout::Fr,
        "German" => Layout::De,
        "Italian" | "Italian-Pro" => Layout::It,
        "Danish" => Layout::Da,
        "Finnish" => Layout::Fi,
        "SwissGerman" => Layout::DeCh,
        "Norwegian" => Layout::No,
        "Portuguese" => Layout::Pt,
        "Swedish" | "Swedish-Pro" => Layout::Sv,
        "Belgian" => Layout::FrBe,
        "Dutch" => Layout::NlNl,
        "Japanese" => Layout::Jp,
        "Turkish" => Layout::TrF,
        "Turkish-QWERTY" | "Turkish-QWERTY-PC" | "Turkish-Standard" => Layout::TrQ,
        _ => return None,
    })
}
