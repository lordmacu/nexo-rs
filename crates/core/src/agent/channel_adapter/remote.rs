//! Phase 81.24 — `ChannelAdapter` impl backed by a subprocess
//! plugin's stdio bridge. Each method serializes its args into a
//! JSON-RPC 2.0 request, registers a oneshot in the shared
//! `pending` map, fires the frame to `stdin_tx`, and awaits the
//! reply with a per-method timeout.
//!
//! The transport handles (`stdin_tx`, `pending`, `next_id`) are
//! cloned from the subprocess plugin's `Inner`. Multiple
//! `RemoteChannelAdapter`s for the same plugin share the single
//! stdio pipe — `kind` on every payload lets the child dispatch
//! correctly when one subprocess advertises multiple kinds via
//! `manifest.plugin.extends.channels`.
//!
//! Error responses come back through the existing pending map as
//! `Err(stringified_json_rpc_error_object)`. We re-parse the
//! string to JSON and map error codes onto the typed
//! `ChannelAdapterError` variants per the contract spec
//! (§5.x, contract v1.5.0):
//!
//! - `-32601` → `Unsupported`
//! - `-32602` / `-32603` → `Other`
//! - `-33001` (channel.connection_failed) → `Connection`
//! - `-33002` (channel.authentication_failed) → `Authentication`
//! - `-33003` (channel.recipient_invalid) → `Recipient`
//! - `-33004` (channel.rate_limited) → `RateLimited`
//! - `-33005` (channel.unsupported_feature) → `Unsupported`
//! - parse failure / unknown code → `Other`

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use nexo_broker::AnyBroker;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::{ChannelAdapter, ChannelAdapterError, OutboundAck, OutboundMessage};

const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// Phase 81.24 — `ChannelAdapter` impl backed by a subprocess
/// plugin. Constructed once per kind by the boot helper after a
/// `SubprocessNexoPlugin::init()` succeeds.
pub struct RemoteChannelAdapter {
    kind: String,
    plugin_id: String,
    stdin_tx: mpsc::Sender<Value>,
    pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: Arc<AtomicU64>,
    start_timeout: Duration,
    stop_timeout: Duration,
    send_timeout: Duration,
}

impl RemoteChannelAdapter {
    pub fn new(
        kind: String,
        plugin_id: String,
        stdin_tx: mpsc::Sender<Value>,
        pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
        next_id: Arc<AtomicU64>,
    ) -> Self {
        let (start_timeout, stop_timeout, send_timeout) = Self::resolve_timeouts();
        Self {
            kind,
            plugin_id,
            stdin_tx,
            pending,
            next_id,
            start_timeout,
            stop_timeout,
            send_timeout,
        }
    }

    fn resolve_timeouts() -> (Duration, Duration, Duration) {
        let env_override = std::env::var("NEXO_PLUGIN_CHANNEL_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis);
        match env_override {
            Some(t) => (t, t, t),
            None => (
                DEFAULT_START_TIMEOUT,
                DEFAULT_STOP_TIMEOUT,
                DEFAULT_SEND_TIMEOUT,
            ),
        }
    }

    /// Plugin id that owns this adapter. Used by the boot helper's
    /// rollback path on `KindAlreadyRegistered`.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Send a JSON-RPC request and await the reply. Times out per
    /// the configured method budget. Maps every failure into a
    /// typed `ChannelAdapterError`.
    async fn send_request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ChannelAdapterError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        if let Err(e) = self.stdin_tx.send(frame).await {
            // Stdin pipe closed before we could enqueue. Drop the
            // pending slot so a late reply doesn't leak.
            self.pending.remove(&id);
            return Err(ChannelAdapterError::Other {
                kind: self.kind.clone(),
                source: anyhow::anyhow!("stdin send failed for {method}: {e}"),
            });
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(err_str))) => Err(self.parse_error_string(&err_str)),
            Ok(Err(_)) => {
                // Sender dropped before sending — usually the
                // subprocess pipeline tearing down.
                self.pending.remove(&id);
                Err(ChannelAdapterError::Other {
                    kind: self.kind.clone(),
                    source: anyhow::anyhow!(
                        "subprocess pending dropped while awaiting {method} reply"
                    ),
                })
            }
            Err(_) => {
                // Timeout. Remove the pending slot so a late reply
                // doesn't leak it.
                self.pending.remove(&id);
                let secs = timeout.as_secs();
                Err(ChannelAdapterError::Other {
                    kind: self.kind.clone(),
                    source: anyhow::anyhow!("{method} timed out after {secs}s"),
                })
            }
        }
    }

    /// Parse the stringified JSON-RPC error object back to JSON
    /// and map `code` / `message` / `data` onto a typed
    /// `ChannelAdapterError`. Falls through to `Other` on parse
    /// failure or unknown code.
    fn parse_error_string(&self, s: &str) -> ChannelAdapterError {
        let parsed: Value = match serde_json::from_str(s) {
            Ok(v) => v,
            Err(_) => {
                return ChannelAdapterError::Other {
                    kind: self.kind.clone(),
                    source: anyhow::anyhow!("{}", s),
                };
            }
        };
        let code = parsed.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let message = parsed
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let data = parsed.get("data").cloned().unwrap_or(Value::Null);

        match code {
            -32601 => ChannelAdapterError::Unsupported {
                kind: self.kind.clone(),
                feature: message.clone(),
            },
            -33001 => ChannelAdapterError::Connection {
                kind: self.kind.clone(),
                source: anyhow::anyhow!("{message}"),
            },
            -33002 => ChannelAdapterError::Authentication {
                kind: self.kind.clone(),
                reason: message.clone(),
            },
            -33003 => {
                let recipient = data
                    .get("recipient")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let reason = data
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&message)
                    .to_string();
                ChannelAdapterError::Recipient {
                    kind: self.kind.clone(),
                    recipient,
                    reason,
                }
            }
            -33004 => {
                let retry_after_secs = data
                    .get("retry_after_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                ChannelAdapterError::RateLimited {
                    kind: self.kind.clone(),
                    retry_after_secs,
                }
            }
            -33005 => {
                let feature = data
                    .get("feature")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&message)
                    .to_string();
                ChannelAdapterError::Unsupported {
                    kind: self.kind.clone(),
                    feature,
                }
            }
            _ => ChannelAdapterError::Other {
                kind: self.kind.clone(),
                source: anyhow::anyhow!("code {code}: {message}"),
            },
        }
    }
}

#[async_trait]
impl ChannelAdapter for RemoteChannelAdapter {
    fn kind(&self) -> &str {
        &self.kind
    }

    async fn start(
        &self,
        _broker: AnyBroker,
        instance: Option<&str>,
    ) -> Result<(), ChannelAdapterError> {
        // `_broker` is unused for remote adapters — the subprocess
        // already has its own broker handle from the bridge wired
        // up by Phase 81.14.b.
        let params = serde_json::json!({
            "kind": &self.kind,
            "instance": instance,
        });
        self.send_request("channel.start", params, self.start_timeout)
            .await?;
        Ok(())
    }

    async fn stop(&self) -> Result<(), ChannelAdapterError> {
        let params = serde_json::json!({ "kind": &self.kind });
        self.send_request("channel.stop", params, self.stop_timeout)
            .await?;
        Ok(())
    }

    async fn send_outbound(
        &self,
        msg: OutboundMessage,
    ) -> Result<OutboundAck, ChannelAdapterError> {
        let params = serde_json::json!({
            "kind": &self.kind,
            "msg": msg,
        });
        let result = self
            .send_request("channel.send_outbound", params, self.send_timeout)
            .await?;
        serde_json::from_value::<OutboundAck>(result).map_err(|e| ChannelAdapterError::Other {
            kind: self.kind.clone(),
            source: anyhow::anyhow!("decode OutboundAck: {e}"),
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fresh adapter + the matching peer-side handles
    /// (`stdin_rx`, shared `pending`, shared `next_id`) so a test
    /// can pretend to be the subprocess: read frames from stdin_rx,
    /// look up the request id, and resolve the matching pending
    /// oneshot with a synthetic reply.
    fn build() -> (
        Arc<RemoteChannelAdapter>,
        mpsc::Receiver<Value>,
        Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    ) {
        let (stdin_tx, stdin_rx) = mpsc::channel(8);
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>> =
            Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(1));
        let adapter = Arc::new(RemoteChannelAdapter::new(
            "mock_chan".to_string(),
            "mock_plugin".to_string(),
            stdin_tx,
            pending.clone(),
            next_id,
        ));
        (adapter, stdin_rx, pending)
    }

    fn resolve_with_result(
        pending: &DashMap<u64, oneshot::Sender<Result<Value, String>>>,
        id: u64,
        result: Value,
    ) {
        if let Some((_, sender)) = pending.remove(&id) {
            let _ = sender.send(Ok(result));
        }
    }

    fn resolve_with_error(
        pending: &DashMap<u64, oneshot::Sender<Result<Value, String>>>,
        id: u64,
        err_obj: Value,
    ) {
        if let Some((_, sender)) = pending.remove(&id) {
            let _ = sender.send(Err(err_obj.to_string()));
        }
    }

    #[test]
    fn kind_returns_declared_kind() {
        let (adapter, _, _) = build();
        assert_eq!(adapter.kind(), "mock_chan");
        assert_eq!(adapter.plugin_id(), "mock_plugin");
    }

    #[tokio::test]
    async fn start_serializes_request_with_kind_and_instance() {
        let (adapter, mut stdin_rx, pending) = build();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let task = tokio::spawn({
            let adapter = adapter.clone();
            async move { adapter.start(broker, Some("acct1")).await }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        assert_eq!(frame["method"], "channel.start");
        assert_eq!(frame["params"]["kind"], "mock_chan");
        assert_eq!(frame["params"]["instance"], "acct1");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(&pending, id, serde_json::json!({ "ok": true }));

        let result = task.await.unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn stop_serializes_request_with_kind() {
        let (adapter, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let adapter = adapter.clone();
            async move { adapter.stop().await }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        assert_eq!(frame["method"], "channel.stop");
        assert_eq!(frame["params"]["kind"], "mock_chan");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(&pending, id, serde_json::json!({ "ok": true }));

        let result = task.await.unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn send_outbound_round_trips_ack() {
        let (adapter, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let adapter = adapter.clone();
            async move {
                adapter
                    .send_outbound(OutboundMessage::Text {
                        to: "U123".into(),
                        body: "hi".into(),
                    })
                    .await
            }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        assert_eq!(frame["method"], "channel.send_outbound");
        assert_eq!(frame["params"]["kind"], "mock_chan");
        assert_eq!(frame["params"]["msg"]["kind"], "text");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(
            &pending,
            id,
            serde_json::json!({ "message_id": "echo-1", "sent_at_unix": 1700000000 }),
        );

        let ack = task.await.unwrap().unwrap();
        assert_eq!(ack.message_id, "echo-1");
        assert_eq!(ack.sent_at_unix, 1700000000);
    }

    #[tokio::test]
    async fn unsupported_method_error_maps_to_unsupported() {
        let (adapter, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let adapter = adapter.clone();
            async move { adapter.stop().await }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_error(
            &pending,
            id,
            serde_json::json!({
                "code": -32601,
                "message": "channel.stop"
            }),
        );

        let err = task.await.unwrap().unwrap_err();
        assert!(matches!(
            err,
            ChannelAdapterError::Unsupported { ref kind, ref feature }
                if kind == "mock_chan" && feature == "channel.stop"
        ));
    }

    #[tokio::test]
    async fn rate_limited_error_extracts_retry_after_secs() {
        let (adapter, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let adapter = adapter.clone();
            async move {
                adapter
                    .send_outbound(OutboundMessage::Text {
                        to: "U1".into(),
                        body: "hi".into(),
                    })
                    .await
            }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_error(
            &pending,
            id,
            serde_json::json!({
                "code": -33004,
                "message": "rate limited",
                "data": { "retry_after_secs": 42 }
            }),
        );

        let err = task.await.unwrap().unwrap_err();
        match err {
            ChannelAdapterError::RateLimited {
                kind,
                retry_after_secs,
            } => {
                assert_eq!(kind, "mock_chan");
                assert_eq!(retry_after_secs, 42);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn connection_error_maps_to_connection() {
        let (adapter, mut stdin_rx, pending) = build();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let task = tokio::spawn({
            let adapter = adapter.clone();
            async move { adapter.start(broker, None).await }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_error(
            &pending,
            id,
            serde_json::json!({
                "code": -33001,
                "message": "tcp dial failed: connection refused"
            }),
        );

        let err = task.await.unwrap().unwrap_err();
        assert!(matches!(
            err,
            ChannelAdapterError::Connection { ref kind, .. } if kind == "mock_chan"
        ));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn request_timeout_returns_other_error() {
        // Build an adapter with a tiny timeout so the test doesn't
        // have to wait the default 60s. We construct manually so
        // the env-var resolution doesn't fight the test.
        let (stdin_tx, mut stdin_rx) = mpsc::channel(8);
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>> =
            Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(1));
        let adapter = RemoteChannelAdapter {
            kind: "mock_chan".into(),
            plugin_id: "mock_plugin".into(),
            stdin_tx,
            pending: pending.clone(),
            next_id,
            start_timeout: Duration::from_millis(50),
            stop_timeout: Duration::from_millis(50),
            send_timeout: Duration::from_millis(50),
        };

        let task = tokio::spawn(async move { adapter.stop().await });
        // Drain the frame but never resolve the pending oneshot.
        let _frame = stdin_rx.recv().await.expect("frame");

        // Advance virtual time past the timeout.
        tokio::time::advance(Duration::from_millis(100)).await;

        let err = task.await.unwrap().unwrap_err();
        match err {
            ChannelAdapterError::Other { kind, source } => {
                assert_eq!(kind, "mock_chan");
                assert!(
                    source.to_string().contains("timed out"),
                    "expected timeout message, got {source}"
                );
            }
            other => panic!("expected Other(timeout), got {other:?}"),
        }
    }
}
