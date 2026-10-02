pub mod exit;

use crate::{Error, Result, protocol};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub name: String,
    pub address: String,
    pub id: u8,
    pub privileges: u32,
}
impl User {
    pub fn parse(body: &[u8]) -> Result<Self> {
        if body.len() != 134 {
            return Err(Error::Protocol("Invalid session user record".into()));
        }
        let string = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes.split(|b| *b == 0).next().unwrap_or_default())
                .trim()
                .to_owned()
        };
        Ok(Self {
            name: string(&body[..64]),
            address: string(&body[64..129]),
            id: body[129],
            privileges: u32::from_le_bytes(body[130..134].try_into().unwrap()),
        })
    }
    pub fn bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = vec![0; 134];
        if self.name.len() > 64
            || self.address.len() > 65
            || self.name.contains('\0')
            || self.address.contains('\0')
        {
            return Err(Error::Protocol(
                "Session user identity exceeds packet limits".into(),
            ));
        }
        bytes[..self.name.len()].copy_from_slice(self.name.as_bytes());
        bytes[64..64 + self.address.len()].copy_from_slice(self.address.as_bytes());
        bytes[129] = self.id;
        Ok(bytes)
    }
    pub fn identity(&self) -> Identity {
        Identity {
            name: self.name.clone(),
            address: self.address.clone(),
        }
    }
    pub fn role_label(&self) -> &'static str {
        match self.privileges {
            4 => "管理员",
            3 => "操作员",
            2 => "用户",
            5 => "专有权限",
            _ => "未知权限",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub address: String,
}
pub fn users(body: &[u8]) -> Result<Vec<User>> {
    if body.len() % 134 != 0 {
        return Err(Error::Protocol("Truncated session user list".into()));
    }
    body.chunks_exact(134).map(User::parse).collect()
}
pub fn answer(user: &User, decision: &str) -> Result<Vec<u8>> {
    let value = match decision {
        "grant" => 0,
        "partial" => 2,
        "deny" => 1,
        "block_partial" => 6,
        "block_deny" => 8,
        _ => return Err(Error::Invalid("Unknown control permission decision".into())),
    };
    protocol::packet(32, 1 + (value << 8), &user.bytes()?)
}
pub fn transfer(user: &User) -> Result<Vec<u8>> {
    protocol::packet(50, 0, &user.bytes()?)
}
pub fn disconnect(id: u8) -> Result<Vec<u8>> {
    let mut body = vec![5];
    body.extend_from_slice(&(id as u32).to_le_bytes());
    protocol::packet(54, 0, &body)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    #[default]
    Ask,
    ViewOnly,
    Deny,
}
impl Policy {
    pub fn value(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::ViewOnly => "view_only",
            Self::Deny => "deny",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "逐次询问",
            Self::ViewOnly => "自动仅允许查看",
            Self::Deny => "自动拒绝访问",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    #[default]
    Disconnected,
    Master,
    Viewer,
    Inactive,
    Rejected,
}
impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::Disconnected => "未连接",
            Self::Master => "持有控制权限",
            Self::Viewer => "仅查看",
            Self::Inactive => "控制权限已暂停",
            Self::Rejected => "访问已拒绝",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub token: Uuid,
    pub user: User,
    pub existing_session: bool,
    pub seconds_remaining: u64,
    #[serde(skip)]
    deadline: Instant,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Waiting {
    pub user: Option<User>,
    pub seconds_remaining: u64,
    #[serde(skip)]
    deadline: Instant,
}
impl Waiting {
    fn new(user: Option<User>, now: Instant) -> Self {
        Self {
            user,
            seconds_remaining: REQUEST_TIMEOUT.as_secs(),
            deadline: now + REQUEST_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub policy: Policy,
    pub role: Role,
    pub requests: Vec<Request>,
    pub waiting: Option<Waiting>,
    pub handoff: Option<Waiting>,
    pub message: Option<String>,
    pub status: u16,
}

#[derive(Default)]
pub struct Effect {
    pub reply: Option<Vec<u8>>,
    pub close: bool,
    pub lost_control: bool,
    pub gained_control: bool,
}

fn remaining(deadline: Instant, now: Instant) -> u64 {
    let milliseconds = deadline.saturating_duration_since(now).as_millis();
    milliseconds.div_ceil(1000) as u64
}

impl State {
    pub fn authenticated(&mut self, first_client: bool) {
        *self = Self::default();
        self.role = if first_client {
            Role::Master
        } else {
            Role::Viewer
        };
    }
    pub fn can_control(&self) -> bool {
        self.role == Role::Master && self.handoff.is_none()
    }
    pub fn can_request(&self) -> bool {
        matches!(self.role, Role::Viewer | Role::Inactive)
            && self.waiting.is_none()
            && self.handoff.is_none()
    }
    fn master(&self) -> Result<()> {
        if !self.can_control() {
            return Err(Error::Authentication("本会话没有控制权限".into()));
        }
        Ok(())
    }
    pub fn set_policy(&mut self, policy: Policy) -> Result<()> {
        self.master()?;
        self.policy = policy;
        self.message = Some(format!("后续权限申请：{}", policy.label()));
        Ok(())
    }
    pub fn request_control(&mut self, now: Instant) -> Result<Vec<u8>> {
        self.expire(now);
        if !self.can_request() {
            return Err(Error::Invalid("当前无法重复申请控制权限".into()));
        }
        self.waiting = Some(Waiting::new(None, now));
        self.message = Some("已申请控制权限，等待 BMC 回复。".into());
        Ok(protocol::command(50, 0))
    }
    fn start_handoff(&mut self, user: User, now: Instant) {
        self.handoff = Some(Waiting::new(Some(user), now));
        self.waiting = None;
        self.requests.clear();
        self.policy = Policy::Ask;
        self.message = Some("正在移交控制权限，等待 BMC 确认。".into());
    }
    pub fn transfer(&mut self, user: User, now: Instant) -> Result<Vec<u8>> {
        self.master()?;
        let bytes = transfer(&user)?;
        self.start_handoff(user, now);
        Ok(bytes)
    }
    pub fn decide(
        &mut self,
        token: Uuid,
        user_id: u8,
        decision: &str,
        now: Instant,
    ) -> Result<Vec<u8>> {
        self.expire(now);
        self.master()?;
        let index = self
            .requests
            .iter()
            .position(|r| r.token == token && r.user.id == user_id && now < r.deadline)
            .ok_or_else(|| Error::Invalid("权限申请已经结束，请使用当前列表。".into()))?;
        let request = &self.requests[index];
        // JViewer hides the ordinary deny choice for an existing-session request.
        if request.existing_session && decision == "deny" {
            return Err(Error::Invalid(
                "现有会话申请不提供单次拒绝，请选择仅查看或阻止后续申请。".into(),
            ));
        }
        let bytes = answer(&request.user, decision)?;
        let user = request.user.clone();
        self.requests.remove(index);
        match decision {
            "grant" => self.start_handoff(user, now),
            "block_partial" => {
                self.policy = Policy::ViewOnly;
                self.message = Some(format!("{} 仅可查看；后续申请自动仅允许查看。", user.name));
            }
            "block_deny" => {
                self.policy = Policy::Deny;
                self.message = Some(format!("已拒绝 {}；后续申请自动拒绝访问。", user.name));
            }
            "partial" => self.message = Some(format!("{} 仅可查看。", user.name)),
            "deny" => self.message = Some(format!("已拒绝 {} 的访问申请。", user.name)),
            _ => unreachable!(),
        }
        Ok(bytes)
    }
    pub fn receive(&mut self, kind: u16, status: u16, body: &[u8], now: Instant) -> Result<Effect> {
        let mut effect = Effect::default();
        let was_master = self.role == Role::Master;
        let could_control = self.can_control();
        self.expire(now);
        if kind == 33 {
            self.role = Role::Inactive;
            self.clear_pending();
            self.message = Some("BMC 已暂停本会话的控制权限。".into());
        } else if matches!(kind, 32 | 50) {
            let user = if body.is_empty() {
                None
            } else {
                Some(User::parse(body)?)
            };
            self.status = status;
            let operation = status as u8;
            let permission = (status >> 8) as u8;
            match operation {
                0 => {
                    let cancels_handoff = self.handoff.as_ref().is_some_and(|h| {
                        user.as_ref().is_none_or(|user| {
                            h.user.as_ref().is_some_and(|target| {
                                target.id == user.id && target.identity() == user.identity()
                            })
                        })
                    });
                    if let Some(user) = user {
                        self.requests.retain(|r| {
                            r.user.id != user.id || r.user.identity() != user.identity()
                        });
                    } else {
                        self.requests.clear();
                    }
                    // JViewer's cancel handler closes the requester dialog as
                    // well as the approval dialog; permit an immediate retry.
                    self.waiting = None;
                    if cancels_handoff {
                        self.handoff = None;
                    }
                    self.message = Some("BMC 已取消权限申请。".into());
                }
                1 => {
                    let user =
                        user.ok_or_else(|| Error::Protocol("权限申请缺少用户记录".into()))?;
                    if !self.can_control() {
                        self.message = Some("收到权限申请，但本会话当前无权应答。".into());
                    } else if self.policy != Policy::Ask {
                        let decision = if self.policy == Policy::ViewOnly {
                            "block_partial"
                        } else {
                            "block_deny"
                        };
                        effect.reply = Some(answer(&user, decision)?);
                        self.message =
                            Some(format!("已自动应答 {}：{}", user.name, self.policy.label()));
                    } else if let Some(request) = self
                        .requests
                        .iter_mut()
                        .find(|r| r.user.id == user.id && r.user.identity() == user.identity())
                    {
                        request.existing_session |= kind == 50;
                    } else {
                        self.requests.retain(|r| r.user.id != user.id);
                        self.requests.push(Request {
                            token: Uuid::new_v4(),
                            user,
                            existing_session: kind == 50,
                            seconds_remaining: REQUEST_TIMEOUT.as_secs(),
                            deadline: now + REQUEST_TIMEOUT,
                        });
                        self.message = None;
                    }
                }
                2 => {
                    if self.waiting.as_ref().is_none_or(|w| w.user.is_none()) {
                        self.waiting = Some(Waiting::new(user, now));
                    }
                    self.message = Some("等待当前控制者应答，权限以 BMC 通知为准。".into());
                }
                3 => {
                    self.role = Role::Viewer;
                    self.clear_pending();
                    self.message = Some(match user {
                        Some(user) => format!(
                            "控制权限已交给 {}（{}），本会话仅可查看。",
                            user.name, user.address
                        ),
                        None => "控制权限已移交，本会话仅可查看。".into(),
                    });
                }
                4 | 6 => {
                    let (role, message) = if operation == 6 {
                        if permission == 0 {
                            (Role::Master, "BMC 已授予控制权限。")
                        } else {
                            (Role::Viewer, "BMC 已设置本会话为仅查看。")
                        }
                    } else {
                        match permission {
                            0 => (Role::Master, "BMC 已授予控制权限。"),
                            1 => (Role::Rejected, "访问申请被拒绝，KVM 会话将关闭。"),
                            2 => (Role::Viewer, "已获得仅查看权限。"),
                            3 => (Role::Master, "BMC 因申请超时授予控制权限。"),
                            4 => (Role::Viewer, "控制者已重新连接，本会话仅可查看。"),
                            5 => (
                                Role::Viewer,
                                "控制者正在处理其他申请；本会话仅可查看，可稍后重试。",
                            ),
                            6 => (Role::Viewer, "控制者阻止控制权限申请，仅允许查看。"),
                            7 => (
                                Role::Viewer,
                                "控制者正在重新连接；本会话仅可查看，可稍后重试。",
                            ),
                            8 => (Role::Rejected, "控制者阻止访问申请，KVM 会话将关闭。"),
                            9 => (Role::Viewer, "原控制者已经离线，可重新申请控制权限。"),
                            _ => {
                                self.message = Some(format!(
                                    "未知 BMC 权限结果 {permission}，保留已确认的权限。"
                                ));
                                return Ok(effect);
                            }
                        }
                    };
                    self.role = role;
                    self.clear_pending();
                    self.message = Some(message.into());
                    effect.close = role == Role::Rejected;
                }
                _ => {
                    self.message =
                        Some(format!("未知 BMC 共享操作 {operation}，保留已确认的权限。"))
                }
            }
        } else {
            return Err(Error::Invalid("不是共享权限报文".into()));
        }
        effect.lost_control = was_master && self.role != Role::Master;
        effect.gained_control = !could_control && self.can_control();
        Ok(effect)
    }
    fn clear_pending(&mut self) {
        self.requests.clear();
        self.waiting = None;
        self.handoff = None;
        self.policy = Policy::Ask;
    }
    pub fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for request in &mut self.requests {
            let seconds = remaining(request.deadline, now);
            changed |= request.seconds_remaining != seconds;
            request.seconds_remaining = seconds;
        }
        let before = self.requests.len();
        self.requests.retain(|r| now < r.deadline);
        if self.requests.len() != before {
            self.message = Some("权限申请已经超时；后续权限变化仍以 BMC 通知为准。".into());
            changed = true;
        }
        for waiting in [&mut self.waiting, &mut self.handoff].into_iter().flatten() {
            let seconds = remaining(waiting.deadline, now);
            changed |= waiting.seconds_remaining != seconds;
            waiting.seconds_remaining = seconds;
        }
        if self.waiting.as_ref().is_some_and(|w| now >= w.deadline) {
            self.waiting = None;
            self.message =
                Some("等待权限应答已超时；未自行授予控制权限，可重试或等待 BMC 通知。".into());
            changed = true;
        }
        if self.handoff.as_ref().is_some_and(|w| now >= w.deadline) {
            self.handoff = None;
            self.role = Role::Inactive;
            self.message = Some("控制权限移交未收到 BMC 确认；请刷新会话或重新连接。".into());
            changed = true;
        }
        changed
    }
    pub fn close(&mut self) {
        let pending = !self.requests.is_empty() || self.waiting.is_some() || self.handoff.is_some();
        self.clear_pending();
        self.role = Role::Disconnected;
        if pending {
            self.message = Some("会话已断开，未完成的权限操作已结束。".into());
        }
    }
}
