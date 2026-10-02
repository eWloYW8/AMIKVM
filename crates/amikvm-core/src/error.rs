#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("Authentication failed: {0}")]
    Authentication(String),
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
