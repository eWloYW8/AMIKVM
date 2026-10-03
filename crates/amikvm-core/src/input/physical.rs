//! Windows synthesizes left Ctrl for AltGr. Linux hosts need only right Alt.
//! Keep this policy separate from software keys, macros and Unicode input.
use super::{
    Keyboard as Report,
    routing::{Host, Modifiers},
    usage,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Client {
    Windows,
    Other,
}
impl Client {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

#[derive(Default)]
pub struct Keyboard {
    report: Report,
    pending_ctrl: bool,
    suppressed_ctrl: bool,
    alt_graph: bool,
}
impl Keyboard {
    /// A deferred Ctrl can yield two ordered reports when it becomes a chord
    /// or a standalone tap. Never infer AltGr from a generic Ctrl+Alt chord.
    pub fn key(
        &mut self,
        code: &str,
        pressed: bool,
        key: &str,
        modifiers: Option<Modifiers>,
        client: Client,
        host: Host,
    ) -> Vec<[u8; 8]> {
        if usage(code).is_none() {
            return vec![];
        }
        let alt_graph = key == "AltGraph" || modifiers.is_some_and(|m| m.alt_graph);
        if client == Client::Windows && host == Host::Linux && code == "ControlLeft" {
            if pressed && self.report.report()[0] & 1 == 0 && !self.alt_graph {
                self.pending_ctrl = true;
                return vec![];
            }
            if !pressed && self.suppressed_ctrl {
                self.suppressed_ctrl = false;
                self.pending_ctrl = false;
                self.report.key(code, false);
                return vec![];
            }
            // A new Ctrl press while AltGr is held is a real additional key.
            if pressed {
                self.suppressed_ctrl = false;
            }
        }
        let mut reports = Vec::with_capacity(2);
        if self.pending_ctrl {
            if code == "AltRight" && pressed && alt_graph {
                self.pending_ctrl = false;
                self.suppressed_ctrl = true;
            } else if let Some(report) = self.flush_pending() {
                reports.push(report);
            }
        }
        if code == "AltRight" {
            self.alt_graph = pressed && alt_graph;
        }
        if let Some(report) = self.report.key(code, pressed) {
            reports.push(report);
        }
        reports
    }

    /// Commit a genuine Ctrl before a pointer click or wheel operation.
    pub fn flush_pending(&mut self) -> Option<[u8; 8]> {
        if !self.pending_ctrl {
            return None;
        }
        self.pending_ctrl = false;
        self.report.key("ControlLeft", true)
    }
    pub fn release_key(&mut self, code: &str) {
        if code == "ControlLeft" {
            self.pending_ctrl = false;
            self.suppressed_ctrl = false;
        }
        if code == "AltRight" {
            self.alt_graph = false;
        }
        self.report.key(code, false);
    }
    pub fn alt_graph(&self) -> bool {
        self.alt_graph
    }
    pub fn report(&self) -> [u8; 8] {
        self.report.report()
    }
    pub fn idle(&self) -> bool {
        !self.pending_ctrl && !self.suppressed_ctrl && self.report() == [0; 8]
    }
    pub fn clear(&mut self) {
        self.report.clear();
        self.pending_ctrl = false;
        self.suppressed_ctrl = false;
        self.alt_graph = false;
    }
}
