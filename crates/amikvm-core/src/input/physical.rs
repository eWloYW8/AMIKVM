//! Logical input honors remapping and the selected remote character layout.
//! Bind each source until release so layout changes cannot leave remote keys held.
use super::{
    Keyboard as Report,
    characters::{self, Stroke},
    layout::Layout,
    routing::{Host, Modifiers},
    usage,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Client {
    Windows,
    Linux,
    Other,
}
impl Client {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

#[derive(Clone, Copy)]
pub struct Key<'a> {
    pub code: &'a str,
    pub key: &'a str,
    pub location: u8,
    pub pressed: bool,
    pub modifiers: Option<Modifiers>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Code(String),
    // Browsers cannot distinguish two unidentified keys with the same logical
    // name and location. Do not invent a hardware identity absent from the event.
    Unidentified { key: String, location: u8 },
}
impl Source {
    fn new(code: &str, key: &str, location: u8) -> Self {
        if code.is_empty() || code == "Unidentified" {
            Self::Unidentified {
                key: key.into(),
                location,
            }
        } else {
            Self::Code(code.into())
        }
    }
}
struct Binding {
    code: String,
    alt_graph: bool,
    glyph: Option<Stroke>,
    adjustment: Adjustment,
    order: u64,
}
#[derive(Default)]
struct Adjustment {
    shift: Option<bool>,
    alt_gr: Option<bool>,
}

/// DOM locations are 1/2 for left/right. AltGraph denotes right Alt even when
/// its physical source is CapsLock, a virtual key, or a modifier on another side.
pub fn resolved_code<'a>(code: &'a str, key: &'a str, location: u8) -> &'a str {
    resolve(code, key, location, Client::current())
}

fn resolve<'a>(code: &'a str, key: &'a str, location: u8, client: Client) -> &'a str {
    let right = location == 2 || (location != 1 && code.ends_with("Right"));
    match key {
        "AltGraph" => "AltRight",
        "Control" => {
            if right {
                "ControlRight"
            } else {
                "ControlLeft"
            }
        }
        "Shift" => {
            if right {
                "ShiftRight"
            } else {
                "ShiftLeft"
            }
        }
        "Alt" => {
            if right {
                "AltRight"
            } else {
                "AltLeft"
            }
        }
        "Meta" => {
            if right {
                "MetaRight"
            } else {
                "MetaLeft"
            }
        }
        // X11's Cancel/SunStop is VK_STOP in the original AWT pipeline. WebKit
        // names its physical code BrowserStop, but it remains a keyboard Stop.
        "Cancel" if client == Client::Linux => "Stop",
        "Redo" => "Again",
        "CapsLock" | "NumLock" | "ScrollLock" | "Backspace" | "Tab" | "Enter" | "Escape"
        | "Insert" | "Delete" | "Home" | "End" | "PageUp" | "PageDown" | "ArrowLeft"
        | "ArrowRight" | "ArrowUp" | "ArrowDown" | "PrintScreen" | "Pause" | "ContextMenu"
        | "Help" | "Stop" | "Again" | "Undo" | "Cut" | "Copy" | "Paste" | "Find" | "Cancel"
        | "Clear" | "Separator" => {
            if location == 3 && code.starts_with("Numpad") {
                code
            } else {
                key
            }
        }
        _ if key.starts_with('F') && usage(key).is_some() => key,
        _ => code,
    }
}

#[derive(Default)]
pub struct Keyboard {
    held: BTreeMap<Source, Binding>,
    pending_ctrl: Option<Source>,
    suppressed_ctrl: BTreeSet<Source>,
    layout: Option<Layout>,
    caps: bool,
    order: u64,
}
impl Keyboard {
    pub fn configure(&mut self, layout: Option<Layout>, caps: bool) {
        self.layout = layout.filter(|layout| layout.physical());
        self.caps = caps;
    }
    fn character(&self, code: &str, key: &str, modifiers: Option<Modifiers>) -> Option<Stroke> {
        let bits = self.base_report()[0];
        characters::stroke(
            self.layout?,
            code,
            key,
            modifiers.map_or(bits & 0x22 != 0, |m| m.shift),
            self.caps,
            modifiers.is_some_and(|m| m.alt_graph) || self.alt_graph(),
        )
    }
    fn target<'a>(
        &self,
        code: &'a str,
        key: &'a str,
        location: u8,
        client: Client,
        glyph: Option<Stroke>,
    ) -> &'a str {
        if self.layout.is_some() && location == 3 {
            return characters::numpad(code, key, glyph);
        }
        if let Some(glyph) = glyph {
            return glyph.code;
        }
        if key == " " {
            return "Space";
        }
        resolve(code, key, location, client)
    }
    pub fn key(&mut self, event: Key<'_>, client: Client, host: Host) -> Vec<[u8; 8]> {
        let Key {
            code,
            key,
            location,
            pressed,
            modifiers,
        } = event;
        let source = Source::new(code, key, location);
        let mut glyph = self.character(code, key, modifiers);
        let target = self
            .held
            .get(&source)
            .map_or_else(
                || self.target(code, key, location, client, glyph),
                |binding| binding.code.as_str(),
            )
            .to_owned();
        // Keypad digits/operators keep their numeric location and actual modifiers.
        if glyph.is_some_and(|glyph| glyph.code != target) {
            glyph = None;
        }
        let bits = self.base_report()[0];
        let shift = modifiers.map_or(bits & 0x22 != 0, |m| m.shift);
        let graph = modifiers.is_some_and(|m| m.alt_graph) || self.alt_graph();
        let adjustment = glyph.map_or_else(Adjustment::default, |glyph| Adjustment {
            shift: (glyph.shift != shift || (glyph.shift && bits & 0x22 == 0))
                .then_some(glyph.shift),
            alt_gr: (glyph.alt_gr != graph || (glyph.alt_gr && bits & 0x40 == 0))
                .then_some(glyph.alt_gr),
        });
        if usage(&target).is_none() {
            return vec![];
        }
        let alt_graph =
            key == "AltGraph" || (target == "AltRight" && modifiers.is_some_and(|m| m.alt_graph));
        if pressed && self.held.contains_key(&source) {
            // A new real Ctrl while AltGr is held supersedes the synthetic one.
            if self.alt_graph() {
                self.suppressed_ctrl.remove(&source);
            }
            return if self.pending_ctrl.as_ref() == Some(&source) {
                vec![]
            } else {
                vec![self.report()]
            };
        }
        let mut reports = Vec::with_capacity(2);
        if self.pending_ctrl.is_some() {
            if pressed && target == "AltRight" && alt_graph {
                self.suppressed_ctrl
                    .insert(self.pending_ctrl.take().unwrap());
            } else if let Some(report) = self.flush_pending() {
                reports.push(report);
            }
        }
        if pressed {
            let defer = client == Client::Windows
                && host == Host::Linux
                && target == "ControlLeft"
                && self.report()[0] & 1 == 0
                && !self.alt_graph();
            self.order = self.order.wrapping_add(1);
            self.held.insert(
                source.clone(),
                Binding {
                    code: target,
                    alt_graph,
                    glyph,
                    adjustment,
                    order: self.order,
                },
            );
            if defer {
                self.pending_ctrl = Some(source);
                return reports;
            }
        } else {
            let suppressed = self.suppressed_ctrl.remove(&source);
            self.held.remove(&source);
            if suppressed {
                return reports;
            }
        }
        reports.push(self.report());
        reports
    }
    pub fn code_for<'a>(&'a self, code: &'a str, key: &'a str, location: u8) -> &'a str {
        self.code_for_client(code, key, location, Client::current())
    }
    fn code_for_client<'a>(
        &'a self,
        code: &'a str,
        key: &'a str,
        location: u8,
        client: Client,
    ) -> &'a str {
        self.held
            .get(&Source::new(code, key, location))
            .map_or_else(
                || self.target(code, key, location, client, self.character(code, key, None)),
                |binding| binding.code.as_str(),
            )
    }
    /// Commit a genuine Ctrl before a pointer click, wheel operation or chord.
    pub fn flush_pending(&mut self) -> Option<[u8; 8]> {
        self.pending_ctrl.take()?;
        Some(self.report())
    }
    pub fn release_key(&mut self, code: &str, key: &str, location: u8) {
        let source = Source::new(code, key, location);
        if self.pending_ctrl.as_ref() == Some(&source) {
            self.pending_ctrl = None;
        }
        self.suppressed_ctrl.remove(&source);
        self.held.remove(&source);
    }
    pub fn alt_graph(&self) -> bool {
        self.held.values().any(|binding| binding.alt_graph)
    }
    pub fn report(&self) -> [u8; 8] {
        let mut report = self.base_report();
        if let Some(binding) = self
            .held
            .values()
            .filter(|b| b.glyph.is_some())
            .max_by_key(|b| b.order)
        {
            if let Some(shift) = binding.adjustment.shift {
                if shift {
                    if report[0] & 0x22 == 0 {
                        report[0] |= 2;
                    }
                } else {
                    report[0] &= !0x22;
                }
            }
            if let Some(alt_gr) = binding.adjustment.alt_gr {
                if alt_gr {
                    report[0] |= 0x40;
                } else {
                    report[0] &= !0x40;
                }
            }
        }
        report
    }
    fn base_report(&self) -> [u8; 8] {
        let mut report = Report::default();
        for (source, binding) in &self.held {
            if self.pending_ctrl.as_ref() != Some(source) && !self.suppressed_ctrl.contains(source)
            {
                report.key(&binding.code, true);
            }
        }
        report.report()
    }
    pub fn idle(&self) -> bool {
        self.held.is_empty()
    }
    pub fn clear(&mut self) {
        self.held.clear();
        self.pending_ctrl = None;
        self.suppressed_ctrl.clear();
        self.order = 0;
    }
}
