//! Renderer geometry is raw data; Rust maps source positions and filters warps.
use super::mouse::Point;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}
impl Rect {
    fn valid(self) -> bool {
        [self.left, self.top, self.width, self.height]
            .iter()
            .all(|v| v.is_finite() && v.abs() < 1_000_000.)
            && self.width > 0.
            && self.height > 0.
    }
    fn intersect(self, other: Self) -> Option<Self> {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = (self.left + self.width).min(other.left + other.width);
        let bottom = (self.top + self.height).min(other.top + other.height);
        (right > left && bottom > top).then_some(Self {
            left,
            top,
            width: right - left,
            height: bottom - top,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Viewport {
    pub bounds: Rect,
    pub clip: Rect,
    pub scale: f64,
}
impl Viewport {
    pub fn validate(self) -> Result<Self> {
        if !self.bounds.valid()
            || !self.clip.valid()
            || !self.scale.is_finite()
            || self.scale <= 0.
            || self.scale > 16.
            || self.bounds.intersect(self.clip).is_none()
        {
            return Err(Error::Invalid("Invalid console geometry".into()));
        }
        Ok(self)
    }
    pub fn visible(self) -> Rect {
        self.bounds
            .intersect(self.clip)
            .expect("validated viewport")
    }
    pub fn area(self, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
        if width == 0 || height == 0 {
            return None;
        }
        let clip = self.visible();
        let min_x =
            ((clip.left - self.bounds.left) * f64::from(width) / self.bounds.width).ceil() as u32;
        let min_y =
            ((clip.top - self.bounds.top) * f64::from(height) / self.bounds.height).ceil() as u32;
        let max_x = (((clip.left + clip.width - self.bounds.left) * f64::from(width)
            / self.bounds.width)
            .floor() as u32)
            .min(width);
        let max_y = (((clip.top + clip.height - self.bounds.top) * f64::from(height)
            / self.bounds.height)
            .floor() as u32)
            .min(height);
        (max_x > min_x && max_y > min_y).then_some((min_x, min_y, max_x, max_y))
    }
    pub fn target(self, p: Point) -> Option<(f64, f64)> {
        if p.width == 0 || p.height == 0 || !p.x.is_finite() || !p.y.is_finite() {
            return None;
        }
        let clip = self.visible();
        let inset_x = (1. / self.scale).min(clip.width / 2.);
        let inset_y = (1. / self.scale).min(clip.height / 2.);
        Some((
            (self.bounds.left + p.x * self.bounds.width / f64::from(p.width))
                .clamp(clip.left + inset_x, clip.left + clip.width - inset_x),
            (self.bounds.top + p.y * self.bounds.height / f64::from(p.height))
                .clamp(clip.top + inset_y, clip.top + clip.height - inset_y),
        ))
    }
}

#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub supported: bool,
    pub message: Option<String>,
    pub focus: Option<Uuid>,
    #[serde(skip)]
    pub viewport: Option<Viewport>,
    #[serde(skip)]
    pub revision: u64,
    #[serde(skip)]
    suppressed: Option<(f64, f64, u8)>,
}
impl State {
    pub fn viewport(&mut self, viewport: Option<Viewport>) -> Result<bool> {
        let viewport = viewport.map(Viewport::validate).transpose()?;
        if self.viewport == viewport {
            return Ok(false);
        }
        self.viewport = viewport;
        self.cancel();
        Ok(true)
    }
    pub fn cancel(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.suppressed = None;
        self.focus = None;
    }
    pub fn warped(&mut self, x: f64, y: f64, buttons: u8) {
        self.suppressed = Some((x, y, buttons));
        self.message = None;
    }
    pub fn synthetic(&mut self, x: f64, y: f64, buttons: u8, wheel: f64) -> bool {
        let tolerance = self.viewport.map_or(0.5, |v| 0.5 / v.scale);
        if self.suppressed.is_some_and(|(px, py, previous)| {
            (x - px).abs() <= tolerance
                && (y - py).abs() <= tolerance
                && buttons == previous
                && wheel == 0.
        }) {
            return true;
        }
        self.suppressed = None;
        false
    }
    pub fn stale(&self, x: f64, y: f64, current: Option<(f64, f64)>) -> Option<(f64, f64, u8)> {
        let (px, py, buttons) = self.suppressed?;
        let (cx, cy) = current?;
        let tolerance = self.viewport.map_or(0.5, |v| 0.5 / v.scale);
        ((cx - px).abs() <= tolerance
            && (cy - py).abs() <= tolerance
            && ((x - px).abs() > tolerance || (y - py).abs() > tolerance))
            .then_some((px, py, buttons))
    }
}
