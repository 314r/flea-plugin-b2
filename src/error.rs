//! One error type: each variant says whether a retry can help and what the user reads.
use std::fmt;

#[derive(Debug, Clone)]
pub enum Error {
    /// Backblaze answered with an error status.
    B2 {
        status: u16,
        code: String,
        message: String,
        retry_after: Option<f64>,
    },
    /// The connection failed or timed out: worth retrying.
    Network(String),
    /// A local read or write failed: never retried as a network problem.
    Local(String),
    /// A refusal or a failure to report as is.
    Msg(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn msg(text: impl Into<String>) -> Self {
        Error::Msg(text.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::B2 { message, .. }
            | Error::Network(message)
            | Error::Local(message)
            | Error::Msg(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Local(error.to_string())
    }
}
