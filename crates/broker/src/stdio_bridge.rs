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
//!   notification (no `id`, fire-and-forget). Daemon validates
//!   the topic against the plugin manifest's
//!   `[plugin.capabilities.broker].publish` allowlist and
//!   forwards onto its in-process broker.
//! - `broker.event { topic, event }` — daemon → subprocess
//!   notification. Daemon pre-subscribes at boot to every pattern
//!   in the manifest's `[plugin.capabilities.broker].subscribe`
//!   list and pushes one frame per matching event. The
//!   subprocess-side broker filters these locally by topic
//!   pattern using [`crate::topic::topic_matches`] (NATS-style
//!   `*` and `>` wildcards) and fans out to every matching
//!   `Subscription`.
//!
//! Output abstraction is `tokio::sync::mpsc::Sender<Value>` so the
//! broker is decoupled from any concrete `Write` type. The SDK's
//! `PluginAdapter` reuses its single async stdout writer for both
//! `tool.invoke` responses (existing) and `broker.publish`
//! notifications (new) by draining the mpsc end into the same
//! writer task — see `nexo-microapp-sdk::plugin::
//! with_stdio_bridge_broker` for the wiring.
//!
//! Inbound events arrive at the bridge either via
//! [`StdioBridgeBroker::handle_inbound_line`] (raw JSON line
//! parsed by the bridge) or [`StdioBridgeBroker::feed_event`]
//! (typed `(topic, event)` already parsed by the SDK's existing
//! dispatch loop). Plugins typically use the latter — the SDK
//! already parses the `broker.event` frame for the
//! `on_broker_event` callback.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use crate::handle::{BrokerHandle, Subscription};
use crate::topic::topic_matches;
use crate::types::{BrokerError, Event, Message};

/// Channel capacity per subscriber. Matches the LocalBroker
/// constant so a single-host deployment behaves identically
/// whether plugins are in-process or stdio-bridged.
const CHANNEL_CAPACITY: usize = 256;

/// Default channel capacity for the outbound JSON-RPC mpsc
/// (publish notifications). Generous because each frame is
/// already serialized into a `Value` upstream of this queue.
pub const DEFAULT_OUTBOUND_CAPACITY: usize = 256;

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

/// Subprocess-side broker that bridges through the parent
/// daemon's stdio JSON-RPC channel. Output goes through an
/// `mpsc::Sender<Value>` so the broker is decoupled from any
/// concrete writer — the SDK's plugin adapter consumes that
/// channel and writes through its single async stdout handle.
#[derive(Clone)]
pub struct StdioBridgeBroker {
    inner: Arc<Inner>,
}

struct Inner {
    /// Outbound JSON-RPC frames. The SDK's `PluginAdapter` owns
    /// the matching receiver and forwards each frame onto the
    /// process's stdout (using its existing async writer).
    out: mpsc::Sender<Value>,
    /// Live subscribers: (topic_pattern, mpsc Sender). Vec
    /// because lookups iterate all entries anyway (topic_matches
    /// is the discriminator); also lets the same pattern have
    /// multiple subscribers if a plugin opens parallel
    /// `Subscription`s on the same pattern.
    subscribers: AsyncMutex<Vec<(String, mpsc::Sender<Event>)>>,
}

impl StdioBridgeBroker {
    /// Construct a broker tied to a pre-existing outbound mpsc
    /// Sender. The matching Receiver is owned by the caller
    /// (typically the SDK's adapter, which drains it into the
    /// process stdout). Returns the broker; the caller is
    /// responsible for keeping the Receiver alive.
    pub fn new(out: mpsc::Sender<Value>) -> Self {
        Self {
            inner: Arc::new(Inner {
                out,
                subscribers: AsyncMutex::new(Vec::new()),
            }),
        }
    }

    /// Convenience constructor that creates a fresh mpsc channel
    /// of [`DEFAULT_OUTBOUND_CAPACITY`] and returns both the
    /// broker and the Receiver. The caller wires the Receiver
    /// into a forwarder task (typically inside
    /// `nexo-microapp-sdk::plugin::PluginAdapter::
    /// with_stdio_bridge_broker`).
    pub fn with_channel() -> (Self, mpsc::Receiver<Value>) {
        let (tx, rx) = mpsc::channel(DEFAULT_OUTBOUND_CAPACITY);
        (Self::new(tx), rx)
    }

    /// Feed a typed `(topic, event)` pair to all matching local
    /// subscribers. Used by the SDK's `on_broker_event`
    /// callback, which already parses the `broker.event` frame
    /// out of stdin. Bypasses JSON deserialization (the SDK
    /// already did it).
    pub async fn feed_event(&self, topic: String, event: Event) {
        self.dispatch_event(BrokerEventParams { topic, event })
            .await;
    }

    /// Parse one raw JSON-RPC line received from the daemon
    /// parent. Recognizes the `broker.event { topic, event }`
    /// notification shape; anything else is logged at TRACE and
    /// dropped. Provided for callers that bypass the SDK's
    /// dispatch loop (tests, embedded variants without a full
    /// PluginAdapter).
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
    /// dropped so the subscribers list stays bounded by live
    /// consumers.
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
        for idx in to_remove.into_iter().rev() {
            subscribers.swap_remove(idx);
        }
    }
}

#[async_trait]
impl BrokerHandle for StdioBridgeBroker {
    /// Fire-and-forget publish. Serializes the broker.publish
    /// notification + pushes it onto the outbound mpsc; the SDK's
    /// adapter writes it to stdout. Returns `Ok(())` once the
    /// frame is queued; the daemon's allowlist check + actual
    /// broker fanout happen asynchronously on its side. Follow-up
    /// `92.followup-publish-ack` will add an optional req/reply
    /// mode for callers that need per-publish confirmation.
    async fn publish(&self, topic: &str, event: Event) -> Result<(), BrokerError> {
        let frame = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "broker.publish",
            params: serde_json::json!({ "topic": topic, "event": event }),
        };
        let value = serde_json::to_value(&frame)
            .map_err(|e| BrokerError::SendError(format!("serialize broker.publish: {e}")))?;
        self.inner
            .out
            .send(value)
            .await
            .map_err(|e| BrokerError::SendError(format!("outbound mpsc closed: {e}")))?;
        Ok(())
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

    fn sample_event(topic: &str, payload: serde_json::Value) -> Event {
        Event::new(topic, "test", payload)
    }

    #[tokio::test]
    async fn publish_writes_notification_without_id() {
        let (broker, mut out_rx) = StdioBridgeBroker::with_channel();
        broker
            .publish(
                "plugin.inbound.test",
                sample_event("plugin.inbound.test", serde_json::json!({"k": "v"})),
            )
            .await
            .unwrap();
        let frame = out_rx.try_recv().expect("publish must enqueue a frame");
        let envelope = frame.as_object().expect("envelope is an object");
        assert!(
            !envelope.contains_key("id"),
            "publish must be a notification, not a request: {frame:#?}"
        );
        assert_eq!(
            envelope.get("method").and_then(Value::as_str),
            Some("broker.publish")
        );
        assert_eq!(
            envelope
                .get("params")
                .and_then(|p| p.get("topic"))
                .and_then(Value::as_str),
            Some("plugin.inbound.test")
        );
    }

    #[tokio::test]
    async fn subscribe_is_synchronous_no_outbound_frame() {
        let (broker, mut out_rx) = StdioBridgeBroker::with_channel();
        let sub = broker.subscribe("plugin.outbound.>").await.unwrap();
        assert_eq!(sub.topic, "plugin.outbound.>");
        // No frames must hit the outbound channel — subscribe is a
        // local-only filter registration.
        assert!(
            out_rx.try_recv().is_err(),
            "subscribe must not write to outbound mpsc"
        );
    }

    #[tokio::test]
    async fn feed_event_routes_to_matching_subscriber() {
        let (broker, _out_rx) = StdioBridgeBroker::with_channel();
        let mut sub = broker
            .subscribe("plugin.outbound.whatsapp.>")
            .await
            .unwrap();
        let event = sample_event(
            "plugin.outbound.whatsapp.smoketest",
            serde_json::json!({"hello": "world"}),
        );
        broker
            .feed_event("plugin.outbound.whatsapp.smoketest".to_string(), event)
            .await;
        let received = tokio::time::timeout(Duration::from_millis(200), sub.next())
            .await
            .expect("sub.next did not yield within timeout")
            .expect("subscription returned None");
        assert_eq!(received.payload, serde_json::json!({"hello": "world"}));
    }

    #[tokio::test]
    async fn handle_inbound_line_routes_broker_event() {
        let (broker, _out_rx) = StdioBridgeBroker::with_channel();
        let mut sub = broker.subscribe("plugin.outbound.test.>").await.unwrap();
        let event = sample_event("plugin.outbound.test.x", serde_json::json!({"i": 1}));
        let frame = format!(
            r#"{{"jsonrpc":"2.0","method":"broker.event","params":{{"topic":"{}","event":{}}}}}"#,
            event.topic,
            serde_json::to_string(&event).unwrap()
        );
        broker.handle_inbound_line(&frame).await;
        let recv = tokio::time::timeout(Duration::from_millis(200), sub.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recv.payload, serde_json::json!({"i": 1}));
    }

    #[tokio::test]
    async fn non_matching_pattern_dropped() {
        let (broker, _out_rx) = StdioBridgeBroker::with_channel();
        let mut sub = broker
            .subscribe("plugin.outbound.whatsapp.>")
            .await
            .unwrap();
        broker
            .feed_event(
                "plugin.outbound.telegram.bot1".to_string(),
                sample_event("plugin.outbound.telegram.bot1", serde_json::json!({})),
            )
            .await;
        let result = tokio::time::timeout(Duration::from_millis(50), sub.next()).await;
        assert!(
            result.is_err(),
            "telegram event must not arrive at whatsapp subscriber"
        );
    }

    #[tokio::test]
    async fn multiple_subscribers_same_pattern_all_receive() {
        let (broker, _out_rx) = StdioBridgeBroker::with_channel();
        let mut sub_a = broker.subscribe("plugin.outbound.test.>").await.unwrap();
        let mut sub_b = broker.subscribe("plugin.outbound.test.>").await.unwrap();
        broker
            .feed_event(
                "plugin.outbound.test.bot".to_string(),
                sample_event("plugin.outbound.test.bot", serde_json::json!({"i": 42})),
            )
            .await;
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
        let (broker, _out_rx) = StdioBridgeBroker::with_channel();
        let dropped = broker.subscribe("plugin.outbound.test.>").await.unwrap();
        let mut live = broker.subscribe("plugin.outbound.test.>").await.unwrap();
        drop(dropped);
        broker
            .feed_event(
                "plugin.outbound.test.x".to_string(),
                sample_event("plugin.outbound.test.x", serde_json::json!({})),
            )
            .await;
        live.next().await.expect("live subscriber must receive");
        let subs = broker.inner.subscribers.lock().await;
        assert_eq!(subs.len(), 1, "dropped subscriber not pruned");
    }

    #[tokio::test]
    async fn malformed_inbound_line_is_dropped_silently() {
        let (broker, _out_rx) = StdioBridgeBroker::with_channel();
        broker.handle_inbound_line("not even json").await;
        broker
            .handle_inbound_line(r#"{"jsonrpc":"2.0","method":"tool.invoke","params":{}}"#)
            .await;
        broker
            .handle_inbound_line(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#)
            .await;
    }

    #[tokio::test]
    async fn request_method_returns_unsupported_error() {
        let (broker, _out_rx) = StdioBridgeBroker::with_channel();
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
                assert!(msg.contains("stdio_bridge"), "msg={msg}");
            }
            other => panic!("expected RequestTimeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn closed_outbound_returns_send_error() {
        let (broker, out_rx) = StdioBridgeBroker::with_channel();
        drop(out_rx); // simulate adapter shutdown / writer task exited
        let err = broker
            .publish(
                "plugin.inbound.test",
                sample_event("plugin.inbound.test", serde_json::json!({})),
            )
            .await
            .expect_err("publish after outbound closed must fail");
        match err {
            BrokerError::SendError(msg) => {
                assert!(msg.contains("outbound mpsc closed"), "msg={msg}");
            }
            other => panic!("expected SendError, got {other:?}"),
        }
    }
}
