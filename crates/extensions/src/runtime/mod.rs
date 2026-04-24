//! Extension runtime — transports that actually execute an extension.
//!
//! 11.3 ships the stdio transport. 11.4 will add NATS.

use std::time::Duration;

use thiserror::Error;

pub mod announce;
pub mod directory;
pub mod nats;
pub mod stdio;
pub mod transport;
pub mod wire;

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
