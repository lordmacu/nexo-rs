//! Phase 82.11 — agent event emitter + in-process broadcast.
//!
//! `AgentEventEmitter` is the single hook point the rest of the
//! daemon calls when something interesting happens that microapps
//! with the right capability should hear about. v0 only emits
//! `TranscriptAppended` (from `TranscriptWriter::append_entry`).
//! Future kinds (batch jobs, output produced) plug into the same
//! trait without touching the firehose plumbing.
//!
//! Default production impl is [`BroadcastAgentEventEmitter`]: a
//! `tokio::sync::broadcast::Sender<AgentEventKind>` with a fixed
//! ring buffer. Subscribers that lag past the buffer get
//! `RecvError::Lagged(n)` — they're expected to call
//! `agent_events/read` to resync rather than panic.
//!
//! `NoopAgentEventEmitter` keeps the field optional in
//! `TranscriptWriter` ergonomic — pass it (instead of `None`) when
//! you want explicit "no-op, by design" instead of "I forgot to
//! wire one".

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use nexo_tool_meta::admin::agent_events::AgentEventKind;
use tokio::sync::broadcast;

/// Default broadcast channel capacity. Sized so a microapp that
/// briefly stalls (e.g. fsync on stdin during a UI redraw) can
/// catch up without lagging — a 256-frame backlog covers ~1 min
/// of typical chat traffic at 4 frames/s. Higher → more
/// resilient to lag, more memory; lower → faster
/// `RecvError::Lagged` signal. Tunable via builder.
pub const DEFAULT_BROADCAST_CAPACITY: usize = 256;

/// Common surface every emit pathway speaks. Implementations
/// must be cheap to call from any context — `emit` MUST NOT
/// block the writer thread.
#[async_trait]
pub trait AgentEventEmitter: Send + Sync + fmt::Debug {
    /// Best-effort fan-out. Implementations log and drop on
    /// transport failure; the caller (transcript writer, future
    /// batch runner, …) keeps going either way.
    async fn emit(&self, event: AgentEventKind);
}

/// No-op emitter — useful as the default when no firehose is
/// wired (tests, headless installs, daemons without admin RPC).
#[derive(Debug, Default, Clone)]
pub struct NoopAgentEventEmitter;

#[async_trait]
impl AgentEventEmitter for NoopAgentEventEmitter {
    async fn emit(&self, _event: AgentEventKind) {}
}

/// In-process broadcast emitter. One sender, fan-out to many
/// receivers. Wrapping `broadcast::Sender` directly means
/// receivers are `Clone`-free (via `subscribe()`), the channel
/// drops oldest on overflow (per tokio semantics), and the
/// sender clones cheaply (Arc inside).
pub struct BroadcastAgentEventEmitter {
    tx: broadcast::Sender<AgentEventKind>,
}

impl fmt::Debug for BroadcastAgentEventEmitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BroadcastAgentEventEmitter")
            .field("subscribers", &self.tx.receiver_count())
            .field("capacity", &self.tx.len())
            .finish_non_exhaustive()
    }
}

impl BroadcastAgentEventEmitter {
    /// Build with the default capacity (256). Boot wiring can
    /// override via [`Self::with_capacity`].
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BROADCAST_CAPACITY)
    }

    /// Build with a custom capacity. Panics on `0` to surface a
    /// clear "you didn't mean to disable the firehose" message
    /// — for true no-op pass [`NoopAgentEventEmitter`] instead.
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "broadcast capacity must be > 0");
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Subscribe a fresh receiver. Boot wiring calls this once
    /// per microapp that holds `transcripts_subscribe` /
    /// `agent_events_subscribe_all`.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEventKind> {
        self.tx.subscribe()
    }

    /// Current subscriber count — for boot diagnostics.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for BroadcastAgentEventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentEventEmitter for BroadcastAgentEventEmitter {
    async fn emit(&self, event: AgentEventKind) {
        // `Sender::send` returns Err only when there are zero
        // receivers — a daemon with no admin-RPC microapps is
        // the common case, so we silently drop the frame.
        let _ = self.tx.send(event);
    }
}

/// Convenience type alias used by builders that want to thread
/// the emitter through trait objects.
pub type SharedAgentEventEmitter = Arc<dyn AgentEventEmitter>;

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::admin::agent_events::TranscriptRole;
    use tokio::sync::broadcast::error::RecvError;
    use uuid::Uuid;

    fn sample_event(seq: u64, body: &str) -> AgentEventKind {
        AgentEventKind::TranscriptAppended {
            agent_id: "ana".into(),
            session_id: Uuid::nil(),
            seq,
            role: TranscriptRole::User,
            body: body.into(),
            sent_at_ms: 1_700_000_000_000 + seq,
            sender_id: None,
            source_plugin: "whatsapp".into(),
        }
    }

    #[tokio::test]
    async fn broadcast_emit_round_trips_through_subscriber() {
        let emitter = BroadcastAgentEventEmitter::new();
        let mut rx = emitter.subscribe();
        let evt = sample_event(0, "[REDACTED:phone] hola");
        emitter.emit(evt.clone()).await;
        let recv = rx.recv().await.unwrap();
        assert_eq!(recv, evt);
        // Body stayed redacted on the wire.
        if let AgentEventKind::TranscriptAppended { body, .. } = &recv {
            assert!(body.starts_with("[REDACTED:"));
        } else {
            panic!("expected TranscriptAppended");
        }
    }

    #[tokio::test]
    async fn broadcast_supports_multiple_subscribers() {
        let emitter = BroadcastAgentEventEmitter::new();
        let mut rx_a = emitter.subscribe();
        let mut rx_b = emitter.subscribe();
        emitter.emit(sample_event(0, "x")).await;
        emitter.emit(sample_event(1, "y")).await;
        for rx in [&mut rx_a, &mut rx_b] {
            let first = rx.recv().await.unwrap();
            let second = rx.recv().await.unwrap();
            assert!(matches!(
                first,
                AgentEventKind::TranscriptAppended { seq: 0, .. }
            ));
            assert!(matches!(
                second,
                AgentEventKind::TranscriptAppended { seq: 1, .. }
            ));
        }
    }

    #[tokio::test]
    async fn broadcast_lag_surfaces_as_lagged_recv_not_panic() {
        // Tiny capacity → cheap to overflow.
        let emitter = BroadcastAgentEventEmitter::with_capacity(2);
        let mut rx = emitter.subscribe();
        for i in 0..5 {
            emitter.emit(sample_event(i, "fill")).await;
        }
        // Tokio guarantees: first recv after overflow yields
        // `RecvError::Lagged(n)`, then receiver re-syncs.
        let first = rx.recv().await.unwrap_err();
        match first {
            RecvError::Lagged(n) => assert!(n >= 1, "should report at least 1 lagged frame"),
            other => panic!("expected Lagged, got {other:?}"),
        }
        // After re-sync the receiver continues from the oldest
        // surviving frame. Subscribers handle this by calling
        // agent_events/read with their last-seen seq.
        let resync = rx.recv().await.unwrap();
        assert!(matches!(
            resync,
            AgentEventKind::TranscriptAppended { .. }
        ));
    }

    #[tokio::test]
    async fn noop_emitter_silently_drops_event() {
        let emitter = NoopAgentEventEmitter;
        // Just asserting it doesn't panic / block.
        emitter.emit(sample_event(0, "x")).await;
    }
}
