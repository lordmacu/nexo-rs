//! Error type for the fork subagent. Step 80.19 / 2.

use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ForkError {
    #[error("LLM error: {0}")]
    Llm(String),
    #[error("Tool dispatch error: {0}")]
    Tool(String),
    #[error("Tool denied by filter: {0}")]
    ToolDenied(String),
    #[error("Aborted by caller")]
    Aborted,
    #[error("Timeout after {0:?}")]
    Timeout(Duration),
    #[error("Subagent registry error: {0}")]
    Registry(String),
    #[error("Internal: {0}")]
    Internal(String),
}

impl From<anyhow::Error> for ForkError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_stable() {
        let e = ForkError::Timeout(Duration::from_secs(30));
        assert_eq!(format!("{e}"), "Timeout after 30s");
    }

    #[test]
    fn from_anyhow_internal() {
        let e: ForkError = anyhow::anyhow!("boom").into();
        assert!(matches!(e, ForkError::Internal(s) if s == "boom"));
    }
}
