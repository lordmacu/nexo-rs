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
//! daemon already opens for `tool.invoke`. The wire shape is the
//! one the daemon-side host (Phase 81.14.b in
//! `proyecto/crates/core/src/agent/nexo_plugin_registry/
//! subprocess.rs`) already speaks:
//!
//! - `broker.publish { topic, event }` — subprocess → daemon
//!   notification (no `id`, fire-and-forget). Daemon validates the
//!   topic against the plugin manifest's
//!   `[plugin.capabilities.broker].publish` allowlist and forwards
//!   onto its in-process broker. Drops the frame on allowlist
//!   reject without a reply; track as
//!   `92.followup-publish-ack` when operators surface a real
//!   complaint about silent drops.
//!
//! - `broker.event { topic, event }` — daemon → subprocess
//!   notification. Daemon pre-subscribes (at boot) to every
//!   pattern in the manifest's
//!   `[plugin.capabilities.broker].subscribe` list + auto-derived
//!   `plugin.outbound.<kind>.>` patterns, and pushes a frame for
//!   every matching event. The subprocess-side broker filters
//!   these locally by topic pattern using [`crate::topic::
//!   topic_matches`] (NATS-style `*` and `>` wildcards) and
//!   fans out to every matching `Subscription`.
//!
//! There is **no** explicit `broker.subscribe` RPC. The daemon's
//! subscriber list is fixed at boot from the manifest; dynamic
//! subscription is bounded by that allowlist (defense-in-depth
//! per Phase 81.29). A plugin calling `subscribe(pattern)` for a
//! pattern outside its manifest will receive zero events
//! silently — the manifest is the source of truth.
//!
//! Sub-phase 92.2 ships the subprocess-side struct + wire format.
//! Sub-phase 92.5 wires the SDK's `PluginAdapter` to feed inbound
//! stdin lines into [`StdioBridgeBroker::handle_inbound_line`].
//!
//! [`handle_inbound_line`]: StdioBridgeBroker::handle_inbound_line

use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use crate::handle::{BrokerHandle, Subscription};
use crate::topic::topic_matches;
use crate::types::{BrokerError, Event, Message};

/// Channel capacity per subscriber. Matches the LocalBroker
/// constant so a single-host deployment behaves identically
/// whether plugins are in-process or stdio-bridged.
const CHANNEL_CAPACITY: usize = 256;

// ---------------------------------------------------------------------------
// JSON-RPC wire types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonRpcNotification<'a, P: Serialize> {
    jsonrpc: &'a str,
    method: &'a str,
    params: P,
}

#[derive(Deserialize)]
struct BrokerEventFrame {
    method: String,
    params: BrokerEventParams,
}

#[derive(Deserialize)]
struct BrokerEventParams {
    topic: String,
    event: Event,
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
    /// Lock-protected writer for outbound JSON-RPC notifications.
    /// Sync `std::sync::Mutex` because `writeln!` + `flush` are
    /// sync calls; near-zero hold time (one line per notify).
    writer: std::sync::Mutex<BoxedWriter>,
    /// Live subscribers: (topic_pattern, mpsc Sender). Vec
    /// because lookups iterate all entries anyway (topic_matches
    /// is the discriminator); also lets the same pattern have
    /// multiple subscribers if a plugin opens parallel
    /// `Subscription`s on the same pattern.
    subscribers: AsyncMutex<Vec<(String, mpsc::Sender<Event>)>>,
}

impl StdioBridgeBroker {
    /// Wire the broker to the current process's stdout, buffered.
    /// `flush` is called after every JSON-RPC line so the daemon
    /// sees notifications immediately (stdio buffering would
    /// otherwise stall the bridge until the buffer fills).
    pub fn new_stdout() -> Self {
        let writer: BoxedWriter = Box::new(BufWriter::new(std::io::stdout()));
        Self::with_writer_boxed(writer)
    }

    /// Construct with a custom writer. Used by tests to assert
    /// the JSON-RPC notification bytes the bridge produces, and
    /// by future embedding scenarios (e.g. an Android FFI shim
    /// that hands the bridge a Java-backed writer).
    pub fn with_writer<W: Write + Send + 'static>(writer: W) -> Self {
        Self::with_writer_boxed(Box::new(writer))
    }

    fn with_writer_boxed(writer: BoxedWriter) -> Self {
        Self {
            inner: Arc::new(Inner {
                writer: std::sync::Mutex::new(writer),
                subscribers: AsyncMutex::new(Vec::new()),
            }),
        }
    }

    /// Feed one JSON-RPC line received from the daemon parent.
    ///
    /// Recognized shape: `broker.event { topic, event }`
    /// notification. Anything else — `tool.invoke` requests,
    /// `llm.chat.delta` streaming chunks, malformed JSON — is
    /// logged at WARN and dropped. The caller (typically the SDK's
    /// `PluginAdapter` stdin multiplexer in 92.5) is responsible
    /// for filtering lines to broker-relevant ones before
    /// invoking this method, but the method itself is defensive
    /// and tolerates noise.
    pub async fn handle_inbound_line(&self, line: &str) {
        let frame: BrokerEventFrame = match serde_json::from_str(line) {
            Ok(f) => f,
            Err(_) => {
                tracing::trace!(
                    line = %line,
                    "stdio_bridge: inbound line not broker.event (dropped)"
                );
                return;
            }
        };
        if frame.method != "broker.event" {
            tracing::trace!(
                method = %frame.method,
                "stdio_bridge: inbound line not broker.event (dropped)"
            );
            return;
        }
        self.dispatch_event(frame.params).await;
    }

    /// Fan an inbound `broker.event` out to every subscriber
    /// whose registered topic pattern matches the event topic.
    /// Lazily prunes subscribers whose receivers have been
    /// dropped (mpsc Closed) so the subscribers list stays
    /// bounded by live consumers.
    async fn dispatch_event(&self, params: BrokerEventParams) {
        let mut subscribers = self.inner.subscribers.lock().await;
        let mut to_remove: Vec<usize> = Vec::new();
        for (i, (pattern, tx)) in subscribers.iter().enumerate() {
            if !topic_matches(pattern, &params.topic) {
                continue;
            }
            match tx.try_send(params.event.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(
                        topic = %params.topic,
                        pattern = %pattern,
                        "stdio_bridge: drop broker.event (subscriber channel full)"
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    to_remove.push(i);
                }
            }
        }
        // Remove from tail to head so indices stay valid.
        for idx in to_remove.into_iter().rev() {
            subscribers.swap_remove(idx);
        }
    }

    /// Serialise + write a JSON-RPC notification to the outbound
    /// channel. Sync writeln + flush under the writer mutex.
    fn write_notification<P: Serialize>(
        &self,
        method: &str,
        params: P,
    ) -> Result<(), BrokerError> {
        let frame = JsonRpcNotification {
            jsonrpc: "2.0",
            method,
            params,
        };
        let line = serde_json::to_string(&frame)
            .map_err(|e| BrokerError::SendError(format!("serialize {method}: {e}")))?;
        let mut writer = self.inner.writer.lock().expect("writer mutex poisoned");
        writeln!(writer, "{}", line)
            .map_err(|e| BrokerError::SendError(format!("write {method}: {e}")))?;
        writer
            .flush()
            .map_err(|e| BrokerError::SendError(format!("flush {method}: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl BrokerHandle for StdioBridgeBroker {
    /// Fire-and-forget publish over stdio. Returns `Ok(())` once
    /// the bytes are flushed to stdout; the daemon's allowlist
    /// check + actual broker fanout happen asynchronously on its
    /// side. Followup `92.followup-publish-ack` will add an
    /// optional req/reply mode for callers that need
    /// per-publish confirmation.
    async fn publish(&self, topic: &str, event: Event) -> Result<(), BrokerError> {
        let params = serde_json::json!({ "topic": topic, "event": event });
        self.write_notification("broker.publish", params)
    }

    /// Register a local subscription filter. No daemon RPC fires
    /// — the daemon's `[plugin.capabilities.broker].subscribe`
    /// patterns from the manifest already cover what events get
    /// streamed inbound; this call only sets up the in-process
    /// fanout for matching frames.
    async fn subscribe(&self, topic_pattern: &str) -> Result<Subscription, BrokerError> {
        let (tx, rx) = mpsc::channel::<Event>(CHANNEL_CAPACITY);
        let mut subscribers = self.inner.subscribers.lock().await;
        subscribers.push((topic_pattern.to_string(), tx));
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
    async fn publish_writes_notification_without_id() {
        let writer = CapturedWriter::default();
        let broker = StdioBridgeBroker::with_writer(writer.clone());
        broker
            .publish(
                "plugin.inbound.test",
                sample_event("plugin.inbound.test", serde_json::json!({"k": "v"})),
            )
            .await
            .unwrap();
        let written = writer.snapshot();
        assert!(
            written.contains("\"method\":\"broker.publish\""),
            "expected broker.publish line, got: {written}"
        );
        assert!(
            written.contains("\"topic\":\"plugin.inbound.test\""),
            "expected topic field, got: {written}"
        );
        // Notifications must NOT carry a top-level id field —
        // that's a JSON-RPC 2.0 invariant the daemon's dispatcher
        // relies on to distinguish notifications from requests.
        // Parse the frame to check the envelope structure
        // (substring check would false-positive on Event.id
        // nested inside params).
        let parsed: serde_json::Value =
            serde_json::from_str(written.trim_end()).expect("publish line is valid JSON");
        let envelope = parsed.as_object().expect("envelope must be an object");
        assert!(
            !envelope.contains_key("id"),
            "publish envelope must not carry id: {written}"
        );
        assert_eq!(envelope.get("method").and_then(|v| v.as_str()), Some("broker.publish"));
    }

    #[tokio::test]
    async fn subscribe_is_synchronous_no_daemon_rpc() {
        let writer = CapturedWriter::default();
        let broker = StdioBridgeBroker::with_writer(writer.clone());
        let sub = broker.subscribe("plugin.outbound.>").await.unwrap();
        assert_eq!(sub.topic, "plugin.outbound.>");
        // No RPC writes to stdout — subscribe is a local filter
        // registration. The daemon's subscriber list is fixed at
        // boot from the manifest's broker.subscribe list.
        assert!(
            writer.snapshot().is_empty(),
            "subscribe must not write to stdout: {}",
            writer.snapshot()
        );
    }

    #[tokio::test]
    async fn broker_event_routes_to_matching_subscriber() {
        let broker = StdioBridgeBroker::with_writer(CapturedWriter::default());
        let mut sub = broker.subscribe("plugin.outbound.whatsapp.>").await.unwrap();

        let event = sample_event(
            "plugin.outbound.whatsapp.smoketest",
            serde_json::json!({"hello": "world"}),
        );
        let frame = format!(
            r#"{{"jsonrpc":"2.0","method":"broker.event","params":{{"topic":"{}","event":{}}}}}"#,
            event.topic,
            serde_json::to_string(&event).unwrap()
        );
        broker.handle_inbound_line(&frame).await;

        let received = tokio::time::timeout(Duration::from_millis(200), sub.next())
            .await
            .expect("sub.next did not yield within timeout")
            .expect("subscription returned None");
        assert_eq!(received.topic, "plugin.outbound.whatsapp.smoketest");
        assert_eq!(received.payload, serde_json::json!({"hello": "world"}));
    }

    #[tokio::test]
    async fn broker_event_not_matching_pattern_dropped() {
        let broker = StdioBridgeBroker::with_writer(CapturedWriter::default());
        let mut sub = broker.subscribe("plugin.outbound.whatsapp.>").await.unwrap();

        // Event on telegram outbound — should NOT match whatsapp pattern.
        let event = sample_event("plugin.outbound.telegram.bot1", serde_json::json!({}));
        let frame = format!(
            r#"{{"jsonrpc":"2.0","method":"broker.event","params":{{"topic":"{}","event":{}}}}}"#,
            event.topic,
            serde_json::to_string(&event).unwrap()
        );
        broker.handle_inbound_line(&frame).await;

        let result = tokio::time::timeout(Duration::from_millis(50), sub.next()).await;
        assert!(
            result.is_err(),
            "telegram event must not arrive at whatsapp subscriber: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn multiple_subscribers_same_pattern_all_receive() {
        let broker = StdioBridgeBroker::with_writer(CapturedWriter::default());
        let mut sub_a = broker.subscribe("plugin.outbound.test.>").await.unwrap();
        let mut sub_b = broker.subscribe("plugin.outbound.test.>").await.unwrap();

        let event = sample_event(
            "plugin.outbound.test.bot",
            serde_json::json!({"i": 42}),
        );
        let frame = format!(
            r#"{{"jsonrpc":"2.0","method":"broker.event","params":{{"topic":"{}","event":{}}}}}"#,
            event.topic,
            serde_json::to_string(&event).unwrap()
        );
        broker.handle_inbound_line(&frame).await;

        let a = tokio::time::timeout(Duration::from_millis(200), sub_a.next())
            .await
            .unwrap()
            .unwrap();
        let b = tokio::time::timeout(Duration::from_millis(200), sub_b.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(a.payload, serde_json::json!({"i": 42}));
        assert_eq!(b.payload, serde_json::json!({"i": 42}));
    }

    #[tokio::test]
    async fn dropped_subscriber_pruned_on_next_event() {
        let broker = StdioBridgeBroker::with_writer(CapturedWriter::default());
        // Subscribe + immediately drop to close the receiver.
        let dropped_sub = broker.subscribe("plugin.outbound.test.>").await.unwrap();
        let mut live_sub = broker.subscribe("plugin.outbound.test.>").await.unwrap();
        drop(dropped_sub);

        // Push an event; the dead subscriber must be pruned and
        // the live one still receives.
        let event = sample_event("plugin.outbound.test.x", serde_json::json!({}));
        let frame = format!(
            r#"{{"jsonrpc":"2.0","method":"broker.event","params":{{"topic":"{}","event":{}}}}}"#,
            event.topic,
            serde_json::to_string(&event).unwrap()
        );
        broker.handle_inbound_line(&frame).await;

        live_sub.next().await.expect("live subscriber must receive event");
        // After dispatch, the subscribers vec should be down to 1.
        let subs = broker.inner.subscribers.lock().await;
        assert_eq!(subs.len(), 1, "dropped subscriber not pruned");
    }

    #[tokio::test]
    async fn malformed_inbound_line_is_dropped_silently() {
        let broker = StdioBridgeBroker::with_writer(CapturedWriter::default());
        broker.handle_inbound_line("not even json").await;
        broker
            .handle_inbound_line(r#"{"jsonrpc":"2.0","method":"tool.invoke","params":{}}"#)
            .await;
        broker
            .handle_inbound_line(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#)
            .await;
        // Must not panic; no assertions needed.
    }

    #[tokio::test]
    async fn request_method_returns_unsupported_error() {
        let broker = StdioBridgeBroker::with_writer(CapturedWriter::default());
        let err = broker
            .request(
                "foo",
                Message::new("foo", serde_json::json!({})),
                Duration::from_millis(10),
            )
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

    #[tokio::test]
    async fn concurrent_publishes_serialize_through_writer_mutex() {
        let writer = CapturedWriter::default();
        let broker = StdioBridgeBroker::with_writer(writer.clone());

        let mut tasks = Vec::new();
        for i in 0..10 {
            let broker = broker.clone();
            tasks.push(tokio::spawn(async move {
                broker
                    .publish(
                        &format!("plugin.inbound.test.{i}"),
                        sample_event(&format!("plugin.inbound.test.{i}"), serde_json::json!({"i": i})),
                    )
                    .await
            }));
        }
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        let written = writer.snapshot();
        // Each publish writes exactly one line; verify the
        // writer captured 10 newline-terminated frames AND
        // every frame parses as a valid JSON object (no
        // interleaving across the mutex).
        let line_count = written.lines().count();
        assert_eq!(line_count, 10, "expected 10 lines, got {line_count}");
        for line in written.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line not valid JSON: {line} ({e})"));
            assert_eq!(parsed.get("method").and_then(|v| v.as_str()), Some("broker.publish"));
        }
    }
}
