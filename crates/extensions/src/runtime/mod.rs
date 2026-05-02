//! Extension runtime — transports that actually execute an extension.
//!
//! 11.3 ships the stdio transport. 11.4 will add NATS.

use std::time::Duration;

use thiserror::Error;

pub mod admin_router;
pub mod announce;
pub mod directory;
pub mod nats;
pub mod stdio;
pub mod transport;
pub mod wire;

pub use admin_router::{AdminRouter, SharedAdminRouter};
pub use directory::{DirectoryEntry, DirectoryEvent, ExtensionDirectory, RemovalReason};
pub use nats::{NatsRuntime, NatsRuntimeOptions};
pub use stdio::{StdioRuntime, StdioSpawnOptions};
pub use transport::ExtensionTransport;

/// Phase 11.6 — lifecycle hook names that extensions can subscribe to.
/// Kept small on purpose; OpenClaw has ~29 hooks but most are feature-
/// specific. This list maps to events our framework already emits.
pub const HOOK_NAMES: &[&str] = &[
    "before_message",
    "after_message",
    "before_tool_call",
    "after_tool_call",
];

/// Returns true if `name` is one of the supported hook points.
pub fn is_valid_hook(name: &str) -> bool {
    HOOK_NAMES.contains(&name)
}

/// Response an extension returns from a hook invocation. `abort=true` on a
/// `before_*` hook short-circuits the host event; on `after_*` hooks it is
/// ignored. Missing `reason` is allowed but the host will log a warning.
///
/// Extensions can also rewrite the event payload by returning a
/// non-null `override_event`. The host shallow-merges those keys onto
/// the original event for the next handler (and for the host's
/// consumption of the final event). `abort=true` wins over any
/// `override_event` — aborted handlers don't get to reshape the event.
///
/// Phase 83.3 adds three optional fields used by the vote-to-block /
/// vote-to-transform interceptor path:
/// - `decision`: explicit `"allow" | "block" | "transform"` (the
///   legacy `abort` boolean is still accepted; this field is the
///   richer audit-log signal).
/// - `transformed_body`: rewritten inbound body the agent should
///   see in place of the original (only meaningful when
///   `decision == "transform"`).
/// - `do_not_reply_again`: anti-loop signal — when `true` the host
///   should suppress pending auto-replies for the same conversation.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct HookResponse {
    #[serde(default)]
    pub abort: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Optional JSON object whose keys replace the event's keys for
    /// subsequent handlers. Non-object values are ignored with a warn
    /// (can't merge a scalar into a map).
    #[serde(default, rename = "override", skip_serializing_if = "Option::is_none")]
    pub override_event: Option<serde_json::Value>,

    /// Phase 83.3 — explicit decision discriminator. `None` for
    /// pre-83.3 responses (legacy `abort` boolean is the source of
    /// truth in that case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// Phase 83.3 — rewritten inbound body. Only meaningful when
    /// `decision == Some("transform")`; the host applies the
    /// rewrite (subject to operator policy) and audit-logs the diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transformed_body: Option<String>,
    /// Phase 83.3 — anti-loop signal. `true` means the host should
    /// suppress pending auto-replies for this conversation in
    /// addition to whatever block/transform action was voted for.
    #[serde(default, skip_serializing_if = "is_false")]
    pub do_not_reply_again: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_hook_accepts_known() {
        assert!(is_valid_hook("before_message"));
        assert!(is_valid_hook("after_tool_call"));
    }

    #[test]
    fn is_valid_hook_rejects_unknown() {
        assert!(!is_valid_hook(""));
        assert!(!is_valid_hook("on_heartbeat"));
        assert!(!is_valid_hook("BEFORE_MESSAGE"));
    }

    #[test]
    fn hook_response_defaults_to_continue() {
        let r = HookResponse::default();
        assert!(!r.abort);
        assert!(r.reason.is_none());
        assert!(r.decision.is_none());
        assert!(r.transformed_body.is_none());
        assert!(!r.do_not_reply_again);
    }

    // ── Phase 83.3 — parser tolerates new + legacy shapes ──

    #[test]
    fn hook_response_parses_legacy_abort_shape() {
        let json = r#"{"abort":true,"reason":"spam"}"#;
        let r: HookResponse = serde_json::from_str(json).unwrap();
        assert!(r.abort);
        assert_eq!(r.reason.as_deref(), Some("spam"));
        assert!(r.decision.is_none());
    }

    #[test]
    fn hook_response_parses_phase_83_3_block_decision() {
        let json = r#"{"abort":true,"decision":"block","reason":"anti-loop","do_not_reply_again":true}"#;
        let r: HookResponse = serde_json::from_str(json).unwrap();
        assert!(r.abort);
        assert_eq!(r.decision.as_deref(), Some("block"));
        assert!(r.do_not_reply_again);
    }

    #[test]
    fn hook_response_parses_phase_83_3_transform_decision() {
        let json = r#"{"abort":false,"decision":"transform","transformed_body":"Hasta luego","reason":"opt-out"}"#;
        let r: HookResponse = serde_json::from_str(json).unwrap();
        // Transform does NOT abort dispatch — body just gets
        // rewritten by the host.
        assert!(!r.abort);
        assert_eq!(r.decision.as_deref(), Some("transform"));
        assert_eq!(r.transformed_body.as_deref(), Some("Hasta luego"));
        assert_eq!(r.reason.as_deref(), Some("opt-out"));
    }

    #[test]
    fn hook_response_parses_continue_with_decision_allow() {
        let json = r#"{"abort":false,"decision":"allow"}"#;
        let r: HookResponse = serde_json::from_str(json).unwrap();
        assert!(!r.abort);
        assert_eq!(r.decision.as_deref(), Some("allow"));
    }
}

/// Tool declared by an extension in its handshake response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "empty_object", rename = "input_schema")]
    pub input_schema: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

/// Payload returned by `initialize`.
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct HandshakeInfo {
    #[serde(default)]
    pub server_version: Option<String>,
    #[serde(default)]
    pub tools: Vec<ToolDescriptor>,
    #[serde(default)]
    pub hooks: Vec<String>,
}

/// JSON-RPC 2.0 error object from the extension side.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "code {}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeState {
    Spawning,
    Ready,
    Restarting { attempt: u32 },
    Failed { reason: String },
    Shutdown,
}

#[derive(Debug, Error)]
pub enum StartError {
    #[error("extension transport is not stdio")]
    UnsupportedTransport,
    #[error("spawn failed: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("handshake failed: {0}")]
    Handshake(RpcError),
    #[error("handshake timed out after {0:?}")]
    HandshakeTimeout(Duration),
    #[error("child exited before handshake completed")]
    EarlyExit,
    #[error("invalid handshake payload: {0}")]
    DecodeHandshake(#[source] serde_json::Error),
    #[error("broker error: {0}")]
    Broker(String),
}

#[derive(Debug, Error)]
pub enum CallError {
    #[error("call timed out after {0:?}")]
    Timeout(Duration),
    #[error("extension not ready; state = {0}")]
    NotReady(String),
    #[error("child exited")]
    ChildExited,
    #[error("circuit breaker open for extension")]
    BreakerOpen,
    #[error("transport I/O: {0}")]
    Transport(#[source] std::io::Error),
    #[error("rpc error: {0}")]
    Rpc(RpcError),
    #[error("encode: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("decode: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("broker error: {0}")]
    Broker(String),
}
