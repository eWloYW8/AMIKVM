//! Bounded reconnection scheduling, independent of desktop rendering and timers.
use crate::Error;
use serde::Serialize;
use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    #[default]
    Idle,
    Waiting,
    Connecting,
    Authenticating,
    Exhausted,
}
impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "连接正常",
            Self::Waiting => "等待重试",
            Self::Connecting => "重新建立连接",
            Self::Authenticating => "恢复会话认证",
            Self::Exhausted => "重试次数已用尽",
        }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub attempt: u32,
    pub limit: u32,
    pub seconds_remaining: u64,
    pub stage: Stage,
    pub last_error: Option<String>,
    #[serde(skip)]
    deadline: Option<Instant>,
}
impl State {
    pub fn schedule(&mut self, limit: u32, interval: u32, now: Instant, error: String) -> bool {
        self.limit = limit;
        self.last_error = Some(error);
        self.seconds_remaining = 0;
        self.deadline = None;
        if self.attempt >= limit {
            self.stage = Stage::Exhausted;
            return false;
        }
        self.attempt += 1;
        self.stage = Stage::Waiting;
        self.deadline = Some(now + Duration::from_secs(u64::from(interval)));
        self.tick(now);
        true
    }
    pub fn tick(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.deadline else {
            return false;
        };
        let remaining = deadline.saturating_duration_since(now);
        self.seconds_remaining = remaining.as_secs() + u64::from(remaining.subsec_nanos() != 0);
        if now >= deadline {
            self.stage = Stage::Connecting;
            self.deadline = None;
            true
        } else {
            false
        }
    }
    pub fn authenticating(&mut self) {
        self.stage = Stage::Authenticating;
        self.seconds_remaining = 0;
        self.deadline = None;
    }
    pub fn authenticated(&mut self) {
        *self = Self::default();
    }
}

pub fn retryable(error: &Error) -> bool {
    match error {
        Error::Io(_) | Error::Timeout(_) => true,
        Error::Tls(error) => {
            // Certificate/protocol errors remain terminal; transport IO failures
            // wrapped by a TLS backend can be retried without weakening trust.
            let mut source = std::error::Error::source(error);
            while let Some(error) = source {
                if error.downcast_ref::<std::io::Error>().is_some() {
                    return true;
                }
                source = error.source();
            }
            false
        }
        _ => false,
    }
}
