//! Phase 82.10 — admin RPC client for microapps.
//!
//! Inverse-direction JSON-RPC: microapp invokes admin methods on
//! the daemon (`nexo/admin/<domain>/<method>`). Reuses the same
//! stdio transport that delivers `tools/call` requests, with
//! `app:` ID prefix to disambiguate microapp-initiated requests
//! from daemon-initiated ones.
//!
//! Capability gate: every admin call requires the matching
//! capability declared in `plugin.toml [capabilities.admin]` AND
//! granted by the operator in `extensions.yaml.<id>.capabilities_grant`.
//! Missing → `AdminError::CapabilityNotGranted`.
//!
//! Sub-modules layer ergonomic helpers on top of the generic
//! [`AdminClient`]:
//! - [`takeover`] — Phase 83.8.6 `HumanTakeover` engage / send /
//!   release.

pub mod takeover;
pub use takeover::{HumanTakeover, SendReplyArgs};

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Default timeout for admin RPC round-trips. Mirrors the daemon
/// pending-request timeout from the Phase 82.10 spec.
pub const DEFAULT_ADMIN_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);

/// Typed errors a microapp sees when calling admin methods.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum AdminError {
    /// JSON-RPC `-32004`: the daemon refused the call because the
    /// operator did not grant `capability` to this microapp.
    #[error("capability_not_granted: {capability} for method {method}")]
    CapabilityNotGranted {
        /// Required capability name (e.g. `agents_crud`).
        capability: String,
        /// Daemon-side method that was rejected.
        method: String,
    },
    /// JSON-RPC `-32602`: caller-supplied params were invalid.
    #[error("invalid_params: {0}")]
    InvalidParams(String),
    /// JSON-RPC `-32601`: domain method not found / disabled by
    /// INVENTORY env toggle.
    #[error("method_not_found: {0}")]
    MethodNotFound(String),
    /// Resource missing (admin domain returned `NotFound`).
    #[error("not_found: {0}")]
    NotFound(String),
    /// Daemon-side internal error. Includes JSON-RPC `-32603`
    /// fallback when no other variant fits.
    #[error("internal: {0}")]
    Internal(String),
    /// Wire-shape problem: stdout write failed, stdin closed,
    /// response timed out, etc.
    #[error("transport: {0}")]
    Transport(String),
}

impl AdminError {
    /// Build an `AdminError` from a JSON-RPC error frame
    /// (`{"code": -32004, "message": "...", "data": {...}}`).
    pub fn from_rpc_error(error: &Value) -> Self {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32603);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("(no message)")
            .to_string();
        let data = error.get("data");
        match code {
            -32004 => {
                let capability = data
                    .and_then(|d| d.get("capability"))
                    .and_then(Value::as_str)
                    .unwrap_or("(unknown)")
                    .to_string();
                let method = data
                    .and_then(|d| d.get("method"))
                    .and_then(Value::as_str)
                    .unwrap_or("(unknown)")
                    .to_string();
                AdminError::CapabilityNotGranted { capability, method }
            }
            -32602 => AdminError::InvalidParams(message),
            -32601 => {
                if message.contains("not_found") {
                    AdminError::NotFound(message)
                } else {
                    AdminError::MethodNotFound(message)
                }
            }
            _ => AdminError::Internal(message),
        }
    }
}

/// Transport adapter the [`AdminClient`] uses to send a JSON-RPC
/// request line out to the daemon. Implementations write the line
/// to whatever sink the SDK runtime is plumbed to (stdout in
/// production; an in-memory channel in tests).
#[async_trait]
pub trait AdminSender: Send + Sync + std::fmt::Debug {
    /// Write a single JSON-RPC frame (without trailing newline —
    /// the implementation appends it). Returns transport error
    /// if the sink is closed / disconnected.
    async fn send_line(&self, line: String) -> Result<(), AdminError>;
}

/// Microapp-side admin RPC client.
///
/// Construct via the SDK's `Microapp` builder once the `admin`
/// feature is enabled; the SDK runtime injects an `AdminSender`
/// that shares the stdio writer with `tools/call` responses.
///
/// Each call generates a new `app:<uuid-v7>` request id, registers
/// a oneshot receiver in the pending map, writes the JSON-RPC
/// frame, and awaits the response (default 30s timeout).
#[derive(Debug, Clone)]
pub struct AdminClient {
    sender: Arc<dyn AdminSender>,
    pending: Arc<DashMap<String, oneshot::Sender<Result<Value, AdminError>>>>,
    timeout: std::time::Duration,
}

impl AdminClient {
    /// Build a client with the default 30 s timeout.
    pub fn new(sender: Arc<dyn AdminSender>) -> Self {
        Self::with_timeout(sender, DEFAULT_ADMIN_TIMEOUT)
    }

    /// Build with a custom timeout. Tests use sub-second values.
    pub fn with_timeout(sender: Arc<dyn AdminSender>, timeout: std::time::Duration) -> Self {
        Self {
            sender,
            pending: Arc::new(DashMap::new()),
            timeout,
        }
    }

    /// Generate the next `app:<uuid>` id. Public so the SDK
    /// dispatch loop can detect microapp-initiated correlations.
    pub fn next_request_id() -> String {
        format!("app:{}", Uuid::new_v4())
    }

    /// Generic typed call — used by domain-specific helpers (added
    /// in subsequent sub-phases). Sends `nexo/admin/<method>`,
    /// awaits the response, deserialises the `result` field.
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, AdminError> {
        let raw = self.call_raw(method, serde_json::to_value(&params).map_err(|e| {
            AdminError::InvalidParams(format!("serialize params: {e}"))
        })?).await?;
        serde_json::from_value(raw).map_err(|e| {
            AdminError::Internal(format!("deserialise result: {e}"))
        })
    }

    /// Lower-level call that returns the raw `result` JSON value
    /// without typed deserialisation. Useful for tests + future
    /// streaming surfaces.
    pub async fn call_raw(&self, method: &str, params: Value) -> Result<Value, AdminError> {
        let id = Self::next_request_id();
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id.clone(), tx);

        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let line = serde_json::to_string(&frame).map_err(|e| {
            AdminError::Internal(format!("serialize frame: {e}"))
        })?;

        if let Err(e) = self.sender.send_line(line).await {
            self.pending.remove(&id);
            return Err(e);
        }

        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                self.pending.remove(&id);
                Err(AdminError::Transport("response channel closed".into()))
            }
            Err(_) => {
                self.pending.remove(&id);
                Err(AdminError::Transport(format!(
                    "timeout after {:?} waiting for {method}",
                    self.timeout
                )))
            }
        }
    }

    /// Dispatch loop hook — called by the SDK runtime when a frame
    /// arrives on stdin whose `id` carries the `app:` prefix.
    /// Looks up the pending oneshot and fires it with the parsed
    /// result/error. Returns `true` if the id was actually
    /// pending (caller can log the unmatched-response case).
    pub fn on_inbound_response(&self, id: &str, frame: &Value) -> bool {
        let Some((_, tx)) = self.pending.remove(id) else {
            return false;
        };
        let payload = if let Some(error) = frame.get("error") {
            Err(AdminError::from_rpc_error(error))
        } else {
            Ok(frame.get("result").cloned().unwrap_or(Value::Null))
        };
        // Receiver may have dropped (caller cancelled / timed out).
        // Discard the send error — the timeout path already
        // surfaced the situation to the caller.
        let _ = tx.send(payload);
        true
    }

    /// Pending request count — observability only.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Test-only sender that captures sent lines into an internal
    /// buffer. The harness then synthesises a response by calling
    /// `client.on_inbound_response`.
    #[derive(Debug, Default, Clone)]
    struct CaptureSender {
        lines: Arc<StdMutex<Vec<String>>>,
    }

    #[async_trait]
    impl AdminSender for CaptureSender {
        async fn send_line(&self, line: String) -> Result<(), AdminError> {
            self.lines.lock().unwrap().push(line);
            Ok(())
        }
    }

    /// Test-only sender that always errors out — exercises the
    /// `Transport` error path.
    #[derive(Debug, Default)]
    struct FailingSender;

    #[async_trait]
    impl AdminSender for FailingSender {
        async fn send_line(&self, _line: String) -> Result<(), AdminError> {
            Err(AdminError::Transport("simulated stdin closed".into()))
        }
    }

    #[tokio::test]
    async fn admin_client_round_trip_via_capture_sender() {
        let sender = Arc::new(CaptureSender::default());
        let client = AdminClient::new(sender.clone());

        // Spawn a task that will call `call_raw` and resolve the
        // pending response by inspecting the captured frame.
        let client_for_call = client.clone();
        let call = tokio::spawn(async move {
            client_for_call
                .call_raw("nexo/admin/echo", serde_json::json!({ "x": 1 }))
                .await
        });

        // Wait for the request to land in the capture buffer.
        for _ in 0..100 {
            if !sender.lines.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let captured = sender.lines.lock().unwrap().first().cloned().expect("frame written");
        let frame: Value = serde_json::from_str(&captured).unwrap();
        assert_eq!(frame["method"], "nexo/admin/echo");
        assert!(frame["id"].as_str().unwrap().starts_with("app:"));

        // Synthesise the daemon's response.
        let id = frame["id"].as_str().unwrap().to_string();
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "echoed": { "x": 1 } }
        });
        assert!(client.on_inbound_response(&id, &response));

        let result = call.await.unwrap().unwrap();
        assert_eq!(result["echoed"]["x"], 1);
    }

    #[tokio::test]
    async fn admin_client_capability_not_granted_maps_to_typed_error() {
        let sender = Arc::new(CaptureSender::default());
        let client = AdminClient::with_timeout(sender.clone(), std::time::Duration::from_secs(2));

        let client_for_call = client.clone();
        let call = tokio::spawn(async move {
            client_for_call
                .call_raw("nexo/admin/agents/list", serde_json::json!({}))
                .await
        });

        for _ in 0..100 {
            if !sender.lines.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let id = serde_json::from_str::<Value>(
            &sender.lines.lock().unwrap().first().cloned().unwrap(),
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        let error_frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32004,
                "message": "capability_not_granted",
                "data": {
                    "capability": "agents_crud",
                    "microapp_id": "agent-creator",
                    "method": "nexo/admin/agents/list"
                }
            }
        });
        client.on_inbound_response(&id, &error_frame);

        let err = call.await.unwrap().unwrap_err();
        match err {
            AdminError::CapabilityNotGranted { capability, method } => {
                assert_eq!(capability, "agents_crud");
                assert_eq!(method, "nexo/admin/agents/list");
            }
            other => panic!("expected CapabilityNotGranted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn admin_client_transport_error_returned_when_send_fails() {
        let client = AdminClient::new(Arc::new(FailingSender));
        let err = client
            .call_raw("nexo/admin/echo", Value::Null)
            .await
            .unwrap_err();
        assert!(matches!(err, AdminError::Transport(_)));
        assert_eq!(client.pending_len(), 0, "pending entry cleared on send fail");
    }

    #[test]
    fn next_request_id_uses_app_prefix() {
        let id = AdminClient::next_request_id();
        assert!(id.starts_with("app:"));
        // UUID v4 portion is 36 chars.
        assert_eq!(id.len(), "app:".len() + 36);
    }

    #[test]
    fn from_rpc_error_falls_back_to_internal_for_unknown_code() {
        let err = AdminError::from_rpc_error(&serde_json::json!({
            "code": -99999,
            "message": "alien error"
        }));
        match err {
            AdminError::Internal(m) => assert!(m.contains("alien error")),
            other => panic!("expected Internal, got {other:?}"),
        }
    }
}
