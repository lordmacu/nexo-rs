//! Error type for AutoDream. Phase 80.1.

use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AutoDreamError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Lock acquire blocked by live PID {pid} (mtime {mtime_secs}s ago)")]
    LockBlocked { pid: i32, mtime_secs: i64 },
    #[error("Fork timeout after {0:?}")]
    Timeout(Duration),
    #[error("Fork error: {0}")]
    Fork(String),
    #[error("Audit error: {0}")]
    Audit(String),
    #[error("Config error: {0}")]
    Config(String),
}

impl From<nexo_fork::ForkError> for AutoDreamError {
    fn from(e: nexo_fork::ForkError) -> Self {
        Self::Fork(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_lock_blocked() {
        let e = AutoDreamError::LockBlocked {
            pid: 1234,
            mtime_secs: 60,
        };
        assert!(format!("{e}").contains("1234"));
        assert!(format!("{e}").contains("60s"));
    }

    #[test]
    fn from_fork_error() {
        let fe = nexo_fork::ForkError::Aborted;
        let e: AutoDreamError = fe.into();
        assert!(matches!(e, AutoDreamError::Fork(_)));
    }
}
