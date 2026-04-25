use nexo_auth::Channel;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PollerError {
    #[error("config invalid for job '{job}': {reason}")]
    Config { job: String, reason: String },

    #[error("credentials missing: agent '{agent}' has no '{channel}' bound")]
    CredentialsMissing { agent: String, channel: Channel },

    /// Network blip, 5xx, rate-limit, etc. Counts toward the
    /// consecutive-error backoff but never auto-pauses the job.
    #[error("transient: {0}")]
    Transient(#[source] anyhow::Error),

    /// Token revoked, scope changed, account deleted — the job will
    /// not recover on its own. Runner sets `paused = 1`, fires a
    /// failure alert, and waits for `agent pollers resume <id>`.
    #[error("permanent: {0}")]
    Permanent(#[source] anyhow::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl PollerError {
    pub fn classify(&self) -> ErrorClass {
        match self {
            Self::Config { .. } => ErrorClass::Config,
            Self::CredentialsMissing { .. } => ErrorClass::Permanent,
            Self::Transient(_) => ErrorClass::Transient,
            Self::Permanent(_) => ErrorClass::Permanent,
            Self::Other(_) => ErrorClass::Transient,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    /// Boot-time validation error — bad YAML / missing field.
    Config,
    /// Retry with backoff.
    Transient,
    /// Auto-pause the job.
    Permanent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_dispatch() {
        let e = PollerError::Transient(anyhow::anyhow!("503"));
        assert_eq!(e.classify(), ErrorClass::Transient);

        let e = PollerError::Permanent(anyhow::anyhow!("revoked"));
        assert_eq!(e.classify(), ErrorClass::Permanent);

        let e = PollerError::Config {
            job: "x".into(),
            reason: "missing field".into(),
        };
        assert_eq!(e.classify(), ErrorClass::Config);

        let e = PollerError::CredentialsMissing {
            agent: "ana".into(),
            channel: nexo_auth::handle::GOOGLE,
        };
        // Cred-missing is treated as Permanent — no retry helps.
        assert_eq!(e.classify(), ErrorClass::Permanent);
    }

    #[test]
    fn other_wraps_anyhow_as_transient() {
        let inner: anyhow::Error = anyhow::anyhow!("network glitch");
        let e: PollerError = inner.into();
        assert_eq!(e.classify(), ErrorClass::Transient);
    }
}
