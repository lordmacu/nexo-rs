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

#[cfg(feature = "test-harness")]
pub mod mock;
pub mod runtime_sender;
pub mod takeover;
pub mod transcripts;
#[cfg(feature = "test-harness")]
pub use mock::{MockAdminRpc, MockRequest};
pub use runtime_sender::WriterAdminSender;
pub use takeover::{HumanTakeover, SendReplyArgs};
pub use transcripts::TranscriptStream;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use dashmap::DashMap;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Closure type for the operator-token-hash source. The SDK
/// invokes the closure once per outbound `call()` whose method
/// is in `nexo_tool_meta::admin::operator_stamping::OPERATOR_STAMPED_METHODS`.
/// Closure-based registration lets a microapp plug into a
/// hot-swappable source (e.g. an `ArcSwap<String>`-backed live
/// token state) so post-rotation calls stamp the new identity
/// without re-registration.
pub type OperatorHashSource = Arc<dyn Fn() -> String + Send + Sync>;

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
#[derive(Clone)]
pub struct AdminClient {
    sender: Arc<dyn AdminSender>,
    pending: Arc<DashMap<String, oneshot::Sender<Result<Value, AdminError>>>>,
    timeout: std::time::Duration,
    operator_hash_source: Arc<OnceLock<OperatorHashSource>>,
}

impl std::fmt::Debug for AdminClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminClient")
            .field("sender", &self.sender)
            .field("pending_len", &self.pending.len())
            .field("timeout", &self.timeout)
            .field(
                "operator_hash_source",
                &if self.operator_hash_source.get().is_some() {
                    "Some(<closure>)"
                } else {
                    "None"
                },
            )
            .finish()
    }
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
            operator_hash_source: Arc::new(OnceLock::new()),
        }
    }

    /// Phase 82.10.m — register a closure that produces the
    /// current operator token hash on demand. After registration,
    /// every outbound [`call`](Self::call) whose method is in
    /// [`nexo_tool_meta::admin::operator_stamping::OPERATOR_STAMPED_METHODS`]
    /// has its `params.operator_token_hash` field overwritten by
    /// the closure's return value.
    ///
    /// Override is unconditional (defense-in-depth): a caller-
    /// supplied value is replaced by the authenticated
    /// server-computed value.
    ///
    /// The closure is invoked once per outbound stamped call so a
    /// microapp can plug a hot-swappable identity source (e.g.
    /// `ArcSwap<String>`) and have post-rotation calls stamp the
    /// new identity automatically.
    ///
    /// Set-once: subsequent calls log via `tracing::warn` and
    /// keep the original source. Microapps register inside their
    /// `on_admin_ready` hook, which fires exactly once.
    pub fn set_operator_token_hash<F>(&self, source: F)
    where
        F: Fn() -> String + Send + Sync + 'static,
    {
        let arc: OperatorHashSource = Arc::new(source);
        if self.operator_hash_source.set(arc).is_err() {
            tracing::warn!(
                "AdminClient::set_operator_token_hash called twice; \
                 keeping the source registered first"
            );
        }
    }

    /// Stamp `operator_token_hash` onto `params` if `method` is
    /// in [`OPERATOR_STAMPED_METHODS`] and a source is registered.
    /// No-op when the source is absent, the method is not stamped,
    /// or `params` is not a JSON object.
    fn maybe_stamp_operator(&self, method: &str, params: &mut Value) {
        let Some(source) = self.operator_hash_source.get() else {
            return;
        };
        if !nexo_tool_meta::admin::operator_stamping::is_operator_stamped(method) {
            return;
        }
        let Some(obj) = params.as_object_mut() else {
            return;
        };
        obj.insert(
            "operator_token_hash".to_string(),
            Value::String(source()),
        );
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

        // Phase 82.10.m — transparent operator-hash stamping.
        let mut params = params;
        self.maybe_stamp_operator(method, &mut params);

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

    /// Helper used by stamping tests: drives the request-side
    /// half of the round-trip (capture, parse, return frame +
    /// id) without resolving the response. The synthesise +
    /// pending-resolve path is unused — these tests only assert
    /// on the captured outbound frame.
    async fn drive_send_only(
        client: &AdminClient,
        sender: &CaptureSender,
        method: &str,
        params: Value,
    ) -> Value {
        let client_for_call = client.clone();
        let m = method.to_string();
        let p = params.clone();
        tokio::spawn(async move {
            let _ = client_for_call.call_raw(&m, p).await;
        });
        for _ in 0..200 {
            if !sender.lines.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let line = sender
            .lines
            .lock()
            .unwrap()
            .first()
            .cloned()
            .expect("frame written");
        serde_json::from_str(&line).unwrap()
    }

    /// Phase 82.10.m — without a registered source, params pass
    /// through untouched even for stamped methods.
    #[tokio::test]
    async fn client_without_source_passes_params_through() {
        let sender = Arc::new(CaptureSender::default());
        let client =
            AdminClient::with_timeout(sender.clone(), std::time::Duration::from_millis(50));
        let frame = drive_send_only(
            &client,
            &sender,
            "nexo/admin/processing/pause",
            serde_json::json!({ "scope": "x" }),
        )
        .await;
        assert!(frame["params"]
            .as_object()
            .unwrap()
            .get("operator_token_hash")
            .is_none());
    }

    /// Phase 82.10.m — registered source stamps the field.
    #[tokio::test]
    async fn client_with_source_stamps_processing_pause() {
        let sender = Arc::new(CaptureSender::default());
        let client =
            AdminClient::with_timeout(sender.clone(), std::time::Duration::from_millis(50));
        client.set_operator_token_hash(|| "abc123".to_string());
        let frame = drive_send_only(
            &client,
            &sender,
            "nexo/admin/processing/pause",
            serde_json::json!({ "scope": "x" }),
        )
        .await;
        assert_eq!(frame["params"]["operator_token_hash"], "abc123");
    }

    /// Phase 82.10.m — caller-supplied value is overridden by
    /// the registered source (defense-in-depth).
    #[tokio::test]
    async fn client_with_source_overrides_caller_value() {
        let sender = Arc::new(CaptureSender::default());
        let client =
            AdminClient::with_timeout(sender.clone(), std::time::Duration::from_millis(50));
        client.set_operator_token_hash(|| "trusted".to_string());
        let frame = drive_send_only(
            &client,
            &sender,
            "nexo/admin/processing/pause",
            serde_json::json!({
                "scope": "x",
                "operator_token_hash": "untrusted-from-client"
            }),
        )
        .await;
        assert_eq!(frame["params"]["operator_token_hash"], "trusted");
    }

    /// Phase 82.10.m — non-stamped methods are not modified.
    #[tokio::test]
    async fn client_with_source_skips_non_stamped_methods() {
        let sender = Arc::new(CaptureSender::default());
        let client =
            AdminClient::with_timeout(sender.clone(), std::time::Duration::from_millis(50));
        client.set_operator_token_hash(|| "should-not-leak".to_string());
        let frame = drive_send_only(
            &client,
            &sender,
            "nexo/admin/agents/list",
            serde_json::json!({}),
        )
        .await;
        assert!(frame["params"]
            .as_object()
            .unwrap()
            .get("operator_token_hash")
            .is_none());
    }

    /// Phase 82.10.m — closure invoked once per outbound stamped
    /// call. Counter increments verify hot-swap support.
    #[tokio::test]
    async fn client_source_called_per_request_supports_hot_swap() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let sender = Arc::new(CaptureSender::default());
        let client =
            AdminClient::with_timeout(sender.clone(), std::time::Duration::from_millis(50));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_for_closure = counter.clone();
        client.set_operator_token_hash(move || {
            let n = counter_for_closure.fetch_add(1, Ordering::SeqCst);
            format!("hash-{n}")
        });

        // First call: increment 0 -> 1; stamp = hash-0
        let client_a = client.clone();
        let h1 = tokio::spawn(async move {
            let _ = client_a
                .call_raw(
                    "nexo/admin/processing/pause",
                    serde_json::json!({"scope": "a"}),
                )
                .await;
        });
        // Second call: increment 1 -> 2; stamp = hash-1
        let client_b = client.clone();
        let h2 = tokio::spawn(async move {
            let _ = client_b
                .call_raw(
                    "nexo/admin/processing/resume",
                    serde_json::json!({"scope": "b"}),
                )
                .await;
        });
        let _ = h1.await;
        let _ = h2.await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "closure called once per stamped request"
        );

        // Non-stamped call afterwards: counter unchanged
        let client_c = client.clone();
        let h3 = tokio::spawn(async move {
            let _ = client_c
                .call_raw("nexo/admin/agents/list", serde_json::json!({}))
                .await;
        });
        let _ = h3.await;
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    /// Phase 82.10.m — non-object params don't panic; stamping
    /// no-ops. Daemon serde will reject the malformed frame
    /// downstream (existing behavior, not ours to fix here).
    #[tokio::test]
    async fn client_with_source_skips_when_params_not_object() {
        let sender = Arc::new(CaptureSender::default());
        let client =
            AdminClient::with_timeout(sender.clone(), std::time::Duration::from_millis(50));
        client.set_operator_token_hash(|| "abc".to_string());
        let frame = drive_send_only(
            &client,
            &sender,
            "nexo/admin/processing/pause",
            Value::Null,
        )
        .await;
        assert_eq!(frame["params"], Value::Null);
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
