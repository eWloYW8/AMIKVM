#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("Authentication failed: {0}")]
    Authentication(String),
    #[error("{0}")]
    SinglePort(#[from] SinglePortRejection),
    #[error("{0}")]
    VideoSession(#[from] VideoSessionError),
    #[error("BMC protocol error: {0}")]
    Protocol(String),
    #[error("Operation timed out: {0}")]
    Timeout(&'static str),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("TLS error: {0}")]
    Tls(#[from] native_tls::Error),
    #[error("HTTP connection failed: {0}")]
    Http(String),
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        // Login requests carry credentials in the query on these BMCs.
        Self::Http(error.without_url().to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Only the numeric rejection code is retained; a gateway reply can contain
/// cookies or other credentials and must never become a diagnostic message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinglePortRejection {
    pub code: Option<u16>,
}

impl SinglePortRejection {
    /// SinglePortKVM.ao and Resources_EN define AE_1 through AE_12_SPKVM.
    pub fn message(self) -> &'static str {
        match self.code {
            Some(1) => "单端口连接被拒绝：Web 会话索引无效",
            Some(2) => "单端口连接被拒绝：Web 会话未激活",
            Some(3) => "单端口连接被拒绝：Web 会话槽位已满",
            Some(4) => "单端口连接被拒绝：Web 会话 Cookie 无效",
            Some(5) => "单端口连接被拒绝：Web 会话 Cookie 格式错误",
            Some(6) => "单端口连接被拒绝：用户名或密码过长",
            Some(7) => "单端口连接被拒绝：Web 登录失败",
            Some(8) => "单端口连接被拒绝：Web 会话已注销",
            Some(9) => "单端口连接被拒绝：缺少 Web 会话 Cookie",
            Some(10) => "单端口连接被拒绝：无法启动控制台",
            Some(11) => "单端口连接被拒绝：断开连接失败",
            Some(12) => "单端口连接被拒绝：Web 会话注销失败",
            _ => "单端口连接被 BMC 拒绝",
        }
    }
}

impl std::fmt::Display for SinglePortRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for SinglePortRejection {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoSessionError {
    Validation(u8),
    SessionLimit(u16),
    MissingValidationStatus,
    UnexpectedValidation,
    UnsupportedSoc(u16),
    InvalidClientList,
    LocalAddressesUnavailable,
}

impl VideoSessionError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Validation(0) => "KVM 认证失败：会话 token 无效",
            Self::Validation(2) => "KVM 认证失败：账号没有 KVM 权限",
            Self::Validation(3) => "KVM 认证失败：重连会话信息无效",
            Self::Validation(6) => "KVM 认证失败：重连客户端 IP 不匹配",
            Self::Validation(7) => "KVM 认证失败：重连客户端 MAC 不匹配",
            Self::Validation(8) => "KVM 认证失败：重连会话信息不存在",
            Self::Validation(_) => "BMC 拒绝了 KVM 会话认证",
            Self::SessionLimit(0) => "KVM 会话数量已达上限，请关闭其他会话后重试",
            Self::SessionLimit(1) => "同一客户端只能连接此 BMC 的一个 KVM 会话",
            Self::SessionLimit(_) => "BMC 拒绝了 KVM 会话连接",
            Self::MissingValidationStatus => "KVM 认证响应缺少状态码",
            Self::UnexpectedValidation => "尚未请求 KVM 认证，BMC 已返回成功响应",
            Self::UnsupportedSoc(_) => "BMC 的视频芯片与 AST 客户端不匹配",
            Self::InvalidClientList => "BMC 的 KVM 客户端地址列表长度无效",
            Self::LocalAddressesUnavailable => "无法读取本机网络接口以建立 KVM 会话",
        }
    }
}

impl std::fmt::Display for VideoSessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for VideoSessionError {}
