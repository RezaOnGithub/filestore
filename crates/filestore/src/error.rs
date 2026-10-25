use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    /// The request is malformed or violates a rule; the caller can fix it.
    #[error("{0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
    /// The request was valid but conflicts with current state (stale
    /// suggestion, reused request ID, closed scan, ...).
    #[error("{0}")]
    Conflict(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Invalid(format!("invalid JSON: {e}"))
    }
}
