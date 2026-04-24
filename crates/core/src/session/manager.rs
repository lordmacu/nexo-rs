use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use dashmap::DashMap;
use uuid::Uuid;

use super::types::Session;

type ExpireCallback = Arc<dyn Fn(Uuid) + Send + Sync>;

/// Soft cap on concurrent live sessions. Prevents a Telegram/WhatsApp
/// spammer from creating unbounded sessions by rotating `chat_id`s.
/// At the cap, the oldest-idle session is evicted before a new one
/// lands — this keeps the policy invisible to legitimate traffic
/// while bounding memory growth under abuse. The default is large
/// enough that real deployments rarely hit it.
pub const DEFAULT_MAX_SESSIONS: usize = 10_000;

#[derive(Clone)]
pub struct SessionManager {
    sessions: Arc<DashMap<Uuid, Session>>,
    ttl: Duration,
    max_turns: usize,
    max_sessions: usize,
    /// Callbacks invoked (via `tokio::spawn`) whenever a session is dropped
    /// from the map — both explicit `delete()` and TTL sweep drop it.
    on_expire: Arc<Mutex<Vec<ExpireCallback>>>,
}

impl SessionManager {
    /// Creates the manager and spawns the background TTL sweeper.
    pub fn new(ttl: Duration, max_turns: usize) -> Self {
        Self::with_cap(ttl, max_turns, DEFAULT_MAX_SESSIONS)
    }

    /// Creates the manager with a custom concurrent-session cap. Values
    /// of `0` disable the cap (unbounded).
    pub fn with_cap(ttl: Duration, max_turns: usize, max_sessions: usize) -> Self {
        let sessions = Arc::new(DashMap::new());
        let on_expire: Arc<Mutex<Vec<ExpireCallback>>> = Arc::new(Mutex::new(Vec::new()));
        let mgr = Self {
            sessions,
            ttl,
            max_turns,
            max_sessions,
            on_expire,
        };
        mgr.spawn_sweeper();
        mgr
    }

    /// Evict the single oldest-idle session once we've hit the cap. The
    /// scan is O(n) on the DashMap, but only runs when we're at the
    /// ceiling — under normal load it never fires.
    fn enforce_cap(&self) {
        if self.max_sessions == 0 {
            return;
        }
        while self.sessions.len() >= self.max_sessions {
            let oldest = self
                .sessions
                .iter()
                .min_by_key(|e| e.value().last_access)
                .map(|e| *e.key());
            match oldest {
                Some(id) => {
                    if self.sessions.remove(&id).is_some() {
                        tracing::warn!(
                            session_id = %id,
                            cap = self.max_sessions,
                            "session cap reached; evicting oldest-idle"
                        );
                        self.fire_expire(id);
                    }
                }
                None => break,
            }
        }
    }

    pub fn create(&self, agent_id: impl Into<String>) -> Session {
        self.enforce_cap();
        let session = Session::new(agent_id, self.max_turns);
        self.sessions.insert(session.id, session.clone());
        session
    }

    /// Returns the session, updating last_access. Returns None if not found.
    pub fn get(&self, id: Uuid) -> Option<Session> {
        let mut entry = self.sessions.get_mut(&id)?;
        entry.last_access = Utc::now();
        Some(entry.clone())
    }

    /// Returns existing session or creates a new one bound to agent_id.
    pub fn get_or_create(&self, id: Uuid, agent_id: impl Into<String>) -> Session {
        // Evict before insert — only fires on the create branch because
        // `enforce_cap` runs against the pre-insert size. An existing
        // session hits DashMap's get path, which is unaffected.
        if !self.sessions.contains_key(&id) {
            self.enforce_cap();
        }
        let agent_id = agent_id.into();
        let max_turns = self.max_turns;
        let mut entry = self
            .sessions
            .entry(id)
            .or_insert_with(|| Session::with_id(id, agent_id, max_turns));
        entry.last_access = Utc::now();
        entry.clone()
    }

    /// Replaces session state. Returns false if the session does not exist.
    ///
    /// The replacement is atomic via DashMap's `get_mut` entry lock — this
    /// prevents a race where `delete()` runs between a check and insert
    /// and the update silently resurrects the removed session.
    pub fn update(&self, session: Session) -> bool {
        let Some(mut entry) = self.sessions.get_mut(&session.id) else {
            return false;
        };
        *entry = session;
        true
    }

    /// Atomically append an interaction to the session's history while
    /// holding the DashMap entry lock. The older pattern — `clone →
    /// mutate clone → update(clone)` — could drop history when two
    /// handlers raced on the same session (both read the pre-trim
    /// state, both pushed their entry, the second overwrote the
    /// first's trim). Returns false if the session no longer exists.
    pub fn push_message(&self, id: Uuid, interaction: super::types::Interaction) -> bool {
        let Some(mut entry) = self.sessions.get_mut(&id) else {
            return false;
        };
        entry.push(interaction);
        true
    }

    /// Removes a session and fires every registered `on_expire` callback.
    /// Returns true if it existed.
    pub fn delete(&self, id: Uuid) -> bool {
        let existed = self.sessions.remove(&id).is_some();
        if existed {
            self.fire_expire(id);
        }
        existed
    }

    pub fn active_count(&self) -> usize {
        self.sessions.len()
    }

    /// Register a callback fired when a session is dropped — either by
    /// explicit `delete()` or by the TTL sweeper. Each invocation runs on
    /// its own `tokio::spawn`, so callbacks must not assume caller context
    /// and should capture owned handles (Arc clones) from the closure's
    /// environment. Safe to call multiple times; callbacks fire in the
    /// registration order snapshot at the moment of expiry.
    pub fn on_expire<F>(&self, f: F)
    where
        F: Fn(Uuid) + Send + Sync + 'static,
    {
        self.on_expire.lock().unwrap().push(Arc::new(f));
    }

    fn fire_expire(&self, id: Uuid) {
        let callbacks: Vec<ExpireCallback> = self.on_expire.lock().unwrap().clone();
        spawn_callbacks(&callbacks, id);
    }

    fn spawn_sweeper(&self) {
        let sessions = Arc::clone(&self.sessions);
        let on_expire = Arc::clone(&self.on_expire);
        let ttl = self.ttl;
        // Sweep interval is ttl/4, minimum 10ms (keeps tests fast).
        let interval = (ttl / 4).max(Duration::from_millis(10));

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // skip first immediate tick
            loop {
                ticker.tick().await;
                let now = Utc::now();
                let ttl_chrono =
                    chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::hours(24));

                // Collect expired ids first so we can drop them + fire
                // callbacks without holding any map guard across await.
                let expired: Vec<Uuid> = sessions
                    .iter()
                    .filter_map(|entry| {
                        if now.signed_duration_since(entry.value().last_access) >= ttl_chrono {
                            Some(*entry.key())
                        } else {
                            None
                        }
                    })
                    .collect();
                if expired.is_empty() {
                    continue;
                }
                for id in &expired {
                    sessions.remove(id);
                }
                let callbacks: Vec<ExpireCallback> = on_expire.lock().unwrap().clone();
                if callbacks.is_empty() {
                    continue;
                }
                for id in expired {
                    spawn_callbacks(&callbacks, id);
                }
            }
        });
    }
}

fn spawn_callbacks(callbacks: &[ExpireCallback], id: Uuid) {
    for cb in callbacks {
        let cb = cb.clone();
        tokio::spawn(async move {
            cb(id);
        });
    }
}
