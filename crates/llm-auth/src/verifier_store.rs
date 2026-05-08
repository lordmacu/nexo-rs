//! In-memory PKCE verifier session store, used by the admin RPC
//! dispatcher to suspend an OAuth flow across two HTTP requests
//! (`oauth_start` then `oauth_finish`).
//!
//! Defensive design:
//!
//! * **Single-use**: [`VerifierStore::take`] removes the entry, so a
//!   second `oauth_finish` with the same `session_id` is rejected
//!   even if the first failed mid-exchange. Prevents replay.
//! * **TTL bounded**: every entry carries `expires_at`; entries past
//!   their expiry are returned as `None` (callers map this to
//!   `SessionExpired`). A background sweep drops them from the map
//!   so memory stays bounded under abandoned sessions.
//! * **Capacity bounded**: insertion past `max_entries` evicts the
//!   oldest entry first (FIFO by `created_at`). Anti-DoS.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;

use crate::pkce::Pkce;

/// What the dispatcher remembers between `oauth_start` and
/// `oauth_finish` for one OAuth session.
///
/// Anything captured here can be needed by the second step:
/// PKCE verifier (always), the device-code response (MiniMax only),
/// the factory id, and the auth_mode discriminator.
#[derive(Debug, Clone)]
pub struct VerifierEntry {
    /// PKCE bundle minted at `oauth_start`.
    pub pkce: Pkce,
    /// Factory id (e.g. `"anthropic"`, `"minimax"`).
    pub factory_type: String,
    /// `auth_code` or `device_code` — discriminates which exchange
    /// path `oauth_finish` should take. Free-form so future flows
    /// land additively.
    pub flow_kind: String,
    /// Optional MiniMax-specific device-code data, populated when
    /// `flow_kind = "device_code"` and absent for auth-code flows.
    pub device_code: Option<DeviceCodeContext>,
    /// Tenant scope to apply on persistence — propagates from
    /// `oauth_start` to `oauth_finish` so multi-tenant SaaS upsert
    /// lands the bundle on the right `tenants.<id>.providers.*`
    /// path.
    pub tenant_id: Option<String>,
    /// Unix seconds at which `take` will return `None`.
    pub expires_at_unix: i64,
    /// Unix seconds at insertion time. Used by FIFO eviction.
    pub created_at_unix: i64,
}

/// MiniMax `request_user_code` response carried across the two RPC
/// calls — the SPA needs `user_code` + `verification_uri` to render
/// the operator-facing pane between start + finish.
#[derive(Debug, Clone)]
pub struct DeviceCodeContext {
    /// Code the user types into the portal.
    pub user_code: String,
    /// URL the user opens in their browser.
    pub verification_uri: String,
    /// Unix-seconds polling deadline.
    pub deadline_unix: i64,
    /// Polling interval the server suggested.
    pub interval: Duration,
}

/// Pluggable OAuth-session store. Production wires
/// [`InMemoryVerifierStore`]; tests can substitute a mock.
#[async_trait]
#[allow(clippy::len_without_is_empty)] // diagnostics-only counter; no "empty store" semantic
pub trait VerifierStore: Send + Sync {
    /// Insert a fresh entry under a freshly-generated `session_id`.
    /// Returns the id the caller surfaces to the operator.
    async fn put(&self, entry: VerifierEntry) -> String;
    /// Atomically remove + return the entry. Returns `None` when
    /// the session id is unknown OR the entry has expired (the
    /// caller maps this to `SessionNotFound` vs `SessionExpired`
    /// using [`VerifierStore::peek_status`] before calling `take`).
    async fn take(&self, session_id: &str) -> Option<VerifierEntry>;
    /// Read-only lookup that returns `Status::Expired` for entries
    /// past their TTL (vs `Status::Missing` for unknown ids).
    /// Useful for diagnostic-quality error messages without burning
    /// the entry.
    async fn peek_status(&self, session_id: &str) -> SessionStatus;
    /// Drop every entry whose TTL has elapsed. Called periodically
    /// from a background sweep task.
    async fn sweep_expired(&self) -> usize;
    /// Number of entries currently held (live + expired-but-not-swept).
    async fn len(&self) -> usize;
}

/// Outcome of a non-mutating session lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Entry exists and is within its TTL.
    Live,
    /// Entry was inserted but the deadline has passed.
    Expired,
    /// No entry under that session id.
    Missing,
}

/// Production [`VerifierStore`] backed by a [`DashMap`].
pub struct InMemoryVerifierStore {
    entries: DashMap<String, VerifierEntry>,
    max_entries: usize,
}

impl InMemoryVerifierStore {
    /// Build a store with `max_entries` capacity. Once the cap is
    /// hit, FIFO eviction drops the oldest entry per insert.
    ///
    /// Recommended production value: 100. Each entry is ~256 B so
    /// the worst-case memory footprint is ≈ 25 KB.
    pub fn new(max_entries: usize) -> Arc<Self> {
        Arc::new(Self {
            entries: DashMap::new(),
            max_entries: max_entries.max(1),
        })
    }
}

#[async_trait]
impl VerifierStore for InMemoryVerifierStore {
    async fn put(&self, entry: VerifierEntry) -> String {
        let session_id = uuid::Uuid::new_v4().to_string();
        // FIFO eviction when at capacity. Cheap because the map is
        // small (≤ max_entries) and this only fires under load.
        if self.entries.len() >= self.max_entries {
            if let Some(oldest_key) = self
                .entries
                .iter()
                .min_by_key(|kv| kv.value().created_at_unix)
                .map(|kv| kv.key().clone())
            {
                self.entries.remove(&oldest_key);
            }
        }
        self.entries.insert(session_id.clone(), entry);
        session_id
    }

    async fn take(&self, session_id: &str) -> Option<VerifierEntry> {
        let (_, entry) = self.entries.remove(session_id)?;
        if entry.expires_at_unix < unix_now() {
            return None;
        }
        Some(entry)
    }

    async fn peek_status(&self, session_id: &str) -> SessionStatus {
        match self.entries.get(session_id) {
            Some(e) if e.expires_at_unix >= unix_now() => SessionStatus::Live,
            Some(_) => SessionStatus::Expired,
            None => SessionStatus::Missing,
        }
    }

    async fn sweep_expired(&self) -> usize {
        let now = unix_now();
        let stale: Vec<String> = self
            .entries
            .iter()
            .filter(|kv| kv.value().expires_at_unix < now)
            .map(|kv| kv.key().clone())
            .collect();
        let count = stale.len();
        for key in stale {
            self.entries.remove(&key);
        }
        if count > 0 {
            tracing::debug!(swept = count, "VerifierStore TTL sweep");
        }
        count
    }

    async fn len(&self) -> usize {
        self.entries.len()
    }
}

fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_with_ttl(ttl_secs: i64) -> VerifierEntry {
        let now = unix_now();
        VerifierEntry {
            pkce: Pkce {
                verifier: "v".into(),
                challenge: "c".into(),
                state: "s".into(),
            },
            factory_type: "anthropic".into(),
            flow_kind: "auth_code".into(),
            device_code: None,
            tenant_id: None,
            expires_at_unix: now + ttl_secs,
            created_at_unix: now,
        }
    }

    #[tokio::test]
    async fn put_then_take_returns_entry_once() {
        let store = InMemoryVerifierStore::new(10);
        let id = store.put(entry_with_ttl(60)).await;
        assert!(store.take(&id).await.is_some(), "first take returns entry");
        assert!(
            store.take(&id).await.is_none(),
            "second take must fail (single-use)"
        );
    }

    #[tokio::test]
    async fn take_returns_none_for_unknown_session() {
        let store = InMemoryVerifierStore::new(10);
        assert!(store.take("nope").await.is_none());
    }

    #[tokio::test]
    async fn take_returns_none_for_expired_entry() {
        let store = InMemoryVerifierStore::new(10);
        let id = store.put(entry_with_ttl(-1)).await;
        assert!(store.take(&id).await.is_none(), "expired entry not taken");
    }

    #[tokio::test]
    async fn peek_status_discriminates_live_expired_missing() {
        let store = InMemoryVerifierStore::new(10);
        let live = store.put(entry_with_ttl(60)).await;
        let dead = store.put(entry_with_ttl(-1)).await;
        assert_eq!(store.peek_status(&live).await, SessionStatus::Live);
        assert_eq!(store.peek_status(&dead).await, SessionStatus::Expired);
        assert_eq!(store.peek_status("nope").await, SessionStatus::Missing);
    }

    #[tokio::test]
    async fn sweep_drops_expired_entries() {
        let store = InMemoryVerifierStore::new(10);
        store.put(entry_with_ttl(60)).await;
        store.put(entry_with_ttl(-1)).await;
        store.put(entry_with_ttl(-1)).await;
        assert_eq!(store.len().await, 3);
        let swept = store.sweep_expired().await;
        assert_eq!(swept, 2);
        assert_eq!(store.len().await, 1);
    }

    #[tokio::test]
    async fn capacity_evicts_oldest_entry_fifo() {
        let store = InMemoryVerifierStore::new(2);
        let id1 = store.put(entry_with_ttl(60)).await;
        // small spacing so created_at_unix differs (it's seconds-resolution)
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let id2 = store.put(entry_with_ttl(60)).await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let id3 = store.put(entry_with_ttl(60)).await;
        assert_eq!(store.len().await, 2);
        assert!(
            store.peek_status(&id1).await == SessionStatus::Missing,
            "oldest should be evicted"
        );
        assert_eq!(store.peek_status(&id2).await, SessionStatus::Live);
        assert_eq!(store.peek_status(&id3).await, SessionStatus::Live);
    }
}
