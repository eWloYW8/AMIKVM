//! The original next-master chooser lasts ten seconds; expiration never transfers.
use super::User;
use crate::{Error, Result};
use serde::Serialize;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const SELECTION_TIME: Duration = Duration::from_secs(10);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "serverId", rename_all = "snake_case")]
pub enum Scope {
    Application,
    Server(Uuid),
}
impl Scope {
    pub fn contains(self, id: Uuid) -> bool {
        matches!(self, Self::Application) || self == Self::Server(id)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Choosing,
    Closing,
    Cancelled,
}

pub struct Session {
    pub id: Uuid,
    pub name: String,
    pub eligible: bool,
    pub own_id: Option<u8>,
    pub users: Vec<User>,
}
#[derive(Clone, Serialize)]
pub struct Candidate {
    pub token: Uuid,
    pub user: User,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub server_id: Uuid,
    pub name: String,
    pub candidates: Vec<Candidate>,
    pub selected: Option<Uuid>,
    pub message: Option<String>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub id: Uuid,
    pub scope: Scope,
    pub phase: Phase,
    pub seconds_remaining: u64,
    pub steps: Vec<Step>,
    pub messages: Vec<String>,
    #[serde(skip)]
    deadline: Option<Instant>,
    #[serde(skip)]
    transfers: Vec<(Uuid, User)>,
}
impl Plan {
    pub fn new(scope: Scope) -> Self {
        Self {
            id: Uuid::new_v4(),
            scope,
            phase: Phase::Preparing,
            seconds_remaining: 10,
            steps: vec![],
            messages: vec![],
            deadline: None,
            transfers: vec![],
        }
    }
    pub fn upgrade(&mut self, scope: Scope) -> Result<()> {
        if scope == self.scope || self.scope == Scope::Application {
            return Ok(());
        }
        if self.phase == Phase::Closing || self.phase == Phase::Cancelled {
            return Err(Error::Invalid("当前关闭操作已开始，请等待收尾。".into()));
        }
        if scope == Scope::Application {
            self.scope = scope;
            Ok(())
        } else {
            Err(Error::Invalid("请先处理当前关闭窗口。".into()))
        }
    }
    pub fn begin(&mut self, sessions: Vec<Session>, now: Instant) {
        if self.phase != Phase::Preparing {
            return;
        }
        self.phase = Phase::Choosing;
        self.deadline = Some(now + SELECTION_TIME);
        self.sync(sessions, now);
    }
    pub fn sync(&mut self, sessions: Vec<Session>, now: Instant) {
        if self.phase != Phase::Choosing {
            return;
        }
        let mut previous = std::mem::take(&mut self.steps);
        for session in sessions {
            if !self.scope.contains(session.id) || !session.eligible {
                continue;
            }
            let Some(own) = session.own_id else {
                continue;
            };
            let mut step = previous
                .iter()
                .position(|s| s.server_id == session.id)
                .map(|i| previous.remove(i))
                .unwrap_or_else(|| Step {
                    server_id: session.id,
                    name: session.name.clone(),
                    candidates: vec![],
                    selected: None,
                    message: None,
                });
            step.name = session.name;
            let old = std::mem::take(&mut step.candidates);
            for user in session.users.into_iter().filter(|u| u.id != own) {
                if step.candidates.iter().any(|c| c.user.id == user.id) {
                    continue;
                }
                let token = old
                    .iter()
                    .find(|c| c.user.id == user.id && c.user.identity() == user.identity())
                    .map_or_else(Uuid::new_v4, |c| c.token);
                step.candidates.push(Candidate { token, user });
            }
            step.candidates.sort_by(|a, b| {
                a.user
                    .name
                    .cmp(&b.user.name)
                    .then(a.user.id.cmp(&b.user.id))
            });
            if step
                .selected
                .is_some_and(|selected| !step.candidates.iter().any(|c| c.token == selected))
            {
                step.selected = None;
                step.message = Some("所选会话已离线或身份变化；此次不转交，仍可重新选择。".into());
            }
            if !step.candidates.is_empty() {
                self.steps.push(step);
            }
        }
        self.steps
            .sort_by(|a, b| a.name.cmp(&b.name).then(a.server_id.cmp(&b.server_id)));
        let deadline = self.deadline.expect("choosing has deadline");
        self.seconds_remaining = deadline
            .saturating_duration_since(now)
            .as_millis()
            .div_ceil(1000) as u64;
        if self.steps.is_empty() || now >= deadline {
            self.close(false, now);
        }
    }
    pub fn choose(
        &mut self,
        plan: Uuid,
        server: Uuid,
        target: Option<Uuid>,
        now: Instant,
    ) -> Result<()> {
        if plan != self.id
            || self.phase != Phase::Choosing
            || self.deadline.is_none_or(|d| now >= d)
        {
            return Err(Error::Invalid("该关闭选择已经结束。".into()));
        }
        let step = self
            .steps
            .iter_mut()
            .find(|s| s.server_id == server)
            .ok_or_else(|| Error::Invalid("当前服务器不再需要选择控制者。".into()))?;
        if target.is_some_and(|target| !step.candidates.iter().any(|c| c.token == target)) {
            return Err(Error::Invalid("所选会话已变化，请使用当前列表。".into()));
        }
        step.selected = target;
        step.message = None;
        Ok(())
    }
    pub fn finish(&mut self, plan: Uuid, transfer: bool, now: Instant) -> Result<()> {
        if plan != self.id || self.phase != Phase::Choosing {
            return Err(Error::Invalid("该关闭选择已经结束。".into()));
        }
        self.close(transfer, now);
        Ok(())
    }
    fn close(&mut self, transfer: bool, now: Instant) {
        if transfer && self.deadline.is_some_and(|d| now < d) {
            self.transfers = self
                .steps
                .iter()
                .filter_map(|s| {
                    s.candidates
                        .iter()
                        .find(|c| Some(c.token) == s.selected)
                        .map(|c| (s.server_id, c.user.clone()))
                })
                .collect();
        }
        self.phase = Phase::Closing;
        self.seconds_remaining = 0;
    }
    pub fn transfers(&self) -> Vec<(Uuid, User)> {
        self.transfers.clone()
    }
    pub fn cancel(&mut self, plan: Uuid) -> Result<()> {
        if plan != self.id || !matches!(self.phase, Phase::Preparing | Phase::Choosing) {
            return Err(Error::Invalid(
                "关闭已经开始，正在等待文件和会话收尾。".into(),
            ));
        }
        self.phase = Phase::Cancelled;
        self.transfers.clear();
        Ok(())
    }
}
