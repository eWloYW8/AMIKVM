//! Local relative-mouse prediction and JViewer's two-stage, 750 ms calibration.
//! Calibration settings never become a BMC configuration packet.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const INTERVAL: Duration = Duration::from_millis(750);
const CHUNK: i32 = 126;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub threshold: u16,
    /// Hundredths avoid accumulated floating-point errors while adjusting by 0.1.
    pub acceleration: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            threshold: 4,
            acceleration: 200,
        }
    }
}
impl Settings {
    pub fn validate(self) -> Result<Self> {
        if self.threshold == 0 || self.acceleration == 0 {
            return Err(Error::Invalid("阈值和加速倍率必须大于零".into()));
        }
        Ok(self)
    }
    pub fn multiplier(self) -> String {
        format!("{}.{:02}", self.acceleration / 100, self.acceleration % 100)
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    #[default]
    Idle,
    Threshold,
    ThresholdReview,
    Acceleration,
    AccelerationReview,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Command {
    Start,
    Adjust {
        direction: i8,
        #[serde(default)]
        fine: bool,
    },
    Detected,
    Accept,
    Retry,
    Cancel,
    Pause {
        paused: bool,
    },
    Configure {
        settings: Settings,
    },
    Synchronize,
}

#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub settings: Settings,
    pub stage: Stage,
    pub candidate: Option<Settings>,
    pub token: Option<Uuid>,
    pub paused: bool,
    pub message: Option<String>,
    pub reference: Option<Point>,
    #[serde(skip)]
    bounds: (u32, u32),
    #[serde(skip)]
    relative: bool,
    #[serde(skip)]
    eligible: bool,
    #[serde(skip)]
    next: Option<Instant>,
    #[serde(skip)]
    vertical: bool,
    #[serde(skip)]
    remainder: (i64, i64),
    #[serde(skip)]
    alt: u8,
    #[serde(skip)]
    area: Option<(u32, u32, u32, u32)>,
    #[serde(skip)]
    pub needs_sync: bool,
}
impl State {
    pub fn active(&self) -> bool {
        self.stage != Stage::Idle
    }
    pub fn running(&self) -> bool {
        !self.paused && matches!(self.stage, Stage::Threshold | Stage::Acceleration)
    }
    /// Context changes cancel the wizard; confirmed threshold remains saved.
    pub fn context(&mut self, relative: bool, eligible: bool, width: u32, height: u32) -> bool {
        let changed = self.relative != relative || self.bounds != (width, height);
        let old_eligible = self.eligible;
        let cancelled = self.active() && (changed || !eligible);
        if cancelled {
            self.stop();
            self.message =
                Some("画面、鼠标模式或控制权限变化，校准已取消；已确认的参数保留。".into());
        }
        self.relative = relative;
        self.eligible = eligible && (1..=65535).contains(&width) && (1..=65535).contains(&height);
        self.bounds = (width, height);
        if changed || old_eligible != self.eligible {
            self.area = None;
            self.needs_sync = true;
        }
        if !relative || !self.eligible {
            self.reference = None;
            self.remainder = (0, 0);
        } else if changed || self.reference.is_none() {
            self.reset_reference();
        }
        cancelled || changed || old_eligible != self.eligible
    }
    pub fn viewport(&mut self, area: Option<(u32, u32, u32, u32)>) -> bool {
        if self.area == area {
            return false;
        }
        self.area = area;
        self.needs_sync = true;
        self.suspend();
        if self.relative && self.eligible {
            self.reset_reference();
        }
        true
    }
    fn require(&self) -> Result<()> {
        if !self.relative || !self.eligible {
            return Err(Error::Invalid(
                "请在有画面的相对鼠标模式下取得控制权限".into(),
            ));
        }
        Ok(())
    }
    pub fn command(
        &mut self,
        command: Command,
        token: Option<Uuid>,
        now: Instant,
    ) -> Result<Vec<[u8; 4]>> {
        self.require()?;
        if self.active() && token != self.token {
            return Err(Error::Invalid(
                "该校准操作已经结束，请使用当前校准面板".into(),
            ));
        }
        if !self.active() && token.is_some() {
            return Err(Error::Invalid("该校准操作已经结束".into()));
        }
        match command {
            Command::Start => {
                if self.active() {
                    return Err(Error::Invalid("此会话已经在校准".into()));
                }
                self.token = Some(Uuid::new_v4());
                self.stage = Stage::Threshold;
                self.candidate = Some(Settings {
                    threshold: 1,
                    acceleration: 100,
                });
                self.paused = false;
                self.message = None;
                self.reset(now);
                Ok(self.synchronize())
            }
            Command::Adjust { direction, fine } => {
                if !matches!(self.stage, Stage::Threshold | Stage::Acceleration)
                    || !matches!(direction, -1 | 1)
                {
                    return Err(Error::Invalid("当前校准步骤不能调整参数".into()));
                }
                let candidate = self.candidate.as_mut().expect("active candidate");
                if self.stage == Stage::Threshold {
                    candidate.threshold = if direction < 0 {
                        candidate.threshold.saturating_sub(1).max(1)
                    } else {
                        candidate
                            .threshold
                            .checked_add(1)
                            .ok_or_else(|| Error::Invalid("阈值已达到最大值".into()))?
                    };
                } else {
                    let step = if fine { 10 } else { 100 };
                    candidate.acceleration = if direction < 0 {
                        candidate
                            .acceleration
                            .checked_sub(step)
                            .filter(|v| *v > 0)
                            .unwrap_or(100)
                    } else {
                        candidate
                            .acceleration
                            .checked_add(step)
                            .ok_or_else(|| Error::Invalid("倍率已达到最大值".into()))?
                    };
                }
                self.reset(now);
                Ok(self.synchronize())
            }
            Command::Detected => {
                self.stage = match self.stage {
                    Stage::Threshold => Stage::ThresholdReview,
                    Stage::Acceleration => Stage::AccelerationReview,
                    _ => return Err(Error::Invalid("当前没有正在运行的校准步骤".into())),
                };
                self.next = None;
                Ok(vec![[0; 4]])
            }
            Command::Accept => {
                match self.stage {
                    Stage::ThresholdReview => {
                        self.settings.threshold = self.candidate.expect("candidate").threshold;
                        self.stage = Stage::Acceleration;
                        self.candidate.as_mut().expect("candidate").acceleration = 100;
                        self.reset(now);
                        return Ok(self.synchronize());
                    }
                    Stage::AccelerationReview => {
                        self.settings = self.candidate.expect("candidate").validate()?;
                        self.stop();
                        self.message = Some("鼠标阈值与加速倍率已保存。".into());
                    }
                    _ => return Err(Error::Invalid("请先确认已找到对应读数".into())),
                }
                Ok(self.synchronize())
            }
            Command::Retry => {
                self.stage = match self.stage {
                    Stage::ThresholdReview => Stage::Threshold,
                    Stage::AccelerationReview => Stage::Acceleration,
                    _ => return Err(Error::Invalid("当前步骤无需返回调整".into())),
                };
                self.reset(now);
                Ok(self.synchronize())
            }
            Command::Cancel => {
                if !self.active() {
                    return Err(Error::Invalid("当前没有校准操作".into()));
                }
                self.stop();
                self.message = Some("校准已取消，未确认的参数已丢弃。".into());
                Ok(self.synchronize())
            }
            Command::Pause { paused } => {
                if !self.active() {
                    return Err(Error::Invalid("当前没有校准操作".into()));
                }
                self.paused = paused;
                self.next = (!paused).then_some(now + INTERVAL);
                self.alt = 0;
                if !paused && self.needs_sync {
                    return Ok(self.synchronize());
                }
                Ok(vec![[0; 4]])
            }
            Command::Configure { settings } => {
                if self.active() {
                    return Err(Error::Invalid("请先结束当前校准".into()));
                }
                self.settings = settings.validate()?;
                self.message = Some("鼠标参数已应用。".into());
                Ok(self.synchronize())
            }
            Command::Synchronize => {
                self.reset(now);
                Ok(self.synchronize())
            }
        }
    }
    pub fn suspend(&mut self) {
        if self.active() {
            self.paused = true;
            self.next = None;
            self.alt = 0;
        }
    }
    /// All calibration keys are local and must not enter the remote keyboard report.
    pub fn key(&mut self, code: &str, pressed: bool) -> Option<Command> {
        match code {
            "AltLeft" => {
                if pressed {
                    self.alt |= 1
                } else {
                    self.alt &= !1
                }
            }
            "AltRight" => {
                if pressed {
                    self.alt |= 2
                } else {
                    self.alt &= !2
                }
            }
            _ => {}
        }
        if !pressed {
            return None;
        }
        match code {
            "Minus" | "NumpadSubtract" => Some(Command::Adjust {
                direction: -1,
                fine: self.alt != 0,
            }),
            "Equal" | "NumpadAdd" => Some(Command::Adjust {
                direction: 1,
                fine: self.alt != 0,
            }),
            "KeyT" if self.alt != 0 => Some(Command::Detected),
            "Enter" | "NumpadEnter"
                if matches!(
                    self.stage,
                    Stage::ThresholdReview | Stage::AccelerationReview
                ) =>
            {
                Some(Command::Accept)
            }
            "Escape" => Some(Command::Cancel),
            _ => None,
        }
    }
    fn stop(&mut self) {
        self.stage = Stage::Idle;
        self.candidate = None;
        self.token = None;
        self.paused = false;
        self.next = None;
        self.alt = 0;
    }
    fn reset(&mut self, now: Instant) {
        self.next = self.running().then_some(now + INTERVAL);
        self.vertical = false;
        self.reset_reference();
    }
    fn reset_reference(&mut self) {
        let (x, y, _, _) = self.area.unwrap_or((0, 0, self.bounds.0, self.bounds.1));
        self.reference = Some(Point {
            x: f64::from(x),
            y: f64::from(y),
            width: self.bounds.0,
            height: self.bounds.1,
        });
        self.remainder = (0, 0);
    }
    fn synchronize(&mut self) -> Vec<[u8; 4]> {
        self.reset_reference();
        self.needs_sync = false;
        // Account for deceleration as well: a source-sized raw movement is
        // insufficient when the confirmed host multiplier is below one.
        let gain = if self.settings.threshold <= 252 {
            self.settings.acceleration.min(100)
        } else {
            100
        };
        let distance = |size: u32| (u64::from(size) * 100).div_ceil(u64::from(gain)) as i32;
        let mut reports = relative(0, -distance(self.bounds.0), -distance(self.bounds.1), 0);
        if let Some((x, y, _, _)) = self.area {
            if x > 0 || y > 0 {
                // Compensate a scrolled viewport using the confirmed host
                // acceleration, including chunks below its threshold.
                let mut position = (0i64, 0i64);
                let mut remainder = (0i64, 0i64);
                for _ in 0..8 {
                    let delta = (i64::from(x) - position.0, i64::from(y) - position.1);
                    let distance = delta.0.abs() + delta.1.abs();
                    if distance == 0 {
                        break;
                    }
                    let gain = if distance >= i64::from(self.settings.threshold)
                        && self.settings.threshold <= 252
                    {
                        f64::from(self.settings.acceleration) / 100.
                    } else {
                        1.
                    };
                    let mut raw = (
                        (delta.0 as f64 / gain).round() as i32,
                        (delta.1 as f64 / gain).round() as i32,
                    );
                    let chunks = if raw.0.unsigned_abs() + raw.1.unsigned_abs()
                        < u32::from(self.settings.threshold)
                    {
                        if self.settings.threshold == 1 {
                            break;
                        }
                        raw = (delta.0 as i32, delta.1 as i32);
                        let budget = i32::from(self.settings.threshold - 1).min(CHUNK);
                        let mut chunks = vec![];
                        while raw != (0, 0) {
                            let x = raw.0.clamp(-budget, budget);
                            let remaining = budget - x.abs();
                            let y = raw.1.clamp(-remaining, remaining);
                            chunks.push([0, x as i8 as u8, y as i8 as u8, 0]);
                            raw.0 -= x;
                            raw.1 -= y;
                        }
                        chunks
                    } else {
                        relative(0, raw.0, raw.1, 0)
                    };
                    let saved = (position, remainder, reports.len());
                    for r in chunks {
                        let dx = i64::from(r[1] as i8);
                        let dy = i64::from(r[2] as i8);
                        let gain = if dx.abs() + dy.abs() >= i64::from(self.settings.threshold) {
                            i64::from(self.settings.acceleration)
                        } else {
                            100
                        };
                        let amount = (dx * gain + remainder.0, dy * gain + remainder.1);
                        position.0 += amount.0 / 100;
                        position.1 += amount.1 / 100;
                        remainder = (amount.0 % 100, amount.1 % 100);
                        let clipped = (
                            position
                                .0
                                .clamp(0, i64::from(self.bounds.0.saturating_sub(1))),
                            position
                                .1
                                .clamp(0, i64::from(self.bounds.1.saturating_sub(1))),
                        );
                        if clipped.0 != position.0 {
                            remainder.0 = 0;
                        }
                        if clipped.1 != position.1 {
                            remainder.1 = 0;
                        }
                        position = clipped;
                        reports.push(r);
                    }
                    let remaining =
                        (i64::from(x) - position.0).abs() + (i64::from(y) - position.1).abs();
                    let inside = position.0 >= i64::from(x) && position.1 >= i64::from(y);
                    let was_inside = saved.0.0 >= i64::from(x) && saved.0.1 >= i64::from(y);
                    if remaining > distance || (remaining == distance && (!inside || was_inside)) {
                        // These reports have not been sent. Keep the closest
                        // reachable point instead of oscillating across it.
                        position = saved.0;
                        remainder = saved.1;
                        reports.truncate(saved.2);
                        break;
                    }
                }
                if let Some(p) = self.reference.as_mut() {
                    p.x = position.0 as f64;
                    p.y = position.1 as f64;
                }
                self.remainder = remainder;
            }
        }
        reports.push([0; 4]);
        reports
    }
    pub fn tick(&mut self, now: Instant) -> Vec<[u8; 4]> {
        if !self.running() || self.next.is_none_or(|next| now < next) {
            return vec![];
        }
        self.next = Some(now + INTERVAL); // no catch-up burst after a slow socket write
        let s = self.candidate.expect("running candidate");
        let (x, y) = if self.stage == Stage::Threshold {
            let v = if self.vertical {
                (0, i32::from(s.threshold))
            } else {
                (i32::from(s.threshold), 0)
            };
            self.vertical = !self.vertical;
            v
        } else {
            (i32::from(s.threshold), i32::from(s.threshold))
        };
        let step_x = if self.stage == Stage::Acceleration {
            (u64::from(s.threshold) * u64::from(s.acceleration) + 50) / 100
        } else {
            x as u64
        };
        let step_y = if self.stage == Stage::Acceleration {
            (u64::from(s.threshold) * u64::from(s.acceleration) + 50) / 100
        } else {
            y as u64
        };
        let p = self.reference.expect("running reference");
        let (_, _, max_x, max_y) = self.area.unwrap_or((0, 0, self.bounds.0, self.bounds.1));
        if p.x + step_x as f64 >= f64::from(max_x) || p.y + step_y as f64 >= f64::from(max_y) {
            return self.synchronize();
        }
        self.reference.as_mut().unwrap().x += step_x as f64;
        self.reference.as_mut().unwrap().y += step_y as f64;
        relative(0, x, y, 0)
    }
    pub fn movement(&mut self, reports: &[Vec<u8>]) {
        if !self.relative || !self.eligible || self.active() {
            return;
        }
        let Some(p) = self.reference.as_mut() else {
            return;
        };
        for r in reports.iter().filter(|r| r.len() == 4) {
            let (dx, dy) = (i32::from(r[1] as i8), i32::from(r[2] as i8));
            let gain =
                if dx.unsigned_abs() + dy.unsigned_abs() >= u32::from(self.settings.threshold) {
                    self.settings.acceleration
                } else {
                    100
                };
            for (coordinate, remainder, delta, max) in [
                (&mut p.x, &mut self.remainder.0, dx, p.width),
                (&mut p.y, &mut self.remainder.1, dy, p.height),
            ] {
                let amount = i64::from(delta) * i64::from(gain) + *remainder;
                *coordinate += (amount / 100) as f64;
                *remainder = amount % 100;
                let clipped = coordinate.clamp(0., f64::from(max.saturating_sub(1)));
                if clipped != *coordinate {
                    *remainder = 0;
                }
                *coordinate = clipped;
            }
        }
    }
}

pub fn relative(buttons: u8, mut x: i32, mut y: i32, wheel: i8) -> Vec<[u8; 4]> {
    let mut result = vec![];
    loop {
        let (dx, dy) = (x.clamp(-CHUNK, CHUNK), y.clamp(-CHUNK, CHUNK));
        result.push([
            buttons & 7,
            dx as i8 as u8,
            dy as i8 as u8,
            if result.is_empty() { wheel as u8 } else { 0 },
        ]);
        x -= dx;
        y -= dy;
        if x == 0 && y == 0 {
            break;
        }
    }
    result
}
