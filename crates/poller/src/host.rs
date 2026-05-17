//! `PollerHost` — the single interface a `Poller` uses to talk to the
//! outside world. Runner stays agnostic of channels, credentials and
//! broker topics: the host adapter is what knows.
//!
//! Two implementations live in the daemon:
//! - `InProcessHost` — direct call into `AgentCredentialResolver` and
//!   `AnyBroker`. Used by built-in pollers (today only `webhook_poll`).
//! - `PluginHost` — reverse-RPC bridge that forwards calls back to the
//!   daemon from a subprocess poller. Used by plugin v2 pollers.
//!
//! Pollers shipping in extracted repos do not implement this trait
//! directly — they call it via `nexo_microapp_sdk::poller::PollerHandler`
//! whose context type wraps the daemon-side `PollerHost`.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// What the poller asks the runtime. Single point of egress for
/// runtime-level concerns (broker publish, credential lookup,
/// observability). Everything outside this trait is the poller's
/// own business.
#[async_trait]
pub trait PollerHost: Send + Sync + 'static {
    /// Publish a payload to a broker topic. Topic + payload are opaque
    /// to the runner. The daemon enforces the plugin manifest's
    /// `[plugin.capabilities.broker].publish` allowlist downstream of
    /// this call — disallowed topics return [`HostError::TopicNotAllowed`].
    async fn broker_publish(&self, topic: String, payload: Vec<u8>) -> Result<(), HostError>;

    /// Resolve credentials for the given channel under the agent that
    /// owns this tick. The runner does not type-check the returned
    /// JSON — each poller knows its own channel shape (Google calls
    /// expect a token bundle, RSS expects nothing, custom channels
    /// expect whatever the operator wires).
    async fn credentials_get(&self, channel: String) -> Result<Value, HostError>;

    /// Forward a structured log line to the daemon's tracing pipeline.
    /// Fields are merged into the span. Fire-and-forget at runtime,
    /// but kept RPC so the subprocess can detect daemon-side
    /// backpressure rather than silently dropping.
    async fn log(&self, level: LogLevel, message: String, fields: Value) -> Result<(), HostError>;

    /// Increment a named counter in the daemon's Prometheus aggregator.
    /// `labels` is a JSON object whose string fields become label
    /// key/values. Non-string values are coerced to strings; missing
    /// labels default to the empty string.
    async fn metric_inc(&self, name: String, labels: Value) -> Result<(), HostError>;
}

/// What `PollerHost` operations can fail with. Pollers map these into
/// `PollerError::Transient` (network/IO) or `PollerError::Permanent`
/// (auth, allowlist, malformed config) depending on the operation.
#[derive(Debug, Error)]
pub enum HostError {
    /// Daemon refused the publish because the topic is not in the
    /// plugin manifest's `[plugin.capabilities.broker].publish` set.
    #[error("topic '{0}' not in plugin publish allowlist")]
    TopicNotAllowed(String),

    /// Agent has no credentials bound for the requested channel.
    #[error("no credentials bound for channel '{0}'")]
    CredentialsMissing(String),

    /// Broker connection lost or daemon stdio pipe closed.
    #[error("broker unavailable: {0}")]
    BrokerUnavailable(String),

    /// Reverse-RPC call timed out waiting for daemon reply.
    #[error("daemon RPC timed out after {0:?}")]
    Timeout(Duration),

    /// Daemon-side handler returned a structured RPC error not covered
    /// by the typed variants above.
    #[error("rpc error: code={code} msg={message}")]
    Rpc { code: i32, message: String },

    /// Anything else — typically serialization, IO, or protocol-level.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl HostError {
    /// Map to a `PollerError` classification hint. Pollers do not have
    /// to use this — they may want different semantics per operation.
    pub fn into_poller_kind(self) -> HostErrorKind {
        match self {
            Self::TopicNotAllowed(_) | Self::CredentialsMissing(_) => HostErrorKind::Permanent,
            Self::BrokerUnavailable(_) | Self::Timeout(_) | Self::Other(_) => {
                HostErrorKind::Transient
            }
            Self::Rpc { code, .. } => match code {
                -32002 => HostErrorKind::Permanent,
                -32602 => HostErrorKind::Config,
                _ => HostErrorKind::Transient,
            },
        }
    }
}

/// Hint used by [`HostError::into_poller_kind`] so callers can wrap
/// host failures into the matching `PollerError` variant.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HostErrorKind {
    Transient,
    Permanent,
    Config,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// What the poller returns on a successful tick. Compared to the
/// pre-Phase-96 `TickOutcome` this drops the typed `deliver` list —
/// outbound is the poller's responsibility, not the runner's.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickAck {
    /// Opaque bytes persisted as the next tick's cursor. `None` means
    /// "keep the previous cursor unchanged"; `Some(empty)` resets.
    pub next_cursor: Option<Vec<u8>>,
    /// Override the next interval just for the upcoming slot. `None`
    /// honors the configured schedule.
    pub next_interval_hint: Option<Duration>,
    /// Pure telemetry. `items_seen` is what the source returned, even
    /// if dedup or filters dropped most of them; `items_dispatched`
    /// is the actually-acted-on subset.
    pub metrics: Option<TickMetrics>,
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickMetrics {
    pub items_seen: u32,
    pub items_dispatched: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync + ?Sized>() {}

    #[test]
    fn poller_host_is_object_safe_and_send_sync() {
        assert_send_sync::<dyn PollerHost>();
    }

    #[test]
    fn tick_ack_default_is_empty() {
        let a = TickAck::default();
        assert!(a.next_cursor.is_none());
        assert!(a.next_interval_hint.is_none());
        assert!(a.metrics.is_none());
    }

    #[test]
    fn host_error_classify_topic_not_allowed_is_permanent() {
        let e = HostError::TopicNotAllowed("plugin.outbound.x".into());
        assert_eq!(e.into_poller_kind(), HostErrorKind::Permanent);
    }

    #[test]
    fn host_error_classify_creds_missing_is_permanent() {
        let e = HostError::CredentialsMissing("google".into());
        assert_eq!(e.into_poller_kind(), HostErrorKind::Permanent);
    }

    #[test]
    fn host_error_classify_timeout_is_transient() {
        let e = HostError::Timeout(Duration::from_secs(5));
        assert_eq!(e.into_poller_kind(), HostErrorKind::Transient);
    }

    #[test]
    fn host_error_classify_rpc_code_routes_correctly() {
        let permanent = HostError::Rpc {
            code: -32002,
            message: "revoked".into(),
        };
        assert_eq!(permanent.into_poller_kind(), HostErrorKind::Permanent);

        let config = HostError::Rpc {
            code: -32602,
            message: "missing field".into(),
        };
        assert_eq!(config.into_poller_kind(), HostErrorKind::Config);

        let other = HostError::Rpc {
            code: -1,
            message: "unknown".into(),
        };
        assert_eq!(other.into_poller_kind(), HostErrorKind::Transient);
    }

    #[test]
    fn log_level_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&LogLevel::Warn).unwrap(),
            "\"warn\""
        );
    }
}
