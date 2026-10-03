#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("Authentication failed: {0}")]
    Authentication(String),
    #[error("{0}")]
    SinglePort(#[from] SinglePortRejection),
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
