//! Phase 92 — `StdioBridgeBroker` ships broker traffic over the
//! parent daemon's JSON-RPC stdio channel.
//!
//! Subprocess plugins extracted under Phase 81.19.a / 81.18 run in
//! a separate OS process and lose direct access to the daemon's
//! in-process `Local` broker (`tokio::mpsc`, a memory-only
//! abstraction without a network endpoint). Before this phase the
//! only escape hatch was NATS — which forces operators to install
//! and run a separate broker server even for single-host deploys.
//!
//! `StdioBridgeBroker` reuses the existing JSON-RPC channel the
//! daemon already opens for `tool.invoke`, adding three new
//! methods:
//!
//! - `broker.publish { topic, event }` — subprocess → daemon
//! - `broker.subscribe { topic_pattern }` — subprocess → daemon
//!   (daemon assigns a `subscriber_id` returned in the reply)
//! - `broker.unsubscribe { subscriber_id }` — subprocess → daemon
//! - `broker.event { subscriber_id, event }` — daemon →
//!   subprocess (notification, no id, no reply)
//!
//! Sub-phase 92.2 ships the subprocess-side struct + JSON-RPC
//! wire format. Sub-phase 92.3 ships the matching daemon-side
//! dispatcher in `proyecto/crates/core/src/agent/
//! nexo_plugin_registry/subprocess.rs`. Sub-phase 92.5 wires the
//! SDK's `PluginAdapter` to feed inbound stdin lines into
//! [`StdioBridgeBroker::handle_inbound_line`].
//!
//! The struct is fully testable in isolation via [`with_writer`]
//! and direct calls to [`handle_inbound_line`]; tests at the
//! bottom of this file cover the request/reply correlation table,
//! topic fanout, malformed-line handling, and concurrent publish
//! safety.
//!
//! [`with_writer`]: StdioBridgeBroker::with_writer
//! [`handle_inbound_line`]: StdioBridgeBroker::handle_inbound_line

use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};

use crate::handle::{BrokerHandle, Subscription};
use crate::types::{BrokerError, Event, Message};

/// Channel capacity per subscriber. Matches the LocalBroker
/// constant so a single-host deployment behaves identically
/// whether plugins are in-process or stdio-bridged.
const CHANNEL_CAPACITY: usize = 256;

// ---------------------------------------------------------------------------
// JSON-RPC wire types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonRpcRequest<'a, P: Serialize> {
    jsonrpc: &'a str,
    id: u64,
    method: &'a str,
    params: P,
}

#[derive(Deserialize)]
struct JsonRpcResponse {
    id: u64,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Deserialize)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i32,
    message: String,
}

/// Daemon → subprocess one-way notification for each matching
/// broker event. No `id` field; subscriber routes via
/// `params.subscriber_id`.
#[derive(Deserialize)]
struct BrokerEventNotification {
    method: String,
    params: BrokerEventParams,
}

#[derive(Deserialize)]
struct BrokerEventParams {
    subscriber_id: String,
    event: Event,
}

/// Daemon's reply to a `broker.subscribe` request. The
/// `subscriber_id` is daemon-assigned and used as the routing
/// key for subsequent `broker.event` notifications.
#[derive(Deserialize)]
struct SubscribeReplyPayload {
    subscriber_id: String,
}

// ---------------------------------------------------------------------------
// Broker
// ---------------------------------------------------------------------------

/// Type alias for the boxed writer the bridge owns. Concrete type
/// is `BufWriter<Stdout>` in production and an in-memory
/// `Vec<u8>`-backed writer in tests.
type BoxedWriter = Box<dyn Write + Send>;

/// Subprocess-side broker that bridges through the parent
/// daemon's stdio JSON-RPC channel. See module docs.
#[derive(Clone)]
pub struct StdioBridgeBroker {
    inner: Arc<Inner>,
}

struct Inner {
    /// Lock-protected writer for outbound JSON-RPC requests.
    /// Sync `std::sync::Mutex` because `writeln!` + `flush` are
    /// sync calls; async callers wrap the critical section in a
    /// short `tokio::task::spawn_blocking` if contention is ever
    /// observed (today single line per call, near-zero hold).
    writer: std::sync::Mutex<BoxedWriter>,
    /// Monotonically-increasing JSON-RPC id source.
    next_id: AtomicU64,
    /// Pending `broker.*` requests awaiting their reply by id.
    pending: AsyncMutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>,
    /// Live subscribers keyed by daemon-assigned subscriber id;
    /// values are the mpsc Sender feeding the `Subscription`
    /// returned to the caller.
    subscribers: AsyncMutex<HashMap<String, mpsc::Sender<Event>>>,
}

impl StdioBridgeBroker {
    /// Wire the broker to the current process's stdout, buffered.
    /// `flush` is called after every JSON-RPC line so the daemon
    /// sees requests immediately (stdio buffering would otherwise
    /// stall the bridge until the buffer fills).
    pub fn new_stdout() -> Self {
        let writer: BoxedWriter = Box::new(BufWriter::new(std::io::stdout()));
        Self::with_writer_boxed(writer)
    }

    /// Construct with a custom writer. Used by tests to assert
    /// the JSON-RPC request bytes the bridge produces, and by
    /// future embedding scenarios (e.g. an Android FFI shim
    /// that hands the bridge a Java-backed writer).
    pub fn with_writer<W: Write + Send + 'static>(writer: W) -> Self {
        Self::with_writer_boxed(Box::new(writer))
    }

    fn with_writer_boxed(writer: BoxedWriter) -> Self {
        Self {
            inner: Arc::new(Inner {
                writer: std::sync::Mutex::new(writer),
                next_id: AtomicU64::new(1),
                pending: AsyncMutex::new(HashMap::new()),
                subscribers: AsyncMutex::new(HashMap::new()),
            }),
        }
    }

    /// Feed one JSON-RPC line received from the daemon parent.
    ///
    /// Recognized shapes:
    ///
    /// 1. **Response** with `id` matching a pending request —
    ///    resolves the awaiting `call()` future.
    /// 2. **Notification** `broker.event` — routes the payload
    ///    to the matching subscriber's mpsc channel.
    ///
    /// Anything else is logged at WARN and dropped. The caller
    /// (typically the SDK's `PluginAdapter` stdin multiplexer)
    /// is responsible for forwarding only broker-relevant lines
    /// here; `tool.invoke` traffic stays on the adapter's own
    /// dispatcher.
    pub async fn handle_inbound_line(&self, line: &str) {
        // Response path — try first because the response shape is
        // a strict subset of the notification shape.
        if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(line) {
            if resp.result.is_some() || resp.error.is_some() {
                let mut pending = self.inner.pending.lock().await;
                if let Some(tx) = pending.remove(&resp.id) {
                    let outcome = match (resp.result, resp.error) {
                        (Some(v), _) => Ok(v),
                        (None, Some(e)) => Err(e.message),
                        (None, None) => Err("malformed JSON-RPC reply".to_string()),
                    };
                    let _ = tx.send(outcome);
                } else {
                    tracing::warn!(
                        id = resp.id,
                        "stdio_bridge: reply for unknown request id (dropped)"
                    );
                }
                return;
            }
        }

        // Notification path.
        if let Ok(notif) = serde_json::from_str::<BrokerEventNotification>(line) {
            if notif.method == "broker.event" {
                self.dispatch_event(notif.params).await;
                return;
            }
        }

        tracing::warn!(
            line = %line,
            "stdio_bridge: unrecognized inbound JSON-RPC line"
        );
    }

    /// Route a single `broker.event` to its subscriber. Drops
    /// the subscription entry when the receiver has been closed
    /// (the `Subscription` was dropped by the plugin) and lazily
    /// sends a `broker.unsubscribe` so the daemon stops
    /// forwarding for it.
    async fn dispatch_event(&self, params: BrokerEventParams) {
        let mut subscribers = self.inner.subscribers.lock().await;
        let still_live = match subscribers.get(&params.subscriber_id) {
            Some(tx) => match tx.try_send(params.event) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(
                        subscriber_id = %params.subscriber_id,
                        "stdio_bridge: drop broker.event (subscriber channel full)"
                    );
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            },
            None => {
                tracing::warn!(
                    subscriber_id = %params.subscriber_id,
                    "stdio_bridge: broker.event for unknown subscriber"
                );
                false
            }
        };
        if !still_live {
            let removed = subscribers.remove(&params.subscriber_id).is_some();
            drop(subscribers);
            if removed {
                // Best-effort lazy unsubscribe; ignore errors
                // because the daemon might already have torn the
                // subscriber down on its side (race with child
                // exit).
                let _ = self
                    .call(
                        "broker.unsubscribe",
                        serde_json::json!({ "subscriber_id": params.subscriber_id }),
                    )
                    .await;
            }
        }
    }

    /// Issue a JSON-RPC request and await the daemon's reply.
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, BrokerError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel::<Result<serde_json::Value, String>>();
        {
            let mut pending = self.inner.pending.lock().await;
            pending.insert(id, tx);
        }
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        let line = serde_json::to_string(&req)
            .map_err(|e| BrokerError::SendError(format!("serialize {method}: {e}")))?;
        {
            let mut writer = self.inner.writer.lock().expect("writer mutex poisoned");
            writeln!(writer, "{}", line)
                .map_err(|e| BrokerError::SendError(format!("write {method}: {e}")))?;
            writer
                .flush()
                .map_err(|e| BrokerError::SendError(format!("flush {method}: {e}")))?;
        }
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(msg)) => Err(BrokerError::SubscribeError(format!("{method}: {msg}"))),
            Err(_) => {
                // Best-effort cleanup of the pending entry; the
                // oneshot was dropped without a send.
                let mut pending = self.inner.pending.lock().await;
                pending.remove(&id);
                Err(BrokerError::SendError(format!(
                    "{method}: reply channel closed before daemon response"
                )))
            }
        }
    }
}

#[async_trait]
impl BrokerHandle for StdioBridgeBroker {
    async fn publish(&self, topic: &str, event: Event) -> Result<(), BrokerError> {
        let params = serde_json::json!({ "topic": topic, "event": event });
        let _reply = self.call("broker.publish", params).await?;
        Ok(())
    }

    async fn subscribe(&self, topic_pattern: &str) -> Result<Subscription, BrokerError> {
        let params = serde_json::json!({ "topic_pattern": topic_pattern });
        let reply = self.call("broker.subscribe", params).await?;
        let parsed: SubscribeReplyPayload = serde_json::from_value(reply)
            .map_err(|e| BrokerError::SubscribeError(format!("parse reply: {e}")))?;
        let (tx, rx) = mpsc::channel::<Event>(CHANNEL_CAPACITY);
        {
            let mut subscribers = self.inner.subscribers.lock().await;
            subscribers.insert(parsed.subscriber_id.clone(), tx);
        }
        Ok(Subscription::new(topic_pattern.to_string(), rx))
    }

    async fn request(
        &self,
        topic: &str,
        _msg: Message,
        _timeout: Duration,
    ) -> Result<Message, BrokerError> {
        // Phase 92 v1 does not pipe req/reply semantics through
        // the bridge — every extracted subprocess plugin in tree
        // today uses only publish + subscribe. Track as a
        // follow-up (`92.followup-request-reply`) once a plugin
        // surfaces the need; until then, fail fast so callers
        // notice and route around it.
        Err(BrokerError::RequestTimeout(format!(
            "stdio_bridge: request/reply not implemented for topic '{topic}' \
             (use publish + subscribe; see Phase 92 followup-request-reply)"
        )))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// In-memory writer backed by a shared `Vec<u8>` so tests can
    /// assert the exact JSON-RPC line bytes the bridge produced.
    #[derive(Clone, Default)]
    struct CapturedWriter {
        buf: Arc<StdMutex<Vec<u8>>>,
    }

    impl Write for CapturedWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.buf.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl CapturedWriter {
        fn snapshot(&self) -> String {
            String::from_utf8(self.buf.lock().unwrap().clone()).unwrap()
        }
    }

    fn sample_event(topic: &str, payload: serde_json::Value) -> Event {
        Event::new(topic, "test", payload)
    }

    #[tokio::test]
    async fn publish_writes_jsonrpc_request_and_resolves_on_ack() {
        let writer = CapturedWriter::default();
        let broker = StdioBridgeBroker::with_writer(writer.clone());

        // Start publish in the background; it will block on the
        // ack so we have to feed `handle_inbound_line` first.
        let pub_task = {
            let broker = broker.clone();
            tokio::spawn(async move {
                broker
                    .publish("plugin.inbound.test", sample_event("plugin.inbound.test", serde_json::json!({"k": "v"})))
                    .await
            })
        };

        // Spin briefly so the request lands in the writer.
        for _ in 0..50 {
            if !writer.snapshot().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let written = writer.snapshot();
        assert!(
            written.contains("\"method\":\"broker.publish\""),
            "expected broker.publish line, got: {written}"
        );
        assert!(
            written.contains("\"topic\":\"plugin.inbound.test\""),
            "expected topic field, got: {written}"
        );

        // Simulate daemon ack.
        broker
            .handle_inbound_line(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#)
            .await;

        pub_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn subscribe_returns_subscription_after_daemon_assigns_id() {
        let writer = CapturedWriter::default();
        let broker = StdioBridgeBroker::with_writer(writer.clone());

        let sub_task = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.subscribe("plugin.outbound.>").await })
        };

        for _ in 0..50 {
            if !writer.snapshot().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        broker
            .handle_inbound_line(
                r#"{"jsonrpc":"2.0","id":1,"result":{"subscriber_id":"sub-abc"}}"#,
            )
            .await;

        let sub = sub_task.await.unwrap().unwrap();
        assert_eq!(sub.topic, "plugin.outbound.>");
    }

    #[tokio::test]
    async fn broker_event_routes_to_subscriber_mpsc() {
        let writer = CapturedWriter::default();
        let broker = StdioBridgeBroker::with_writer(writer);

        // Open a subscription with daemon-assigned id "sub-1".
        let sub_task = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.subscribe("plugin.outbound.test").await })
        };
        // Wait a tick so the pending request lands.
        tokio::time::sleep(Duration::from_millis(20)).await;
        broker
            .handle_inbound_line(
                r#"{"jsonrpc":"2.0","id":1,"result":{"subscriber_id":"sub-1"}}"#,
            )
            .await;
        let mut sub = sub_task.await.unwrap().unwrap();

        // Daemon pushes one event.
        let event = sample_event("plugin.outbound.test", serde_json::json!({"hello": "world"}));
        let notification = format!(
            r#"{{"jsonrpc":"2.0","method":"broker.event","params":{{"subscriber_id":"sub-1","event":{}}}}}"#,
            serde_json::to_string(&event).unwrap()
        );
        broker.handle_inbound_line(&notification).await;

        let received = tokio::time::timeout(Duration::from_millis(200), sub.next())
            .await
            .expect("sub.next() did not yield within timeout");
        let received = received.expect("subscription returned None");
        assert_eq!(received.topic, "plugin.outbound.test");
        assert_eq!(received.payload, serde_json::json!({"hello": "world"}));
    }

    #[tokio::test]
    async fn jsonrpc_error_reply_surfaces_as_subscribe_error() {
        let writer = CapturedWriter::default();
        let broker = StdioBridgeBroker::with_writer(writer);
        let pub_task = {
            let broker = broker.clone();
            tokio::spawn(async move {
                broker
                    .publish("forbidden.topic", sample_event("forbidden.topic", serde_json::json!({})))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        broker
            .handle_inbound_line(
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"topic not in publish allowlist"}}"#,
            )
            .await;
        let err = pub_task.await.unwrap().expect_err("expected publish to fail");
        match err {
            BrokerError::SubscribeError(msg) => {
                assert!(msg.contains("topic not in publish allowlist"), "msg={msg}");
            }
            other => panic!("expected SubscribeError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_inbound_line_is_dropped_silently() {
        // No subscribers, no pending — the bridge must not panic.
        let broker = StdioBridgeBroker::with_writer(CapturedWriter::default());
        broker.handle_inbound_line("not even json").await;
        broker
            .handle_inbound_line(r#"{"id": 999, "no": "matching fields"}"#)
            .await;
    }

    #[tokio::test]
    async fn unknown_subscriber_id_is_dropped_silently() {
        let broker = StdioBridgeBroker::with_writer(CapturedWriter::default());
        let event = sample_event("any.topic", serde_json::json!({}));
        let notif = format!(
            r#"{{"jsonrpc":"2.0","method":"broker.event","params":{{"subscriber_id":"sub-missing","event":{}}}}}"#,
            serde_json::to_string(&event).unwrap()
        );
        broker.handle_inbound_line(&notif).await;
        // No assertions — must not panic and must not block.
    }

    #[tokio::test]
    async fn concurrent_publishes_get_unique_ids() {
        let writer = CapturedWriter::default();
        let broker = StdioBridgeBroker::with_writer(writer.clone());

        let mut tasks = Vec::new();
        for i in 0..10 {
            let broker = broker.clone();
            tasks.push(tokio::spawn(async move {
                broker
                    .publish(
                        &format!("test.topic.{i}"),
                        sample_event(&format!("test.topic.{i}"), serde_json::json!({"i": i})),
                    )
                    .await
            }));
        }

        // Give all tasks time to write their requests.
        for _ in 0..50 {
            let written = writer.snapshot();
            if written.matches("broker.publish").count() == 10 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let written = writer.snapshot();
        let mut seen_ids = std::collections::HashSet::new();
        for line in written.lines() {
            // Extract the id field; rough but sufficient for the
            // uniqueness assertion.
            if let Some(idx) = line.find("\"id\":") {
                let tail = &line[idx + 5..];
                let end = tail.find(',').unwrap_or(tail.len());
                let id_str = tail[..end].trim();
                seen_ids.insert(id_str.to_string());
            }
        }
        assert_eq!(seen_ids.len(), 10, "expected 10 unique ids, got {:?}", seen_ids);

        // Resolve each pending publish so the tasks can complete.
        for id in 1..=10u64 {
            broker
                .handle_inbound_line(&format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"result":{{"ok":true}}}}"#
                ))
                .await;
        }
        for t in tasks {
            t.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn request_method_returns_unsupported_error() {
        let broker = StdioBridgeBroker::with_writer(CapturedWriter::default());
        let err = broker
            .request("foo", Message::new("foo", serde_json::json!({})), Duration::from_millis(10))
            .await
            .expect_err("request must not succeed on the stdio bridge today");
        match err {
            BrokerError::RequestTimeout(msg) => {
                assert!(
                    msg.contains("stdio_bridge"),
                    "expected stdio_bridge mention in error, got: {msg}"
                );
            }
            other => panic!("expected RequestTimeout, got {other:?}"),
        }
    }
}
