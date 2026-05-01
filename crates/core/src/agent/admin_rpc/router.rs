//! Phase 82.10.h.b.3 — concrete `AdminRouter` implementation
//! that bridges the extension-host stdio reader to the
//! [`AdminRpcDispatcher`].
//!
//! Lifecycle:
//! 1. Microapp emits a JSON-RPC request frame on its stdout
//!    with `id = "app:<uuid>"`.
//! 2. The extension-host reader_task (in
//!    `nexo_extensions::runtime::stdio`) detects the `app:`
//!    prefix and forwards the raw line to
//!    [`AdminRouter::handle_frame`].
//! 3. This adapter parses `method` + `params` + `id`, calls
//!    [`AdminRpcDispatcher::dispatch`], frames the result as a
//!    JSON-RPC response, and writes it back through the
//!    extension's stdin via the [`AdminOutboundWriter`].
//!
//! All errors are logged at `warn` and never propagated — the
//! reader_task contract requires `handle_frame` to be infallible
//! so a bad admin frame can't stall the regular `tools/call`
//! flow on the same stdio.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_extensions::runtime::admin_router::AdminRouter;
use serde_json::Value;

use super::dispatcher::{AdminRpcDispatcher, AdminRpcResult};

/// Handle to the extension's stdin used to send admin RPC
/// responses back to the microapp. Implementations are typically
/// thin wrappers around `tokio::sync::mpsc::Sender<String>` —
/// the same outbox the host already uses for daemon-initiated
/// `tools/call` writes.
#[async_trait]
pub trait AdminOutboundWriter: Send + Sync + std::fmt::Debug {
    /// Send one fully-framed JSON-RPC response line (no trailing
    /// newline; the writer adds framing). Errors are logged
    /// internally — admin response delivery is best-effort.
    async fn send(&self, line: String);
}

/// `AdminRouter` impl that delegates to a single dispatcher and
/// echoes responses back through one outbound writer. Boot wires
/// one of these per microapp at spawn time and passes it via
/// `StdioSpawnOptions::admin_router`.
#[derive(Clone)]
pub struct DispatcherAdminRouter {
    dispatcher: Arc<AdminRpcDispatcher>,
    writer: Arc<dyn AdminOutboundWriter>,
}

impl std::fmt::Debug for DispatcherAdminRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatcherAdminRouter")
            .field("dispatcher", &self.dispatcher)
            .finish_non_exhaustive()
    }
}

impl DispatcherAdminRouter {
    /// Build a router pinned to one dispatcher + one outbound
    /// writer.
    pub fn new(
        dispatcher: Arc<AdminRpcDispatcher>,
        writer: Arc<dyn AdminOutboundWriter>,
    ) -> Self {
        Self { dispatcher, writer }
    }
}

#[async_trait]
impl AdminRouter for DispatcherAdminRouter {
    async fn handle_frame(&self, extension_id: &str, line: String) {
        // Parse — defensive: bad JSON or wrong shape gets logged
        // and dropped without crashing the reader.
        let frame: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    ext = %extension_id,
                    error = %e,
                    "admin frame is not valid json; dropping",
                );
                return;
            }
        };

        let id = frame.get("id").cloned().unwrap_or(Value::Null);
        let method = match frame.get("method").and_then(Value::as_str) {
            Some(m) => m.to_string(),
            None => {
                // It's a response, not a request — i.e. the
                // microapp answered a daemon-initiated call.
                // v1 doesn't issue any such admin-shaped calls,
                // so log + drop.
                tracing::debug!(
                    ext = %extension_id,
                    "admin frame has no method (response shape); dropping",
                );
                return;
            }
        };
        let params = frame.get("params").cloned().unwrap_or(Value::Null);

        // Dispatch — never panics; returns either result or error.
        let result = self.dispatcher.dispatch(extension_id, &method, params).await;
        let response = frame_response(id, result);
        let line = match serde_json::to_string(&response) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    ext = %extension_id,
                    method = %method,
                    error = %e,
                    "failed to serialize admin response; dropping",
                );
                return;
            }
        };
        self.writer.send(line).await;
    }
}

/// Frame an [`AdminRpcResult`] as the JSON-RPC 2.0 response
/// envelope expected by the SDK side.
fn frame_response(id: Value, result: AdminRpcResult) -> Value {
    match (result.result, result.error) {
        (Some(value), _) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": value,
        }),
        (None, Some(err)) => {
            let mut error = serde_json::json!({
                "code": err.code(),
                "message": format!("{err}"),
            });
            if let Some(data) = err.data() {
                error["data"] = data;
            }
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": error,
            })
        }
        (None, None) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": Value::Null,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    use crate::agent::admin_rpc::CapabilitySet;

    /// Test-only outbound writer that captures every sent line.
    #[derive(Debug, Default)]
    struct CapturingWriter {
        sent: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl AdminOutboundWriter for CapturingWriter {
        async fn send(&self, line: String) {
            self.sent.lock().unwrap().push(line);
        }
    }

    fn build_dispatcher_granting(microapp_id: &str, caps: &[&str]) -> AdminRpcDispatcher {
        let mut grants = HashMap::new();
        grants.insert(
            microapp_id.to_string(),
            caps.iter().map(|s| s.to_string()).collect::<HashSet<_>>(),
        );
        AdminRpcDispatcher::new().with_capabilities(CapabilitySet::from_grants(grants))
    }

    #[tokio::test]
    async fn handle_frame_dispatches_and_writes_response() {
        let dispatcher = Arc::new(build_dispatcher_granting("agent-creator", &["_echo"]));
        let writer = Arc::new(CapturingWriter::default());
        let router = DispatcherAdminRouter::new(dispatcher, writer.clone());

        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "app:abc",
            "method": "nexo/admin/echo",
            "params": { "x": 7 },
        })
        .to_string();
        router.handle_frame("agent-creator", frame).await;

        let sent = writer.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let response: Value = serde_json::from_str(&sent[0]).unwrap();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], "app:abc");
        assert_eq!(response["result"]["echoed"]["x"], 7);
        assert_eq!(response["result"]["microapp_id"], "agent-creator");
    }

    #[tokio::test]
    async fn handle_frame_emits_error_envelope_on_capability_denial() {
        let dispatcher = Arc::new(AdminRpcDispatcher::new());
        let writer = Arc::new(CapturingWriter::default());
        let router = DispatcherAdminRouter::new(dispatcher, writer.clone());

        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "app:def",
            "method": "nexo/admin/echo",
            "params": Value::Null,
        })
        .to_string();
        router.handle_frame("agent-creator", frame).await;

        let sent = writer.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let response: Value = serde_json::from_str(&sent[0]).unwrap();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], "app:def");
        assert_eq!(response["error"]["code"], -32004);
        assert_eq!(response["error"]["data"]["capability"], "_echo");
    }

    #[tokio::test]
    async fn handle_frame_drops_invalid_json_silently() {
        let dispatcher = Arc::new(AdminRpcDispatcher::new());
        let writer = Arc::new(CapturingWriter::default());
        let router = DispatcherAdminRouter::new(dispatcher, writer.clone());

        router
            .handle_frame("agent-creator", "{not valid json".to_string())
            .await;
        assert!(
            writer.sent.lock().unwrap().is_empty(),
            "no response should be sent for unparseable input"
        );
    }

    #[tokio::test]
    async fn handle_frame_drops_response_shaped_frames() {
        // Response frame (no `method`, has `result`) — admin v1
        // doesn't issue daemon-initiated admin calls so this
        // shape is unexpected and gets dropped, not echoed.
        let dispatcher = Arc::new(AdminRpcDispatcher::new());
        let writer = Arc::new(CapturingWriter::default());
        let router = DispatcherAdminRouter::new(dispatcher, writer.clone());

        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "app:ghi",
            "result": { "ok": true },
        })
        .to_string();
        router.handle_frame("agent-creator", frame).await;
        assert!(writer.sent.lock().unwrap().is_empty());
    }
}
