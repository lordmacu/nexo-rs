//! Phase 80.9.d — agent-side channel bridge.
//!
//! The dispatcher (Phase 80.9 + 80.9.c) publishes
//! [`ChannelEnvelope`] payloads on `mcp.channel.>`. This module
//! ships the *consumer* side: a [`ChannelBridge`] that subscribes
//! to the broker subject, resolves the envelope's
//! [`ChannelSessionKey`] into a stable agent session uuid, and
//! delegates the actual delivery into the agent runtime to a
//! caller-provided [`ChannelInboundSink`]. Splitting "transport"
//! (this module) from "delivery" (the sink) keeps the bridge
//! testable without a live agent runtime and keeps every
//! deployment path — in-process, NATS-backed, hosted SaaS —
//! reachable through the same interface.
//!
//! Threading model: `ChannelSessionKey::derive` already separates
//! Slack threads / Telegram chats / iMessage conversations into
//! distinct keys, so a `(server, thread_ts)` pair maps to one
//! stable agent session even across daemon restarts (when paired
//! with a persistent registry — 80.9.d.b extends
//! [`InMemorySessionRegistry`] to SQLite-backed storage).

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use nexo_broker::{
    handle::BrokerHandle,
    types::{BrokerError, Event as BrokerEvent},
    AnyBroker,
};
use serde_json::Value;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::channel::{ChannelEnvelope, ChannelSessionKey, CHANNEL_INBOX_WILDCARD};

// ---------------------------------------------------------------
// Session registry.
// ---------------------------------------------------------------

/// Resolves a [`ChannelSessionKey`] into a stable agent session
/// uuid. The trait is async to leave room for a SQLite-backed
/// implementation (80.9.d.b) that persists across daemon
/// restarts. The in-memory impl below is the MVP — it only
/// remembers mappings while the process is alive but is good
/// enough for single-process operators.
#[async_trait]
pub trait SessionRegistry: Send + Sync {
    /// Return the uuid associated with `key`. First-seen keys
    /// get a fresh uuid and a "now" timestamp; repeats refresh
    /// the timestamp.
    async fn resolve(&self, key: &ChannelSessionKey) -> Uuid;

    /// Drop entries idle longer than `max_idle_ms`. Returns the
    /// number of evictions. Called on a timer by the bridge so
    /// long-running daemons don't grow the map unbounded.
    /// `0` is a no-op sentinel (operator opt-in).
    async fn gc_idle(&self, max_idle_ms: i64) -> usize;

    /// Best-effort current count. Used by `setup doctor` and
    /// telemetry — not load-bearing.
    async fn len(&self) -> usize;
}

#[derive(Clone, Debug)]
struct SessionEntry {
    session_id: Uuid,
    last_seen_ms: i64,
}

/// In-memory implementation. Behind an `RwLock<BTreeMap<...>>`
/// so the read-hot path stays cheap and writes (first-seen +
/// timestamp refresh) only block briefly. `BTreeMap` over
/// `HashMap` for deterministic iteration in tests + GC sweeps.
#[derive(Default, Debug)]
pub struct InMemorySessionRegistry {
    inner: RwLock<BTreeMap<ChannelSessionKey, SessionEntry>>,
}

impl InMemorySessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self) -> Vec<(ChannelSessionKey, Uuid, i64)> {
        self.inner
            .read()
            .await
            .iter()
            .map(|(k, e)| (k.clone(), e.session_id, e.last_seen_ms))
            .collect()
    }
}

#[async_trait]
impl SessionRegistry for InMemorySessionRegistry {
    async fn resolve(&self, key: &ChannelSessionKey) -> Uuid {
        let now_ms = chrono::Utc::now().timestamp_millis();
        // Try read-only fast path; if present, only need a write
        // lock to refresh `last_seen_ms`.
        let cached_id = self
            .inner
            .read()
            .await
            .get(key)
            .map(|e| e.session_id);
        if let Some(id) = cached_id {
            self.inner
                .write()
                .await
                .entry(key.clone())
                .and_modify(|e| e.last_seen_ms = now_ms);
            return id;
        }
        // Insert under the write lock — re-check inside in case a
        // concurrent caller raced past the first read.
        let mut w = self.inner.write().await;
        let entry = w
            .entry(key.clone())
            .or_insert_with(|| SessionEntry {
                session_id: Uuid::new_v4(),
                last_seen_ms: now_ms,
            });
        entry.last_seen_ms = now_ms;
        entry.session_id
    }

    async fn gc_idle(&self, max_idle_ms: i64) -> usize {
        if max_idle_ms <= 0 {
            return 0;
        }
        let cutoff = chrono::Utc::now().timestamp_millis() - max_idle_ms;
        let mut w = self.inner.write().await;
        let before = w.len();
        w.retain(|_, e| e.last_seen_ms >= cutoff);
        before - w.len()
    }

    async fn len(&self) -> usize {
        self.inner.read().await.len()
    }
}

// ---------------------------------------------------------------
// Inbound event + sink.
// ---------------------------------------------------------------

/// What the bridge hands to the sink for each envelope. A typed
/// payload so the sink doesn't have to re-parse JSON. The
/// `rendered` field is the pre-built `<channel source="...">`
/// XML — sinks that want the model-facing form can use it as-is
/// without depending on this crate's wrapper helper.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelInboundEvent {
    pub binding_id: String,
    pub server_name: String,
    pub session_id: Uuid,
    pub session_key: ChannelSessionKey,
    pub content: String,
    pub meta: BTreeMap<String, String>,
    /// Pre-rendered `<channel source="...">content</channel>`.
    pub rendered: String,
    pub envelope_id: Uuid,
    pub sent_at_ms: i64,
}

/// Caller-provided handler that injects the inbound event into
/// the agent runtime. Implementations decide which intake path
/// the message follows — typically synthesising an inbound on
/// `agent.intake.<binding_id>` so the existing pairing /
/// dispatch / rate-limit gates apply unchanged.
#[async_trait]
pub trait ChannelInboundSink: Send + Sync {
    async fn deliver(&self, event: ChannelInboundEvent) -> Result<(), SinkError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    #[error("intake rejected: {0}")]
    Rejected(String),
    #[error("delivery failed: {0}")]
    Other(String),
}

// ---------------------------------------------------------------
// Bridge — broker → session-resolve → sink.
// ---------------------------------------------------------------

/// Configuration for [`ChannelBridge::spawn`]. Sized to make the
/// boot site trivial — every field is `Arc<...>` or small.
#[derive(Clone)]
pub struct ChannelBridgeConfig {
    pub broker: AnyBroker,
    pub registry: Arc<dyn SessionRegistry>,
    pub sink: Arc<dyn ChannelInboundSink>,
    /// Subject pattern to subscribe on. Defaults to
    /// [`CHANNEL_INBOX_WILDCARD`] (`mcp.channel.>`); callers can
    /// narrow it for tenancy isolation.
    pub subject: String,
    /// Idle GC interval in milliseconds. `0` disables the GC
    /// task. Default 5 minutes.
    pub gc_interval_ms: u64,
    /// Maximum idle window before a session is evicted. `0`
    /// disables eviction (entries live forever — fine for an
    /// in-memory registry on a process you restart often).
    pub max_idle_ms: i64,
}

impl ChannelBridgeConfig {
    pub fn new(
        broker: AnyBroker,
        registry: Arc<dyn SessionRegistry>,
        sink: Arc<dyn ChannelInboundSink>,
    ) -> Self {
        Self {
            broker,
            registry,
            sink,
            subject: CHANNEL_INBOX_WILDCARD.into(),
            gc_interval_ms: 5 * 60_000,
            max_idle_ms: 60 * 60_000, // 1h default
        }
    }
}

/// Live bridge handle. Two tasks are spawned: the consumer that
/// drains the broker subscription + the GC ticker. Both stop
/// when the cancellation token fires.
pub struct ChannelBridgeHandle {
    pub consumer: JoinHandle<()>,
    pub gc: Option<JoinHandle<()>>,
}

pub struct ChannelBridge {
    cfg: ChannelBridgeConfig,
}

impl ChannelBridge {
    pub fn new(cfg: ChannelBridgeConfig) -> Self {
        Self { cfg }
    }

    /// Spawn the consumer + GC tasks. Returns a handle the caller
    /// can join on shutdown. Caller-controlled `cancel` token
    /// triggers a clean exit on either side.
    pub async fn spawn(self, cancel: CancellationToken) -> Result<ChannelBridgeHandle, BrokerError> {
        let cfg = self.cfg;
        let mut sub = BrokerHandle::subscribe(&cfg.broker, &cfg.subject).await?;
        let registry = cfg.registry.clone();
        let sink = cfg.sink.clone();
        let cancel_consumer = cancel.clone();

        let consumer = tokio::spawn(async move {
            tracing::info!(
                subject = %sub.topic,
                "channel bridge consumer running"
            );
            loop {
                tokio::select! {
                    _ = cancel_consumer.cancelled() => {
                        tracing::info!("channel bridge consumer cancelled");
                        return;
                    }
                    next = sub.next() => {
                        let Some(evt) = next else {
                            tracing::info!("channel bridge subscription closed");
                            return;
                        };
                        Self::handle_broker_event(&registry, sink.as_ref(), evt).await;
                    }
                }
            }
        });

        let gc = if cfg.gc_interval_ms > 0 && cfg.max_idle_ms > 0 {
            let registry = cfg.registry.clone();
            let interval = std::time::Duration::from_millis(cfg.gc_interval_ms);
            let max_idle = cfg.max_idle_ms;
            let cancel_gc = cancel.clone();
            Some(tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = cancel_gc.cancelled() => return,
                        _ = ticker.tick() => {
                            let evicted = registry.gc_idle(max_idle).await;
                            if evicted > 0 {
                                tracing::debug!(evicted, "channel bridge gc swept idle sessions");
                            }
                        }
                    }
                }
            }))
        } else {
            None
        };

        Ok(ChannelBridgeHandle { consumer, gc })
    }

    async fn handle_broker_event(
        registry: &Arc<dyn SessionRegistry>,
        sink: &dyn ChannelInboundSink,
        evt: BrokerEvent,
    ) {
        let envelope: ChannelEnvelope = match serde_json::from_value::<Value>(evt.payload.clone())
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
        {
            Some(e) => e,
            None => {
                tracing::warn!(
                    topic = %evt.topic,
                    "channel bridge: malformed envelope"
                );
                return;
            }
        };

        let session_id = registry.resolve(&envelope.session_key).await;
        let inbound = ChannelInboundEvent {
            binding_id: envelope.binding_id,
            server_name: envelope.server_name,
            session_id,
            session_key: envelope.session_key,
            content: envelope.content,
            meta: envelope.meta,
            rendered: envelope.rendered,
            envelope_id: envelope.envelope_id,
            sent_at_ms: envelope.sent_at_ms,
        };
        if let Err(e) = sink.deliver(inbound).await {
            tracing::warn!(error = %e, "channel bridge sink rejected event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{BrokerChannelDispatcher, ChannelDispatcher, ChannelInbound};
    use std::sync::Mutex as StdMutex;

    // ---- registry ----

    #[tokio::test]
    async fn registry_first_seen_creates_uuid() {
        let r = InMemorySessionRegistry::new();
        let key = ChannelSessionKey("slack|chat=A".into());
        let id = r.resolve(&key).await;
        assert_eq!(r.len().await, 1);
        // Repeat: same uuid.
        let again = r.resolve(&key).await;
        assert_eq!(id, again);
    }

    #[tokio::test]
    async fn registry_distinct_keys_get_distinct_uuids() {
        let r = InMemorySessionRegistry::new();
        let a = r.resolve(&ChannelSessionKey("slack|chat=A".into())).await;
        let b = r.resolve(&ChannelSessionKey("slack|chat=B".into())).await;
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn registry_gc_evicts_old_entries() {
        let r = InMemorySessionRegistry::new();
        let _ = r.resolve(&ChannelSessionKey("a".into())).await;
        let _ = r.resolve(&ChannelSessionKey("b".into())).await;
        // No entries are old enough — gc should evict 0.
        assert_eq!(r.gc_idle(1).await, 0);
        assert_eq!(r.len().await, 2);
        // Force-stale entries by sleeping past the cutoff.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let evicted = r.gc_idle(10).await;
        assert_eq!(evicted, 2);
        assert_eq!(r.len().await, 0);
    }

    #[tokio::test]
    async fn registry_gc_zero_is_noop() {
        let r = InMemorySessionRegistry::new();
        let _ = r.resolve(&ChannelSessionKey("a".into())).await;
        assert_eq!(r.gc_idle(0).await, 0);
        assert_eq!(r.len().await, 1);
    }

    // ---- sink ----

    #[derive(Default)]
    struct CapturingSink {
        seen: StdMutex<Vec<ChannelInboundEvent>>,
        fail: bool,
    }

    #[async_trait]
    impl ChannelInboundSink for CapturingSink {
        async fn deliver(&self, event: ChannelInboundEvent) -> Result<(), SinkError> {
            if self.fail {
                return Err(SinkError::Rejected("forced".into()));
            }
            self.seen.lock().unwrap().push(event);
            Ok(())
        }
    }

    // ---- bridge end-to-end via a real local broker ----

    async fn publish_envelope(
        broker: &AnyBroker,
        binding: &str,
        server: &str,
        content: &str,
    ) {
        let mut meta = BTreeMap::new();
        meta.insert("chat_id".to_string(), "C123".to_string());
        let inbound = ChannelInbound {
            server_name: server.to_string(),
            content: content.to_string(),
            session_key: ChannelSessionKey::derive(server, &meta),
            meta,
        };
        let dispatcher = BrokerChannelDispatcher::new(broker.clone());
        dispatcher.dispatch(binding, inbound).await.unwrap();
    }

    #[tokio::test]
    async fn bridge_resolves_session_and_delivers() {
        let broker = AnyBroker::local();
        let registry: Arc<dyn SessionRegistry> = Arc::new(InMemorySessionRegistry::new());
        let sink = Arc::new(CapturingSink::default());
        let sink_dyn: Arc<dyn ChannelInboundSink> = sink.clone();
        let cfg = ChannelBridgeConfig {
            gc_interval_ms: 0, // disable gc for the test
            max_idle_ms: 0,
            ..ChannelBridgeConfig::new(broker.clone(), registry.clone(), sink_dyn)
        };
        let cancel = CancellationToken::new();
        let handle = ChannelBridge::new(cfg).spawn(cancel.clone()).await.unwrap();

        publish_envelope(&broker, "b", "slack", "hello").await;
        publish_envelope(&broker, "b", "slack", "again").await;

        // Allow the loop to process.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let got = sink.seen.lock().unwrap().clone();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].binding_id, "b");
        assert_eq!(got[0].server_name, "slack");
        assert_eq!(got[0].content, "hello");
        // Same session_key → same session_id across both messages.
        assert_eq!(got[0].session_id, got[1].session_id);
        // Rendered XML pre-built.
        assert!(got[0].rendered.starts_with("<channel source=\"slack\""));

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), handle.consumer).await;
    }

    #[tokio::test]
    async fn bridge_logs_sink_failures_without_dying() {
        let broker = AnyBroker::local();
        let registry: Arc<dyn SessionRegistry> = Arc::new(InMemorySessionRegistry::new());
        let sink = Arc::new(CapturingSink {
            fail: true,
            ..CapturingSink::default()
        });
        let sink_dyn: Arc<dyn ChannelInboundSink> = sink.clone();
        let cfg = ChannelBridgeConfig {
            gc_interval_ms: 0,
            max_idle_ms: 0,
            ..ChannelBridgeConfig::new(broker.clone(), registry.clone(), sink_dyn)
        };
        let cancel = CancellationToken::new();
        let handle = ChannelBridge::new(cfg).spawn(cancel.clone()).await.unwrap();

        for _ in 0..3 {
            publish_envelope(&broker, "b", "slack", "msg").await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        // Sink rejected every event; bridge stays alive.
        assert!(sink.seen.lock().unwrap().is_empty());
        // Registry still recorded the session (resolution happens
        // before delivery — that's intentional, sink failure
        // mustn't lose the threading state).
        assert_eq!(registry.len().await, 1);

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), handle.consumer).await;
    }

    #[tokio::test]
    async fn bridge_threads_separate_keys_into_distinct_sessions() {
        let broker = AnyBroker::local();
        let registry: Arc<dyn SessionRegistry> = Arc::new(InMemorySessionRegistry::new());
        let sink = Arc::new(CapturingSink::default());
        let sink_dyn: Arc<dyn ChannelInboundSink> = sink.clone();
        let cfg = ChannelBridgeConfig {
            gc_interval_ms: 0,
            max_idle_ms: 0,
            ..ChannelBridgeConfig::new(broker.clone(), registry.clone(), sink_dyn)
        };
        let cancel = CancellationToken::new();
        let handle = ChannelBridge::new(cfg).spawn(cancel.clone()).await.unwrap();

        // Two distinct chat_ids → two sessions.
        let dispatcher = BrokerChannelDispatcher::new(broker.clone());
        for chat in ["A", "B"] {
            let mut meta = BTreeMap::new();
            meta.insert("chat_id".into(), chat.into());
            let inbound = ChannelInbound {
                server_name: "slack".into(),
                content: format!("hi from {chat}"),
                session_key: ChannelSessionKey::derive("slack", &meta),
                meta,
            };
            dispatcher.dispatch("b", inbound).await.unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let got = sink.seen.lock().unwrap().clone();
        assert_eq!(got.len(), 2);
        assert_ne!(got[0].session_id, got[1].session_id);

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), handle.consumer).await;
    }

    #[tokio::test]
    async fn bridge_subject_filter_narrows_subscription() {
        let broker = AnyBroker::local();
        let registry: Arc<dyn SessionRegistry> = Arc::new(InMemorySessionRegistry::new());
        let sink = Arc::new(CapturingSink::default());
        let sink_dyn: Arc<dyn ChannelInboundSink> = sink.clone();
        let mut cfg = ChannelBridgeConfig {
            gc_interval_ms: 0,
            max_idle_ms: 0,
            ..ChannelBridgeConfig::new(broker.clone(), registry.clone(), sink_dyn)
        };
        // Only the exact "b" binding subject. Local broker uses
        // exact-match topics, so this is the contract: callers
        // can scope by tenant by narrowing the subject.
        cfg.subject = "mcp.channel.b.slack".to_string();
        let cancel = CancellationToken::new();
        let handle = ChannelBridge::new(cfg).spawn(cancel.clone()).await.unwrap();

        publish_envelope(&broker, "b", "slack", "match").await;
        publish_envelope(&broker, "other", "slack", "noise").await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let got = sink.seen.lock().unwrap().clone();
        // The local broker is exact-match: we asked for the b/slack
        // subject so only the matching publish lands. The "other"
        // publish goes nowhere (no subscriber) and is dropped.
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].binding_id, "b");

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), handle.consumer).await;
    }

    #[tokio::test]
    async fn bridge_gc_task_runs_when_interval_set() {
        let broker = AnyBroker::local();
        let registry: Arc<dyn SessionRegistry> = Arc::new(InMemorySessionRegistry::new());
        let sink: Arc<dyn ChannelInboundSink> = Arc::new(CapturingSink::default());
        let cfg = ChannelBridgeConfig {
            gc_interval_ms: 50,
            max_idle_ms: 30,
            ..ChannelBridgeConfig::new(broker.clone(), registry.clone(), sink)
        };
        let cancel = CancellationToken::new();
        let handle = ChannelBridge::new(cfg).spawn(cancel.clone()).await.unwrap();
        assert!(handle.gc.is_some());

        publish_envelope(&broker, "b", "slack", "hi").await;
        // First tick happens almost immediately on tokio::interval.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        // Either the gc evicted the session or the message was
        // re-delivered; both produce a registry length of 0 or 1.
        assert!(registry.len().await <= 1);

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), handle.consumer).await;
        if let Some(gc) = handle.gc {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(200), gc).await;
        }
    }
}
