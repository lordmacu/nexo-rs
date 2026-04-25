use thiserror::Error;

#[derive(Debug, Error)]
pub enum PermissionError {
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("decider error: {0}")]
    Decider(String),
    #[error("decider timeout")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
}
