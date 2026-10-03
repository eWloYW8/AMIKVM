//! Convert a logical character to a position in the selected remote layout.
//! A source remains bound to this stroke until release, even if layouts change.
use super::layout::{Layout, positions};

#[derive(Clone, Copy)]
pub struct Stroke {
    pub code: &'static str,
    pub shift: bool,
    pub alt_gr: bool,
}

pub fn stroke(
    layout: Layout,
    code: &str,
    key: &str,
    shift: bool,
    caps: bool,
    alt_gr: bool,
) -> Option<Stroke> {
    let mut chars = key.chars();
    let character = chars.next()?;
    if chars.next().is_some() || character.is_control() || character.is_whitespace() {
        return None;
    }
    let positions = positions(layout);
    let matches = |code, shift, alt_gr| {
        if alt_gr && matches!(layout, Layout::Us | Layout::Jp) {
            return false;
        }
        layout
            .caption(code, shift, false, alt_gr)
            .map(|caption| {
                if !caps || alt_gr {
                    return caption;
                }
                // Caps affects alphabetic glyphs, not the original soft-keyboard
                // table's shifted number-row labels. Preserve the selected
                // physical layer and invert letter case independently.
                match caption.as_str() {
                    "i" if matches!(layout, Layout::TrF | Layout::TrQ) => "İ".into(),
                    "İ" if matches!(layout, Layout::TrF | Layout::TrQ) => "i".into(),
                    "ß" => "ẞ".into(),
                    _ if caption.chars().any(char::is_lowercase) => caption.to_uppercase(),
                    _ => caption.to_lowercase(),
                }
            })
            .is_some_and(|caption| caption == key)
    };
    // Preserve a matching physical position, including duplicate character keys.
    if let Some(code) = positions.iter().find(|position| **position == code) {
        if matches(code, shift, alt_gr) {
            return Some(Stroke {
                code,
                shift,
                alt_gr,
            });
        }
    }
    // Prefer the actual glyph layer; other layers support remapping and an
    // explicitly selected layout that differs from the local input source.
    for (shift, alt_gr) in [
        (shift, alt_gr),
        (true, false),
        (false, false),
        (false, true),
        (true, true),
    ] {
        if let Some(code) = positions.iter().find(|code| matches(code, shift, alt_gr)) {
            return Some(Stroke {
                code,
                shift,
                alt_gr,
            });
        }
    }
    None
}

/// AWT's cross-map keypad branch uses the main comma/period only when the
/// character conversion resolves to those positions. Other keypad values keep
/// their numeric location, independently of the physical key's current label.
pub fn numpad<'a>(code: &'a str, key: &str, character: Option<Stroke>) -> &'a str {
    if let Some(stroke) = character.filter(|s| matches!(s.code, "Comma" | "Period")) {
        return stroke.code;
    }
    match key {
        "0" | "Insert" => "Numpad0",
        "1" | "End" => "Numpad1",
        "2" | "ArrowDown" => "Numpad2",
        "3" | "PageDown" => "Numpad3",
        "4" | "ArrowLeft" => "Numpad4",
        "5" => "Numpad5",
        "6" | "ArrowRight" => "Numpad6",
        "7" | "Home" => "Numpad7",
        "8" | "ArrowUp" => "Numpad8",
        "9" | "PageUp" => "Numpad9",
        "/" => "NumpadDivide",
        "*" => "NumpadMultiply",
        "-" => "NumpadSubtract",
        "+" => "NumpadAdd",
        "=" => "NumpadEqual",
        "Enter" => "NumpadEnter",
        "NumLock" => "NumLock",
        "NumpadDecimal" => "NumpadDecimal",
        "Delete" => "Delete",
        "Clear" => "Clear",
        "Separator" => "Separator",
        _ => code,
    }
}
