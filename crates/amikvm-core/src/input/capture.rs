//! Session-scoped ownership of the system webview's native pointer lock.
use serde::Serialize;
use uuid::Uuid;

#[derive(Default, Clone, Serialize)]
pub struct State {
    pub token: Option<Uuid>,
    pub active: bool,
    pub message: Option<String>,
    #[serde(skip)]
    last: Option<(f64, f64, u32, u32)>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Ignored,
    Locked,
    Released,
}
impl State {
    pub fn requested(&self) -> bool {
        self.token.is_some()
    }
    pub fn request(&mut self) {
        self.token = Some(Uuid::new_v4());
        self.active = false;
        self.message = None;
        self.last = None;
    }
    pub fn release(&mut self) -> bool {
        let changed = self.requested() || self.active;
        self.token = None;
        self.active = false;
        self.last = None;
        if changed {
            self.message = Some("鼠标捕获已释放。".into());
        }
        changed
    }
    pub fn event(&mut self, token: Uuid, locked: bool, failed: bool) -> Change {
        if self.token != Some(token) {
            return Change::Ignored;
        }
        if locked && !failed {
            self.active = true;
            self.message = None;
            Change::Locked
        } else {
            self.release();
            self.message = Some(if failed {
                "系统 WebView 未能捕获鼠标，请重新开启后点击画面。".into()
            } else {
                "鼠标捕获已释放。".into()
            });
            Change::Released
        }
    }
    pub fn allows(&self, token: Option<Uuid>) -> bool {
        match token {
            Some(token) => self.active && self.token == Some(token),
            None => !self.requested() && !self.active,
        }
    }
    /// Ordinary viewport deltas are computed in Rust; locked movement is raw
    /// webview event data and is independent of the stationary cursor position.
    pub fn movement(
        &mut self,
        x: f64,
        y: f64,
        width: u32,
        height: u32,
        entered: bool,
    ) -> (f64, f64) {
        let previous = if entered { None } else { self.last };
        self.last = Some((x, y, width, height));
        previous
            .filter(|(_, _, w, h)| (*w, *h) == (width, height))
            .map_or((0., 0.), |(px, py, _, _)| (x - px, y - py))
    }
}
