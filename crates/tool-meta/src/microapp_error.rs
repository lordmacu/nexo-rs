//! Phase 83.16 — `nexo/notify/microapp_error` wire shape.
//!
//! When a microapp subprocess misbehaves, the daemon's stdio
//! supervisor emits a structured notification on the broker so
//! operators get an actionable signal in admin-ui, Prometheus,
//! and the audit log — instead of having to tail the
//! microapp's stderr.
//!
//! Four canonical error kinds (forward-compat with `#[non_exhaustive]`):
//!
//! | Kind | When the daemon emits |
//! |---|---|
//! | `InitTimeout` | `initialize` did not return within timeout |
//! | `HandlerPanic` | `tools/call` returned `error.data.unhandled = true` |
//! | `Exit` | Subprocess exited with non-zero code |
//! | `CapabilityDenied` | Microapp called an admin RPC it does not hold |
//!
//! The notification is `{"jsonrpc":"2.0","method":
//! "nexo/notify/microapp_error","params":<MicroappError>}`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Method string for the JSON-RPC notification.
pub const MICROAPP_ERROR_NOTIFY_METHOD: &str = "nexo/notify/microapp_error";

/// Kind discriminator. `#[non_exhaustive]` so future kinds
/// (e.g. `RateLimitTripped` once 83.16's respawn-throttle
/// follow-up ships) can land non-major.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicroappErrorKind {
    /// Microapp didn't reply to `initialize` within the
    /// configured timeout.
    InitTimeout,
    /// `tools/call` returned an error with
    /// `data.unhandled = true` — i.e. a panic / unhandled
    /// exception inside the microapp's handler.
    HandlerPanic,
    /// Subprocess exited with a non-zero exit code (or
    /// SIGSEGV / SIGTERM / etc).
    Exit,
    /// Microapp issued an admin-RPC call (`nexo/admin/*`)
    /// that it does not hold the capability for.
    CapabilityDenied,
}

impl MicroappErrorKind {
    /// Stable wire string. Matches `serde(rename_all =
    /// "snake_case")` so admin-ui / Prometheus labels stay
    /// consistent.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            MicroappErrorKind::InitTimeout => "init_timeout",
            MicroappErrorKind::HandlerPanic => "handler_panic",
            MicroappErrorKind::Exit => "exit",
            MicroappErrorKind::CapabilityDenied => "capability_denied",
        }
    }
}

/// Notification payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicroappError {
    /// Operator-facing extension id (`extensions.yaml.entries.<id>`).
    pub microapp_id: String,
    /// Discriminator for the four canonical error categories.
    pub kind: MicroappErrorKind,
    /// JSON-RPC `id` that triggered the error, when applicable.
    /// `None` for `Exit` (no in-flight call) and for boot-time
    /// failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// UTC wall-clock at which the daemon observed the error.
    pub occurred_at: DateTime<Utc>,
    /// One-line human-readable summary surfaced in the admin-ui
    /// hover tooltip and Prometheus annotations. Keep terse —
    /// stack trace goes in `stack_trace`.
    pub summary: String,
    /// Optional multi-line stack trace / traceback. Bounded by
    /// the daemon's stdio reader (16 KiB cap by convention) so
    /// a runaway microapp can't blow up the broker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<String>,
}

impl MicroappError {
    /// Convenience: build a notification with `Utc::now()` as
    /// the timestamp.
    pub fn new(
        microapp_id: impl Into<String>,
        kind: MicroappErrorKind,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            microapp_id: microapp_id.into(),
            kind,
            correlation_id: None,
            occurred_at: Utc::now(),
            summary: summary.into(),
            stack_trace: None,
        }
    }

    /// Set the JSON-RPC `id` correlation. Use the `app:<uuid>`
    /// id when reporting failures of microapp-initiated outbound
    /// calls.
    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = Some(id.into());
        self
    }

    /// Attach a multi-line stack trace. Daemon caps it at 16 KiB
    /// before broadcasting on the broker.
    pub fn with_stack_trace(mut self, trace: impl Into<String>) -> Self {
        self.stack_trace = Some(trace.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_method_constant_is_stable() {
        // Operators key admin-ui subscriptions on this string —
        // changing it is a breaking wire change.
        assert_eq!(
            MICROAPP_ERROR_NOTIFY_METHOD,
            "nexo/notify/microapp_error"
        );
    }

    #[test]
    fn kind_serialises_snake_case() {
        let json = serde_json::to_string(&MicroappErrorKind::InitTimeout).unwrap();
        assert_eq!(json, "\"init_timeout\"");
        let json = serde_json::to_string(&MicroappErrorKind::HandlerPanic).unwrap();
        assert_eq!(json, "\"handler_panic\"");
        let json = serde_json::to_string(&MicroappErrorKind::CapabilityDenied)
            .unwrap();
        assert_eq!(json, "\"capability_denied\"");
        let json = serde_json::to_string(&MicroappErrorKind::Exit).unwrap();
        assert_eq!(json, "\"exit\"");
    }

    #[test]
    fn kind_wire_str_matches_serde() {
        for k in [
            MicroappErrorKind::InitTimeout,
            MicroappErrorKind::HandlerPanic,
            MicroappErrorKind::Exit,
            MicroappErrorKind::CapabilityDenied,
        ] {
            let serde_json = serde_json::to_string(&k).unwrap();
            // serde_json wraps in quotes; strip them.
            let trimmed = serde_json.trim_matches('"');
            assert_eq!(trimmed, k.as_wire_str());
        }
    }

    #[test]
    fn microapp_error_round_trips_full_payload() {
        let err = MicroappError {
            microapp_id: "agent-creator".into(),
            kind: MicroappErrorKind::HandlerPanic,
            correlation_id: Some("12".into()),
            occurred_at: chrono::DateTime::parse_from_rfc3339(
                "2026-05-02T12:00:00Z",
            )
            .unwrap()
            .with_timezone(&Utc),
            summary: "tool 'register' panicked: index out of bounds".into(),
            stack_trace: Some("at handler.rs:42\nat dispatch.rs:118".into()),
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: MicroappError = serde_json::from_str(&json).unwrap();
        assert_eq!(back, err);
    }

    #[test]
    fn microapp_error_optional_fields_skip_when_none() {
        let err = MicroappError::new(
            "agent-creator",
            MicroappErrorKind::Exit,
            "exit code 137 (SIGKILL)",
        );
        let json = serde_json::to_string(&err).unwrap();
        assert!(!json.contains("correlation_id"));
        assert!(!json.contains("stack_trace"));
    }

    #[test]
    fn builder_chain_sets_optional_fields() {
        let err = MicroappError::new(
            "x",
            MicroappErrorKind::CapabilityDenied,
            "needs agents_crud",
        )
        .with_correlation_id("app:abc-123")
        .with_stack_trace("at admin.rs:99");
        assert_eq!(err.correlation_id.as_deref(), Some("app:abc-123"));
        assert_eq!(err.stack_trace.as_deref(), Some("at admin.rs:99"));
    }
}
