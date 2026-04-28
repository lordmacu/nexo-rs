//! Phase 76.1 — HTTP session manager.
//!
//! Owns the per-session state shared between `POST /mcp`,
//! `GET /mcp` (SSE), and `DELETE /mcp`. Sessions live in a
//! `DashMap` capped at `max_sessions`; a janitor task expires
//! idle / over-lifetime entries every 30 s. Server shutdown
//! broadcasts a `Shutdown` event to every active SSE consumer
//! before tearing down listeners.
//!
//! Phase 76.8 — when an [`SessionEventStore`] is wired in, every
//! emit() call assigns a per-session monotonic `seq`, persists the
//! frame, and ships [`SessionEvent::IndexedMessage`] over the
//! broadcast so SSE handlers can label the wire frame with `id:`
//! and reconnecting clients can replay the gap via `Last-Event-ID`.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dashmap::{DashMap, DashSet};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::event_store::SessionEventStore;
use super::http_config::HttpTransportConfig;

/// One server-pushed event delivered through the per-session
/// broadcast channel. `Message` is the JSON-RPC payload itself
/// (used by `notifications/progress`, `tools/list_changed`,
/// `resources/updated`, …); the others are transport-level
/// lifecycle signals consumed by SSE handlers.
///
/// Phase 76.7 promoted this to `pub` so the dispatcher
/// (`DispatchContext.session_sink`) can carry a
/// `broadcast::Sender<SessionEvent>` across the public API
/// boundary. External pattern matches should use a wildcard arm
/// — variants may grow over time.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SessionEvent {
    /// JSON-RPC envelope to forward as `event: message` — legacy
    /// path with no replay. `progress.rs` still emits this because
    /// per-call progress is by-design ephemeral. SSE frames built
    /// from `Message` carry no `id:` line.
    Message(serde_json::Value),
    /// Phase 76.8 — JSON-RPC envelope tagged with the per-session
    /// monotonic event-id. SSE frames built from `IndexedMessage`
    /// carry `id: <seq>` so reconnecting clients can resume via
    /// `Last-Event-ID`.
    IndexedMessage { seq: u64, body: serde_json::Value },
    /// Server is going down gracefully.
    Shutdown { reason: String },
    /// SSE stream should close (max-age, explicit close, idle).
    EndOfStream { reason: String },
}

pub(crate) struct HttpSession {
    pub(crate) id: String,
    pub(crate) created_at: Instant,
    last_seen: Mutex<Instant>,
    pub(crate) notif_tx: broadcast::Sender<SessionEvent>,
    pub(crate) sse_active: AtomicUsize,
    pub(crate) cancel: CancellationToken,
    /// Phase 76.7 — set of resource URIs this session subscribed
    /// to via `resources/subscribe`. Mutated under the
    /// `DashSet` shard lock; read by the notification fan-out
    /// without taking the session lock. Cleared when the session
    /// is removed from the manager.
    pub(crate) subscriptions: DashSet<String>,
    /// Phase 76.8 — per-session monotonic event-id counter. Bumped
    /// once per `emit()`. Starts at 1 (seq 0 is reserved as "no
    /// events yet"; clients send `Last-Event-ID: 0` for full
    /// stream).
    pub(crate) next_seq: AtomicU64,
}

impl HttpSession {
    fn new(buffer_size: usize, parent_cancel: &CancellationToken) -> Arc<Self> {
        let (notif_tx, _) = broadcast::channel(buffer_size);
        let now = Instant::now();
        Arc::new(Self {
            id: Uuid::new_v4().to_string(),
            created_at: now,
            last_seen: Mutex::new(now),
            notif_tx,
            sse_active: AtomicUsize::new(0),
            cancel: parent_cancel.child_token(),
            subscriptions: DashSet::new(),
            next_seq: AtomicU64::new(1),
        })
    }

    pub(crate) fn touch(&self) {
        *self
            .last_seen
            .lock()
            .expect("session last_seen mutex poisoned") = Instant::now();
    }

    pub(crate) fn last_seen(&self) -> Instant {
        *self
            .last_seen
            .lock()
            .expect("session last_seen mutex poisoned")
    }

    pub(crate) fn age(&self) -> Duration {
        self.created_at.elapsed()
    }

    pub(crate) fn idle(&self) -> Duration {
        self.last_seen().elapsed()
    }
}

#[derive(Debug)]
pub(crate) struct SessionLimitExceeded;

#[derive(Clone)]
pub(crate) struct HttpSessionManager {
    inner: Arc<Inner>,
}

struct Inner {
    sessions: DashMap<String, Arc<HttpSession>>,
    max_sessions: usize,
    idle_timeout: Duration,
    max_lifetime: Duration,
    sse_buffer_size: usize,
    parent_cancel: CancellationToken,
    /// Phase 76.8 — when set, every `emit()` persists the frame to
    /// the store + assigns a per-session monotonic seq.
    event_store: Option<Arc<dyn SessionEventStore>>,
    /// Phase 76.8 — per-session ring cap. Triggers
    /// `purge_oldest_for_session(keep)` when `seq % cap_check_every == 0`.
    max_events_per_session: u64,
    /// Phase 76.8 — replay batch ceiling — passed through to
    /// `replay()`.
    max_replay_batch: usize,
}

impl HttpSessionManager {
    #[allow(dead_code)]
    pub(crate) fn new(cfg: &HttpTransportConfig, parent_cancel: CancellationToken) -> Arc<Self> {
        Self::with_event_store(cfg, parent_cancel, None)
    }

    /// Phase 76.8 — construct with an optional event store + caps
    /// pulled from the runtime config block.
    pub(crate) fn with_event_store(
        cfg: &HttpTransportConfig,
        parent_cancel: CancellationToken,
        event_store: Option<Arc<dyn SessionEventStore>>,
    ) -> Arc<Self> {
        let (max_per, max_replay) = cfg
            .session_event_store
            .as_ref()
            .map(|c| (c.max_events_per_session, c.max_replay_batch))
            .unwrap_or((10_000, 1_000));
        Arc::new(Self {
            inner: Arc::new(Inner {
                sessions: DashMap::new(),
                max_sessions: cfg.max_sessions,
                idle_timeout: cfg.session_idle_timeout(),
                max_lifetime: cfg.session_max_lifetime(),
                sse_buffer_size: cfg.sse_buffer_size,
                parent_cancel,
                event_store,
                max_events_per_session: max_per,
                max_replay_batch: max_replay,
            }),
        })
    }

    #[allow(dead_code)]
    pub(crate) fn event_store(&self) -> Option<&Arc<dyn SessionEventStore>> {
        self.inner.event_store.as_ref()
    }

    #[allow(dead_code)]
    pub(crate) fn max_replay_batch(&self) -> usize {
        self.inner.max_replay_batch
    }

    /// Allocate a fresh session. Returns `Err(SessionLimitExceeded)`
    /// when the global cap is reached.
    pub(crate) fn create(&self) -> Result<Arc<HttpSession>, SessionLimitExceeded> {
        if self.inner.sessions.len() >= self.inner.max_sessions {
            return Err(SessionLimitExceeded);
        }
        let session = HttpSession::new(self.inner.sse_buffer_size, &self.inner.parent_cancel);
        self.inner
            .sessions
            .insert(session.id.clone(), session.clone());
        Ok(session)
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<HttpSession>> {
        self.inner.sessions.get(id).map(|r| r.clone())
    }

    pub(crate) fn touch(&self, id: &str) {
        if let Some(s) = self.inner.sessions.get(id) {
            s.touch();
        }
    }

    /// Explicit teardown.
    pub(crate) fn close(&self, id: &str) -> Option<Arc<HttpSession>> {
        let removed = self.inner.sessions.remove(id).map(|(_, v)| v);
        if let Some(s) = &removed {
            let _ = s.notif_tx.send(SessionEvent::EndOfStream {
                reason: "session_closed".into(),
            });
            s.cancel.cancel();
        }
        removed
    }

    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.inner.sessions.len()
    }

    /// Broadcast a `Shutdown` event to every active session.
    /// Ignores send errors (no SSE subscriber is OK).
    pub(crate) async fn shutdown_all(&self, reason: &str) {
        for entry in self.inner.sessions.iter() {
            let _ = entry.notif_tx.send(SessionEvent::Shutdown {
                reason: reason.into(),
            });
            entry.cancel.cancel();
        }
    }

    /// Spawn the background janitor. Cancels with `parent_cancel`.
    pub(crate) fn spawn_janitor(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let interval = Duration::from_secs(30);
            let parent = manager.inner.parent_cancel.clone();
            loop {
                tokio::select! {
                    _ = parent.cancelled() => break,
                    _ = tokio::time::sleep(interval) => {
                        manager.expire_stale();
                    }
                }
            }
        })
    }

    fn expire_stale(&self) {
        let mut to_remove: Vec<String> = Vec::new();
        for entry in self.inner.sessions.iter() {
            let s = entry.value();
            if s.idle() > self.inner.idle_timeout || s.age() > self.inner.max_lifetime {
                to_remove.push(s.id.clone());
            }
        }
        for id in to_remove {
            if let Some((_, s)) = self.inner.sessions.remove(&id) {
                let _ = s.notif_tx.send(SessionEvent::EndOfStream {
                    reason: "expired".into(),
                });
                s.cancel.cancel();
            }
        }
    }

    /// Phase 76.7 — broadcast a JSON-RPC notification to every
    /// active session. Phase 76.8 — when an event store is wired,
    /// each per-session emission goes through `emit()` so the
    /// frame is persisted + tagged with a monotonic seq.
    pub(crate) fn broadcast_to_all(&self, body: serde_json::Value) -> usize {
        let mut reached = 0;
        for entry in self.inner.sessions.iter() {
            if self.emit_to(entry.value(), body.clone()) {
                reached += 1;
            }
        }
        reached
    }

    /// Phase 76.7 — fan-out for `notifications/resources/updated`.
    /// Only sessions whose `subscriptions` set contains `uri`
    /// receive the event.
    pub(crate) fn notify_resource_updated(&self, uri: &str, body: serde_json::Value) -> usize {
        let mut reached = 0;
        for entry in self.inner.sessions.iter() {
            let session = entry.value();
            if !session.subscriptions.contains(uri) {
                continue;
            }
            if self.emit_to(session, body.clone()) {
                reached += 1;
            }
        }
        reached
    }

    /// Phase 76.8 — assign seq, persist (best-effort), broadcast
    /// `IndexedMessage`. Returns true if the broadcast had a live
    /// receiver. Persist failures are logged but do NOT abort the
    /// emission — the in-memory broadcast is the primary path.
    fn emit_to(&self, session: &Arc<HttpSession>, body: serde_json::Value) -> bool {
        let seq = session.next_seq.fetch_add(1, Ordering::Relaxed);
        if let Some(store) = self.inner.event_store.as_ref() {
            let store = Arc::clone(store);
            let session_id = session.id.clone();
            let body_for_persist = body.clone();
            let cap = self.inner.max_events_per_session;
            tokio::spawn(async move {
                if let Err(e) = store.append(&session_id, seq, &body_for_persist).await {
                    tracing::warn!(
                        session_id = %session_id,
                        seq,
                        error = %e,
                        "mcp session event_store append failed"
                    );
                }
                // Cap enforcement: every 1000th emit, prune oldest.
                if cap > 0 && seq % 1000 == 0 {
                    if let Err(e) = store.purge_oldest_for_session(&session_id, cap).await {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "mcp session event_store purge_oldest failed"
                        );
                    }
                }
            });
        }
        session
            .notif_tx
            .send(SessionEvent::IndexedMessage { seq, body })
            .is_ok()
    }

    /// Phase 76.8 — surface for the SSE handler. Returns persisted
    /// frames with `seq > min_seq`, capped at `max_replay_batch`.
    /// Empty when no store is configured or no gap exists.
    pub(crate) async fn replay(
        &self,
        session_id: &str,
        min_seq: u64,
    ) -> Vec<(u64, serde_json::Value)> {
        let Some(store) = self.inner.event_store.as_ref() else {
            return Vec::new();
        };
        match store
            .tail_after(session_id, min_seq, self.inner.max_replay_batch)
            .await
        {
            Ok(rows) => rows.into_iter().map(|r| (r.seq, r.frame)).collect(),
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    min_seq,
                    error = %e,
                    "mcp session event_store replay failed"
                );
                Vec::new()
            }
        }
    }

    /// Phase 76.8 — background purge tick. Drops events older than
    /// the session max-lifetime cutoff.
    pub(crate) async fn purge_expired_events(&self) {
        let Some(store) = self.inner.event_store.as_ref() else {
            return;
        };
        let cutoff = now_ms().saturating_sub(self.inner.max_lifetime.as_millis() as i64);
        if let Err(e) = store.purge_older_than(cutoff).await {
            tracing::warn!(
                cutoff_ms = cutoff,
                error = %e,
                "mcp session event_store purge_older_than failed"
            );
        }
    }

    /// Phase 76.7 — public accessor for `SessionLookup`
    /// implementations and tests.
    pub(crate) fn session_by_id(&self, id: &str) -> Option<Arc<HttpSession>> {
        self.inner.sessions.get(id).map(|s| Arc::clone(s.value()))
    }
}

/// Phase 76.7 — abstract session lookup so the dispatcher can
/// mutate `subscriptions` without depending on the concrete
/// `HttpSessionManager`. Stdio passes `None` for this trait
/// object, so `resources/subscribe` over stdio is a no-op (the
/// dispatcher returns an empty `Reply`).
pub trait SessionLookup: Send + Sync {
    fn subscribe(&self, session_id: &str, uri: &str) -> bool;
    fn unsubscribe(&self, session_id: &str, uri: &str) -> bool;
}

impl SessionLookup for HttpSessionManager {
    fn subscribe(&self, session_id: &str, uri: &str) -> bool {
        if let Some(session) = self.session_by_id(session_id) {
            session.subscriptions.insert(uri.to_string());
            self.persist_subscriptions(&session);
            true
        } else {
            false
        }
    }

    fn unsubscribe(&self, session_id: &str, uri: &str) -> bool {
        if let Some(session) = self.session_by_id(session_id) {
            session.subscriptions.remove(uri);
            self.persist_subscriptions(&session);
            true
        } else {
            false
        }
    }
}

impl HttpSessionManager {
    /// Phase 76.8 — fire-and-forget snapshot of the in-mem
    /// subscription set into the event store. Async write off the
    /// hot path; failures are logged.
    fn persist_subscriptions(&self, session: &Arc<HttpSession>) {
        let Some(store) = self.inner.event_store.as_ref() else {
            return;
        };
        let store = Arc::clone(store);
        let session_id = session.id.clone();
        let uris: Vec<String> = session
            .subscriptions
            .iter()
            .map(|r| r.key().clone())
            .collect();
        tokio::spawn(async move {
            if let Err(e) = store.put_subscriptions(&session_id, &uris).await {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "mcp session event_store put_subscriptions failed"
                );
            }
        });
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cfg(max_sessions: usize) -> HttpTransportConfig {
        let mut c = HttpTransportConfig::default();
        c.max_sessions = max_sessions;
        c.session_idle_timeout_secs = 1; // for janitor test
        c.session_max_lifetime_secs = 1;
        c
    }

    #[tokio::test]
    async fn create_increments_count() {
        let cancel = CancellationToken::new();
        let mgr = HttpSessionManager::new(&cfg(10), cancel);
        assert_eq!(mgr.len(), 0);
        let s = mgr.create().unwrap();
        assert_eq!(mgr.len(), 1);
        assert!(mgr.get(&s.id).is_some());
    }

    #[tokio::test]
    async fn over_cap_returns_err() {
        let cancel = CancellationToken::new();
        let mgr = HttpSessionManager::new(&cfg(2), cancel);
        let _a = mgr.create().unwrap();
        let _b = mgr.create().unwrap();
        assert!(mgr.create().is_err());
    }

    #[tokio::test]
    async fn touch_updates_last_seen() {
        let cancel = CancellationToken::new();
        let mgr = HttpSessionManager::new(&cfg(10), cancel);
        let s = mgr.create().unwrap();
        let before = s.last_seen();
        tokio::time::sleep(Duration::from_millis(10)).await;
        mgr.touch(&s.id);
        assert!(s.last_seen() > before);
    }

    #[tokio::test]
    async fn close_removes_and_cancels() {
        let cancel = CancellationToken::new();
        let mgr = HttpSessionManager::new(&cfg(10), cancel);
        let s = mgr.create().unwrap();
        let mut rx = s.notif_tx.subscribe();
        mgr.close(&s.id);
        assert_eq!(mgr.len(), 0);
        assert!(s.cancel.is_cancelled());
        // Subscriber sees EndOfStream.
        let evt = rx.recv().await.unwrap();
        assert!(matches!(evt, SessionEvent::EndOfStream { .. }));
    }

    #[tokio::test]
    async fn janitor_expires_idle() {
        let cancel = CancellationToken::new();
        let mut c = cfg(10);
        c.session_idle_timeout_secs = 1;
        c.session_max_lifetime_secs = 1;
        let mgr = HttpSessionManager::new(&c, cancel);
        let _s = mgr.create().unwrap();
        // Janitor runs every 30s — too slow for the test. Call
        // `expire_stale` directly after waiting past idle.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        mgr.expire_stale();
        assert_eq!(mgr.len(), 0);
    }

    #[tokio::test]
    async fn shutdown_all_broadcasts() {
        let cancel = CancellationToken::new();
        let mgr = HttpSessionManager::new(&cfg(10), cancel);
        let a = mgr.create().unwrap();
        let b = mgr.create().unwrap();
        let mut ra = a.notif_tx.subscribe();
        let mut rb = b.notif_tx.subscribe();
        mgr.shutdown_all("test").await;
        assert!(matches!(
            ra.recv().await.unwrap(),
            SessionEvent::Shutdown { .. }
        ));
        assert!(matches!(
            rb.recv().await.unwrap(),
            SessionEvent::Shutdown { .. }
        ));
        assert!(a.cancel.is_cancelled());
        assert!(b.cancel.is_cancelled());
    }

    #[tokio::test]
    async fn emit_assigns_monotonic_seq_and_persists() {
        use crate::server::event_store::MemorySessionEventStore;
        use serde_json::json;
        let cancel = CancellationToken::new();
        let store = MemorySessionEventStore::new();
        let store_dyn: Arc<dyn SessionEventStore> = store.clone();
        let mgr = HttpSessionManager::with_event_store(&cfg(10), cancel, Some(store_dyn));
        let s = mgr.create().unwrap();
        let mut rx = s.notif_tx.subscribe();
        mgr.broadcast_to_all(json!({"v": 1}));
        mgr.broadcast_to_all(json!({"v": 2}));
        let e1 = rx.recv().await.unwrap();
        let e2 = rx.recv().await.unwrap();
        match (e1, e2) {
            (
                SessionEvent::IndexedMessage { seq: s1, .. },
                SessionEvent::IndexedMessage { seq: s2, .. },
            ) => assert_eq!((s1, s2), (1, 2)),
            other => panic!("expected IndexedMessage pair, got {other:?}"),
        }
        // Persist is fire-and-forget; yield until the store sees both.
        for _ in 0..50 {
            let rows = store.tail_after(&s.id, 0, 100).await.unwrap();
            if rows.len() == 2 {
                assert_eq!(rows[0].seq, 1);
                assert_eq!(rows[1].seq, 2);
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("event_store did not see persisted rows in time");
    }

    #[tokio::test]
    async fn replay_returns_gap_after_min_seq() {
        use crate::server::event_store::MemorySessionEventStore;
        use serde_json::json;
        let cancel = CancellationToken::new();
        let store = MemorySessionEventStore::new();
        let store_dyn: Arc<dyn SessionEventStore> = store.clone();
        let mgr = HttpSessionManager::with_event_store(&cfg(10), cancel, Some(store_dyn));
        let s = mgr.create().unwrap();
        for n in 0..5 {
            mgr.broadcast_to_all(json!({"n": n}));
        }
        // Wait for persist.
        for _ in 0..50 {
            if store.tail_after(&s.id, 0, 100).await.unwrap().len() == 5 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let gap = mgr.replay(&s.id, 2).await;
        assert_eq!(gap.len(), 3);
        assert_eq!(gap[0].0, 3);
        assert_eq!(gap[2].0, 5);
    }

    #[tokio::test]
    async fn subscribe_persists_uri_set() {
        use crate::server::event_store::MemorySessionEventStore;
        let cancel = CancellationToken::new();
        let store = MemorySessionEventStore::new();
        let store_dyn: Arc<dyn SessionEventStore> = store.clone();
        let mgr = HttpSessionManager::with_event_store(&cfg(10), cancel, Some(store_dyn));
        let s = mgr.create().unwrap();
        assert!(<HttpSessionManager as SessionLookup>::subscribe(
            &mgr,
            &s.id,
            "res://foo"
        ));
        for _ in 0..50 {
            let got = store.load_subscriptions(&s.id).await.unwrap();
            if got == vec!["res://foo".to_string()] {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("subscription was not persisted in time");
    }

    #[tokio::test]
    async fn replay_without_store_returns_empty() {
        let cancel = CancellationToken::new();
        let mgr = HttpSessionManager::new(&cfg(10), cancel);
        let s = mgr.create().unwrap();
        let gap = mgr.replay(&s.id, 0).await;
        assert!(gap.is_empty());
    }

    #[tokio::test]
    async fn parent_cancel_stops_janitor() {
        let cancel = CancellationToken::new();
        let mgr = HttpSessionManager::new(&cfg(10), cancel.clone());
        let h = mgr.spawn_janitor();
        cancel.cancel();
        // Should exit promptly.
        let res = tokio::time::timeout(Duration::from_secs(2), h).await;
        assert!(res.is_ok(), "janitor did not stop on parent cancel");
    }
}
