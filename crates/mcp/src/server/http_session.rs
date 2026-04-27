//! Phase 76.1 — HTTP session manager.
//!
//! Owns the per-session state shared between `POST /mcp`,
//! `GET /mcp` (SSE), and `DELETE /mcp`. Sessions live in a
//! `DashMap` capped at `max_sessions`; a janitor task expires
//! idle / over-lifetime entries every 30 s. Server shutdown
//! broadcasts a `Shutdown` event to every active SSE consumer
//! before tearing down listeners.

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dashmap::{DashMap, DashSet};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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
    /// JSON-RPC envelope to forward as `event: message`.
    Message(serde_json::Value),
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
}

impl HttpSessionManager {
    pub(crate) fn new(cfg: &HttpTransportConfig, parent_cancel: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Inner {
                sessions: DashMap::new(),
                max_sessions: cfg.max_sessions,
                idle_timeout: cfg.session_idle_timeout(),
                max_lifetime: cfg.session_max_lifetime(),
                sse_buffer_size: cfg.sse_buffer_size,
                parent_cancel,
            }),
        })
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
    /// active session. Returns the number of sessions reached
    /// (including those whose receiver was lagged or dropped —
    /// the broadcast `send` returns the receiver count, but
    /// here we count successfully-sent envelopes; failed sends
    /// are counted as 0).
    pub(crate) fn broadcast_to_all(&self, body: serde_json::Value) -> usize {
        let mut reached = 0;
        for entry in self.inner.sessions.iter() {
            if entry
                .value()
                .notif_tx
                .send(SessionEvent::Message(body.clone()))
                .is_ok()
            {
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
            if session
                .notif_tx
                .send(SessionEvent::Message(body.clone()))
                .is_ok()
            {
                reached += 1;
            }
        }
        reached
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
            true
        } else {
            false
        }
    }

    fn unsubscribe(&self, session_id: &str, uri: &str) -> bool {
        if let Some(session) = self.session_by_id(session_id) {
            session.subscriptions.remove(uri);
            true
        } else {
            false
        }
    }
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
