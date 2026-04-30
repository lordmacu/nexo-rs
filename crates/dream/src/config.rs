//! AutoDream configuration.
//!
//! The struct lives in `nexo-config::types::dream` so `AgentConfig`
//! can embed it without dep cycles. This module re-exports for
//! ergonomic operator imports + adds the validation helper that
//! requires `AutoDreamError` (a `nexo-dream`-side type).

use std::time::Duration;

pub use nexo_config::types::dream::AutoDreamConfig;

use crate::error::AutoDreamError;

/// Validate constraints. Defensive — invalid configs fail at boot
/// rather than firing weird behavior at run time. Lives here (not
/// on the config struct itself) because it returns
/// [`AutoDreamError`].
pub fn validate(cfg: &AutoDreamConfig) -> Result<(), AutoDreamError> {
    if cfg.min_hours < Duration::from_secs(60 * 60) {
        return Err(AutoDreamError::Config(
            "min_hours must be >= 1h".into(),
        ));
    }
    if cfg.min_sessions == 0 {
        return Err(AutoDreamError::Config(
            "min_sessions must be >= 1".into(),
        ));
    }
    if cfg.fork_timeout < Duration::from_secs(10) {
        return Err(AutoDreamError::Config(
            "fork_timeout must be >= 10s".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_defaults() {
        validate(&AutoDreamConfig::default()).unwrap();
    }

    #[test]
    fn validate_rejects_min_hours_too_low() {
        let mut c = AutoDreamConfig::default();
        c.min_hours = Duration::from_secs(60);
        assert!(validate(&c).is_err());
    }

    #[test]
    fn validate_rejects_min_sessions_zero() {
        let mut c = AutoDreamConfig::default();
        c.min_sessions = 0;
        assert!(validate(&c).is_err());
    }

    #[test]
    fn validate_rejects_fork_timeout_too_low() {
        let mut c = AutoDreamConfig::default();
        c.fork_timeout = Duration::from_secs(1);
        assert!(validate(&c).is_err());
    }
}
