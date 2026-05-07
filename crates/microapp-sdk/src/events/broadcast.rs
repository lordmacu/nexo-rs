//! `EventBroadcastState<T>` + persisting listener bridge.
//!
//! The "firehose" pattern: a microapp subscribes to a daemon
//! notification, decodes each frame into a typed `T`, fans the
//! typed value out to in-process SSE subscribers via a
//! [`tokio::sync::broadcast::Sender`], AND persists every event
//! to a SQLite append-log so HTTP clients can backfill.
//!
//! [`build_persisting_listener`] returns the
//! `Arc<dyn Fn(serde_json::Value)>` shape
//! `Microapp::with_notification_listener` expects. It composes
//! [`crate::notifications::build_broadcast_listener`] (decode +
//! broadcast) with a spawned task that taps the broadcast and
//! writes to [`crate::events::EventStore`] — so backfill stays
//! consistent with the live broadcast without re-decoding the
//! value twice.
//!
//! MUST be invoked from inside a Tokio runtime; the listener
//! spawns a background task via `tokio::spawn`.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

use super::metadata::EventMetadata;
use super::store::EventStore;
use crate::notifications::build_broadcast_listener;

/// Default broadcast channel capacity. Lagged subscribers receive
/// `RecvError::Lagged(n)` and reconcile via the SQLite backfill
/// path (`since_ms` query).
pub const DEFAULT_BROADCAST_CAPACITY: usize = 256;

/// Shared state behind every consumer of the persisting listener:
/// the SQLite store + the broadcast `Sender` whose `subscribe()`
/// each SSE handler calls.
///
/// Cheap to clone via `Arc`.
pub struct EventBroadcastState<T> {
    /// Append-log used for backfill queries. Sharing the same
    /// `Arc<EventStore<T>>` across the listener + HTTP routes
    /// keeps the read side hitting one pool.
    pub store: Arc<EventStore<T>>,
    /// Tokio broadcast channel. SSE route handlers call
    /// `state.broadcast.subscribe()` once per connection.
    pub broadcast: broadcast::Sender<T>,
}

impl<T> std::fmt::Debug for EventBroadcastState<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBroadcastState")
            .field("subscribers", &self.broadcast.receiver_count())
            .finish_non_exhaustive()
    }
}

impl<T> EventBroadcastState<T>
where
    T: Send + Sync + Clone + 'static,
{
    /// Build a fresh state with a broadcast channel of the given
    /// capacity. Use [`DEFAULT_BROADCAST_CAPACITY`] when you don't
    /// need to override.
    pub fn new(store: Arc<EventStore<T>>, capacity: usize) -> Self {
        let (broadcast, _) = broadcast::channel(capacity.max(1));
        Self { store, broadcast }
    }
}

/// Build the notification listener closure for
/// `Microapp::with_notification_listener(method_name, …)`.
///
/// The returned `Arc<dyn Fn(Value)>` decodes each frame, fans it
/// out via the broadcast, and asynchronously appends to the
/// SQLite store. Decode failures warn-log and drop the frame; SQL
/// failures warn-log without breaking the broadcast.
///
/// `method_name` is forwarded to
/// [`crate::notifications::build_broadcast_listener`] for log
/// breadcrumbs only.
///
/// MUST be called from inside a Tokio runtime — the persistence
/// tap is `tokio::spawn`-ed at call time.
pub fn build_persisting_listener<T>(
    method_name: &'static str,
    state: Arc<EventBroadcastState<T>>,
) -> Arc<dyn Fn(Value) + Send + Sync>
where
    T: EventMetadata + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let inner = build_broadcast_listener::<T>(method_name, state.broadcast.clone());
    // Subscribe ONCE here so the persistence side-effect rides on
    // the typed broadcast — no double JSON decode.
    let mut tap = state.broadcast.subscribe();
    let store = state.store.clone();
    tokio::spawn(async move {
        loop {
            match tap.recv().await {
                Ok(event) => {
                    if let Err(e) = store.append(&event).await {
                        tracing::warn!(
                            method = method_name,
                            error = %e,
                            "events::build_persisting_listener: store append failed",
                        );
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::store::{EventStore, ListFilter, DEFAULT_TABLE};
    use nexo_tool_meta::admin::agent_events::{AgentEventKind, TranscriptRole};
    use uuid::Uuid;

    fn transcript_value(seq: u64) -> Value {
        serde_json::json!({
            "kind": "transcript_appended",
            "agent_id": "ana",
            "session_id": Uuid::nil(),
            "seq": seq,
            "role": "user",
            "body": "hi",
            "sent_at_ms": 1000 + seq,
            "source_plugin": "whatsapp"
        })
    }

    async fn fresh_state() -> Arc<EventBroadcastState<AgentEventKind>> {
        let store = Arc::new(EventStore::open_memory(DEFAULT_TABLE).await.unwrap());
        Arc::new(EventBroadcastState::new(store, 16))
    }

    #[tokio::test]
    async fn listener_appends_to_store_and_broadcasts() {
        let state = fresh_state().await;
        let mut rx = state.broadcast.subscribe();
        let listener = build_persisting_listener("nexo/notify/agent_event", Arc::clone(&state));
        listener(transcript_value(0));
        // Live broadcast got the event.
        let live = rx.recv().await.unwrap();
        assert!(matches!(live, AgentEventKind::TranscriptAppended { .. }));
        // Detached append eventually lands.
        for _ in 0..50 {
            let rows = state
                .store
                .list(&ListFilter {
                    agent_id: Some("ana".into()),
                    limit: 10,
                    ..Default::default()
                })
                .await
                .unwrap();
            if rows.len() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("store append never landed");
    }

    #[tokio::test]
    async fn listener_skips_malformed_payload() {
        let state = fresh_state().await;
        let listener = build_persisting_listener("nexo/notify/agent_event", Arc::clone(&state));
        listener(serde_json::json!({"not": "a real event"}));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let rows = state
            .store
            .list(&ListFilter {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn listener_continues_broadcast_when_no_subscribers() {
        let state = fresh_state().await;
        let listener = build_persisting_listener("nexo/notify/agent_event", Arc::clone(&state));
        // Zero external subscribers — the persistence tap is
        // still attached, so append must land.
        listener(transcript_value(0));
        for _ in 0..50 {
            let rows = state
                .store
                .list(&ListFilter {
                    agent_id: Some("ana".into()),
                    limit: 10,
                    ..Default::default()
                })
                .await
                .unwrap();
            if rows.len() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("append did not land despite zero external subscribers");
    }

    // Silence unused warnings for `TranscriptRole` import if the
    // test variant set ever shrinks.
    #[allow(dead_code)]
    fn _force_use(_: TranscriptRole) {}
}
