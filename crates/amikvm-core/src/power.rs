//! IVTP power control: acknowledgement is separate from observed host power.
use crate::{
    Error, Result,
    protocol::{self, Control, PowerOperation},
};
use serde::Serialize;
use std::time::{Duration, Instant};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const OFF_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Waiting,
    Accepted,
    Rejected,
    TimedOut,
    Interrupted,
}
impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "尚未执行电源操作",
            Self::Waiting => "等待服务器确认电源操作",
            Self::Accepted => "服务器已确认电源操作",
            Self::Rejected => "服务器拒绝了电源操作",
            Self::TimedOut => "电源操作回执超时，结果未知；请重新连接后再操作",
            Self::Interrupted => "连接中断，电源操作结果未知",
        }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub status: Option<u8>,
    pub phase: Phase,
    pub operation: Option<PowerOperation>,
    pub response_code: Option<u8>,
    pub needs_reconnect: bool,
    pub query_pending: bool,
    pub query_failed: bool,
    pub waiting_for_off: bool,
    #[serde(skip)]
    operation_deadline: Option<Instant>,
    #[serde(skip)]
    query_deadline: Option<Instant>,
    #[serde(skip)]
    next_query: Option<Instant>,
}

impl State {
    pub fn available(&self, operation: PowerOperation) -> bool {
        self.phase != Phase::Waiting
            && !self.needs_reconnect
            && match self.status {
                Some(0) => operation == PowerOperation::On,
                Some(1) => operation != PowerOperation::On,
                _ => true,
            }
    }

    /// Called by the transport writer immediately before writing command 35.
    pub fn begin(&mut self, operation: PowerOperation, now: Instant) -> Result<Vec<u8>> {
        if self.needs_reconnect {
            return Err(Error::Invalid(Phase::TimedOut.label().into()));
        }
        if self.phase == Phase::Waiting {
            return Err(Error::Invalid("已有电源操作正在等待服务器回执".into()));
        }
        if !self.available(operation) {
            return Err(Error::Invalid(
                "该电源操作不适用于服务器当前电源状态".into(),
            ));
        }
        let bytes = Control::Power { operation }.encode()?;
        self.operation = Some(operation);
        self.phase = Phase::Waiting;
        self.response_code = None;
        self.operation_deadline = Some(now + RESPONSE_TIMEOUT);
        self.waiting_for_off = false;
        Ok(bytes)
    }

    /// Command 36 has no request identifier. Unsolicited/late replies cannot
    /// resolve a new operation, and a timeout quarantines this link's controls.
    pub fn acknowledge(&mut self, status: u16, now: Instant) -> bool {
        self.expire(now);
        if self.phase != Phase::Waiting || self.needs_reconnect {
            return false;
        }
        let status = status as u8; // JViewer uses the low byte of the IVTP status.
        self.response_code = Some(status);
        self.phase = if status == 0 {
            Phase::Accepted
        } else {
            Phase::Rejected
        };
        self.operation_deadline = None;
        self.waiting_for_off = status == 0
            && matches!(
                self.operation,
                Some(PowerOperation::Off | PowerOperation::Shutdown)
            );
        // Neither a successful write nor an ACK predicts physical host power.
        self.next_query = Some(now);
        true
    }

    /// At most one status query is outstanding; additional refreshes coalesce.
    pub fn query(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.query_pending {
            return None;
        }
        self.query_pending = true;
        self.query_failed = false;
        self.query_deadline = Some(now + RESPONSE_TIMEOUT);
        self.next_query = None;
        Some(protocol::command(34, 0))
    }

    pub fn status_reply(&mut self, status: u16, now: Instant) {
        self.query_pending = false;
        self.query_deadline = None;
        self.status = match status as u8 {
            status @ 0..=1 => Some(status),
            _ => None,
        };
        self.query_failed = self.status.is_none();
        if self.waiting_for_off {
            if self.status == Some(0) {
                self.waiting_for_off = false;
                self.next_query = None;
            } else {
                self.next_query.get_or_insert(now + OFF_POLL_INTERVAL);
            }
        }
    }

    pub fn query_due(&self, now: Instant) -> bool {
        !self.query_pending && self.next_query.is_some_and(|deadline| now >= deadline)
    }

    pub fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self
            .operation_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.phase = Phase::TimedOut;
            self.needs_reconnect = true;
            self.operation_deadline = None;
            self.next_query = Some(now);
            changed = true;
        }
        if self.query_deadline.is_some_and(|deadline| now >= deadline) {
            self.query_pending = false;
            self.query_failed = true;
            self.status = None;
            self.query_deadline = None;
            if self.waiting_for_off {
                self.next_query.get_or_insert(now + OFF_POLL_INTERVAL);
            }
            changed = true;
        }
        changed
    }

    pub fn close(&mut self) {
        if self.phase == Phase::Waiting {
            self.phase = Phase::Interrupted;
        }
        self.status = None;
        self.query_pending = false;
        self.query_failed = false;
        self.waiting_for_off = false;
        self.operation_deadline = None;
        self.query_deadline = None;
        self.next_query = None;
    }
}
