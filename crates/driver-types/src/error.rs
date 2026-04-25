//! Errors any harness can surface.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("harness {0} not found")]
    NotFound(String),
    #[error("harness rejected attempt: {0}")]
    Unsupported(String),
    #[error("subprocess error: {0}")]
    Subprocess(String),
    #[error("session expired or invalid: {0}")]
    SessionInvalid(String),
    #[error("acceptance evaluator failed: {0}")]
    Acceptance(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}
