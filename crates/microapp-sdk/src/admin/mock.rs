//! Phase 83.15.b — programmable `nexo/admin/*` mock for SDK tests.
//!
//! Microapps that consume `ctx.admin().call(...)` can't easily be
//! unit-tested without a live daemon: the production [`AdminClient`]
//! talks JSON-RPC over stdio. This module ships a programmable
//! in-process replacement so tool / hook tests run with synthetic
//! responses and assert on the request shape.
//!
//! Pattern mirrored from OpenClaw
//! `research/extensions/telegram/src/polling-transport-state.test.ts:8-16`
//! (`makeMockTransport()` returns a fake transport with hardcoded
//! responses + a spy that captures interactions for later
//! assertions). Translated to Rust: register handlers per method,
//! capture every request seen, hand callers an [`AdminClient`]
//! bound to the mock for direct injection into the harness.
//!
//! Gated behind the `admin` cargo feature (the surface this mocks)
//! AND the `test-harness` feature (the consumer audience).

#![cfg(feature = "test-harness")]

use std::sync::{Arc, Mutex as StdMutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::{json, Value};

use super::{AdminClient, AdminError, AdminSender};

/// One captured request. Tests assert on the request log to verify
/// their tool actually invoked the expected admin method with the
/// expected params.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockRequest {
    /// `nexo/admin/<domain>/<method>` as received.
    pub method: String,
    /// Raw JSON params block. Test deserialises if it cares about
    /// the shape.
    pub params: Value,
    /// Wall-clock timestamp (ms since epoch) for ordering / latency
    /// assertions.
    pub at_ms: u64,
}

type MockResponder = Arc<dyn Fn(Value) -> Result<Value, AdminError> + Send + Sync>;

/// Programmable mock for the `nexo/admin/*` surface. Build one,
/// register responders per method via [`Self::on`] / [`Self::on_err`]
/// / [`Self::on_with`], then hand [`Self::client`] to the test
/// harness or directly to a [`super::AdminClient`] consumer.
///
/// Methods without a registered responder return
/// `AdminError::MethodNotFound("mock: no handler for <method>")` so
/// the test stack-trace points at missing setup.
#[derive(Clone)]
pub struct MockAdminRpc {
    handlers: Arc<DashMap<String, MockResponder>>,
    requests: Arc<StdMutex<Vec<MockRequest>>>,
    client: AdminClient,
}

impl std::fmt::Debug for MockAdminRpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockAdminRpc")
            .field("registered_methods", &self.handlers.len())
            .field("requests_seen", &self.requests.lock().unwrap().len())
            .finish()
    }
}

impl Default for MockAdminRpc {
    fn default() -> Self {
        Self::new()
    }
}

impl MockAdminRpc {
    /// Build a fresh mock. The internal client is wired so calls
    /// from the microapp surface flow through the registered
    /// handlers immediately.
    pub fn new() -> Self {
        let handlers: Arc<DashMap<String, MockResponder>> = Arc::new(DashMap::new());
        let requests: Arc<StdMutex<Vec<MockRequest>>> = Arc::new(StdMutex::new(Vec::new()));
        // Lazy-bind: the sender needs the client to deliver
        // responses, but the client's ctor needs the sender. Stash
        // the client behind a sync `Mutex<Option<_>>` so the
        // sender's async hot path doesn't risk the
        // `blocking_write` panic that bites tokio's RwLock when
        // called from inside a runtime context.
        let client_back: Arc<StdMutex<Option<AdminClient>>> = Arc::new(StdMutex::new(None));

        let sender = Arc::new(MockAdminSender {
            handlers: Arc::clone(&handlers),
            requests: Arc::clone(&requests),
            client_back: Arc::clone(&client_back),
        });
        let client = AdminClient::new(sender);
        *client_back.lock().unwrap() = Some(client.clone());
        Self {
            handlers,
            requests,
            client,
        }
    }

    /// Register a static `Ok(value)` response for `method`. Replaces
    /// any prior responder for the same method (last write wins —
    /// tests can override per case).
    pub fn on(&self, method: &str, response: Value) -> &Self {
        let frozen = response;
        let resp: MockResponder = Arc::new(move |_params| Ok(frozen.clone()));
        self.handlers.insert(method.to_string(), resp);
        self
    }

    /// Register a static `Err(err)` response. Useful for asserting
    /// the microapp handles `CapabilityNotGranted`, `NotFound`, etc.
    /// `err` is captured by clone; pass an owned `AdminError`.
    pub fn on_err(&self, method: &str, err: AdminError) -> &Self {
        // `AdminError` is `#[non_exhaustive]` + not `Clone`, so
        // capture a snapshot the closure reuses. Each invocation
        // rebuilds the typed variant from the snapshot — costs
        // one match arm per dispatch, not measurable in tests.
        let snapshot = render_error_snapshot(&err);
        let resp: MockResponder = Arc::new(move |_params| Err(rebuild_error(&snapshot)));
        self.handlers.insert(method.to_string(), resp);
        self
    }

    /// Register a closure responder. Receives the request `params`
    /// JSON, returns the typed result. Use this when the response
    /// depends on input or the test wants to count invocations
    /// (closure can capture an `Arc<AtomicUsize>`).
    pub fn on_with<F>(&self, method: &str, handler: F) -> &Self
    where
        F: Fn(Value) -> Result<Value, AdminError> + Send + Sync + 'static,
    {
        let resp: MockResponder = Arc::new(handler);
        self.handlers.insert(method.to_string(), resp);
        self
    }

    /// `AdminClient` wired to this mock. Cheap clone — internal
    /// `Arc`s share state with the mock.
    pub fn client(&self) -> AdminClient {
        self.client.clone()
    }

    /// Snapshot of every request seen so far, in arrival order.
    pub fn requests(&self) -> Vec<MockRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Filter requests by method. Equivalent to
    /// `requests().into_iter().filter(...).collect()` but avoids
    /// the closure noise at the call site.
    pub fn requests_for(&self, method: &str) -> Vec<MockRequest> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == method)
            .cloned()
            .collect()
    }

    /// Drop every captured request. Tests that re-use a single mock
    /// across cases call this between cases instead of building a
    /// fresh mock.
    pub fn reset_requests(&self) {
        self.requests.lock().unwrap().clear();
    }
}

/// Internal: variant-preserving snapshot kept in handler closures
/// (and again at outbound-frame build time), since `AdminError`
/// is `#[non_exhaustive]` and not `Clone`. Matches the wire shape
/// emitted by the production daemon dispatcher so the mock and
/// real path are byte-identical from the caller's POV.
#[derive(Clone, Debug)]
enum ErrSnapshot {
    CapabilityNotGranted { capability: String, method: String },
    InvalidParams(String),
    MethodNotFound(String),
    NotFound(String),
    Internal(String),
    Transport(String),
}

fn render_error_snapshot(e: &AdminError) -> ErrSnapshot {
    match e {
        AdminError::CapabilityNotGranted { capability, method } => {
            ErrSnapshot::CapabilityNotGranted {
                capability: capability.clone(),
                method: method.clone(),
            }
        }
        AdminError::InvalidParams(m) => ErrSnapshot::InvalidParams(m.clone()),
        AdminError::MethodNotFound(m) => ErrSnapshot::MethodNotFound(m.clone()),
        AdminError::NotFound(m) => ErrSnapshot::NotFound(m.clone()),
        AdminError::Internal(m) => ErrSnapshot::Internal(m.clone()),
        AdminError::Transport(m) => ErrSnapshot::Transport(m.clone()),
    }
}

fn rebuild_error(snap: &ErrSnapshot) -> AdminError {
    match snap {
        ErrSnapshot::CapabilityNotGranted { capability, method } => {
            AdminError::CapabilityNotGranted {
                capability: capability.clone(),
                method: method.clone(),
            }
        }
        ErrSnapshot::InvalidParams(m) => AdminError::InvalidParams(m.clone()),
        ErrSnapshot::MethodNotFound(m) => AdminError::MethodNotFound(m.clone()),
        ErrSnapshot::NotFound(m) => AdminError::NotFound(m.clone()),
        ErrSnapshot::Internal(m) => AdminError::Internal(m.clone()),
        ErrSnapshot::Transport(m) => AdminError::Transport(m.clone()),
    }
}

/// Render an error snapshot as the JSON-RPC `error` object the
/// daemon would emit on the wire. Mirrors
/// `from_rpc_error`'s parser so the round-trip
/// `AdminError → wire frame → AdminError` lands on the original
/// variant (and `data` carries fields the parser needs).
fn snapshot_to_wire(snap: &ErrSnapshot) -> Value {
    match snap {
        ErrSnapshot::CapabilityNotGranted { capability, method } => json!({
            "code": -32004,
            "message": format!("capability_not_granted: {capability} for method {method}"),
            "data": { "capability": capability, "method": method },
        }),
        ErrSnapshot::InvalidParams(m) => json!({
            "code": -32602,
            "message": format!("invalid_params: {m}"),
        }),
        ErrSnapshot::MethodNotFound(m) => json!({
            // Match production dispatcher's wire form: the
            // message must NOT contain the substring "not_found"
            // because `from_rpc_error` peeks at message text to
            // disambiguate -32601 between MethodNotFound and
            // NotFound. Production stamps `"no admin handler
            // registered for ..."` for the same reason.
            "code": -32601,
            "message": format!("no admin handler registered: {m}"),
        }),
        ErrSnapshot::NotFound(m) => json!({
            "code": -32601,
            "message": format!("not_found: {m}"),
        }),
        ErrSnapshot::Internal(m) => json!({
            "code": -32603,
            "message": format!("internal: {m}"),
        }),
        ErrSnapshot::Transport(m) => json!({
            "code": -32603,
            "message": format!("transport: {m}"),
        }),
    }
}

/// `AdminSender` impl that intercepts every outbound frame, looks
/// up a registered responder by method, builds the JSON-RPC reply
/// frame, and feeds it back to the bound `AdminClient` via
/// `on_inbound_response`. Lives behind `Arc` so it stays cheap to
/// share between the client and the `MockAdminRpc` handle.
#[derive(Clone)]
struct MockAdminSender {
    handlers: Arc<DashMap<String, MockResponder>>,
    requests: Arc<StdMutex<Vec<MockRequest>>>,
    client_back: Arc<StdMutex<Option<AdminClient>>>,
}

impl std::fmt::Debug for MockAdminSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockAdminSender").finish_non_exhaustive()
    }
}

#[async_trait]
impl AdminSender for MockAdminSender {
    async fn send_line(&self, line: String) -> Result<(), AdminError> {
        let frame: Value = serde_json::from_str(&line)
            .map_err(|e| AdminError::Transport(format!("mock: parse outbound frame: {e}")))?;
        let id = frame
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| AdminError::Transport("mock: outbound frame missing string id".into()))?;
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                AdminError::Transport("mock: outbound frame missing method".into())
            })?;
        let params = frame.get("params").cloned().unwrap_or(Value::Null);

        // Capture for assertions BEFORE invoking the responder so a
        // panicking responder still leaves the request log intact.
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.requests.lock().unwrap().push(MockRequest {
            method: method.clone(),
            params: params.clone(),
            at_ms,
        });

        // Dispatch through the registered responder, defaulting to
        // MethodNotFound when the test forgot to register one.
        let response_payload = match self.handlers.get(&method) {
            Some(h) => h.value()(params),
            None => Err(AdminError::MethodNotFound(format!(
                "mock: no handler for `{method}`"
            ))),
        };

        let response_frame = match response_payload {
            Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
            Err(e) => {
                let snap = render_error_snapshot(&e);
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": snapshot_to_wire(&snap),
                })
            }
        };

        // Deliver back to the bound client. The pending oneshot
        // wakes the caller awaiting `client.call_raw`. Spawn so
        // the send_line future returns promptly — matches the
        // production stdio path where the writer doesn't block on
        // the response coming back.
        let bound = self.client_back.lock().unwrap().clone();
        if let Some(client) = bound {
            let id_clone = id.clone();
            tokio::spawn(async move {
                client.on_inbound_response(&id_clone, &response_frame);
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn on_returns_canned_value_to_caller() {
        let mock = MockAdminRpc::new();
        mock.on(
            "nexo/admin/agents/list",
            json!({ "agents": [{ "id": "ana" }] }),
        );
        let client = mock.client();
        let result: Value = client
            .call_raw("nexo/admin/agents/list", json!({}))
            .await
            .unwrap();
        assert_eq!(result["agents"][0]["id"], "ana");
    }

    #[tokio::test]
    async fn on_err_propagates_capability_not_granted() {
        let mock = MockAdminRpc::new();
        mock.on_err(
            "nexo/admin/agents/upsert",
            AdminError::CapabilityNotGranted {
                capability: "agents_crud".into(),
                method: "nexo/admin/agents/upsert".into(),
            },
        );
        let client = mock.client();
        let err = client
            .call_raw("nexo/admin/agents/upsert", json!({}))
            .await
            .unwrap_err();
        match err {
            AdminError::CapabilityNotGranted { capability, .. } => {
                assert_eq!(capability, "agents_crud");
            }
            other => panic!("expected CapabilityNotGranted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unregistered_method_returns_method_not_found() {
        let mock = MockAdminRpc::new();
        let client = mock.client();
        let err = client
            .call_raw("nexo/admin/totally_unknown", json!({}))
            .await
            .unwrap_err();
        match err {
            AdminError::MethodNotFound(msg) => {
                assert!(msg.contains("nexo/admin/totally_unknown"), "{msg}");
            }
            other => panic!("expected MethodNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn on_with_receives_params_and_can_echo() {
        let mock = MockAdminRpc::new();
        mock.on_with("nexo/admin/echo", |params| {
            Ok(json!({ "echoed": params }))
        });
        let client = mock.client();
        let result: Value = client
            .call_raw("nexo/admin/echo", json!({ "x": 7 }))
            .await
            .unwrap();
        assert_eq!(result["echoed"]["x"], 7);
    }

    #[tokio::test]
    async fn requests_for_returns_each_call_with_params() {
        let mock = MockAdminRpc::new();
        mock.on("nexo/admin/agents/list", json!({ "agents": [] }));
        let client = mock.client();
        client
            .call_raw("nexo/admin/agents/list", json!({ "active_only": true }))
            .await
            .unwrap();
        client
            .call_raw("nexo/admin/agents/list", json!({ "tenant_id": "acme" }))
            .await
            .unwrap();
        let calls = mock.requests_for("nexo/admin/agents/list");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].params["active_only"], true);
        assert_eq!(calls[1].params["tenant_id"], "acme");
    }

    #[tokio::test]
    async fn handlers_for_distinct_methods_dont_cross_talk() {
        let mock = MockAdminRpc::new();
        mock.on("nexo/admin/a", json!({ "tag": "A" }));
        mock.on("nexo/admin/b", json!({ "tag": "B" }));
        let client = mock.client();
        let a: Value = client.call_raw("nexo/admin/a", json!({})).await.unwrap();
        let b: Value = client.call_raw("nexo/admin/b", json!({})).await.unwrap();
        assert_eq!(a["tag"], "A");
        assert_eq!(b["tag"], "B");
    }

    #[tokio::test]
    async fn reset_requests_clears_log_without_dropping_handlers() {
        let mock = MockAdminRpc::new();
        mock.on("nexo/admin/echo", json!({}));
        let client = mock.client();
        client.call_raw("nexo/admin/echo", json!({})).await.unwrap();
        assert_eq!(mock.requests().len(), 1);
        mock.reset_requests();
        assert!(mock.requests().is_empty());
        // Handler still wired — second call still works.
        client.call_raw("nexo/admin/echo", json!({})).await.unwrap();
        assert_eq!(mock.requests().len(), 1);
    }

    #[tokio::test]
    async fn on_with_closure_can_count_invocations() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&count);
        let mock = MockAdminRpc::new();
        mock.on_with("nexo/admin/ping", move |_| {
            count_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        });
        let client = mock.client();
        for _ in 0..3 {
            client.call_raw("nexo/admin/ping", json!({})).await.unwrap();
        }
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }
}
