//! Phase 76.8 — `SessionEventStore` trait + `MemorySessionEventStore`.
//!
//! Trait shape mirrors `crates/agent-registry/src/turn_log.rs:64-89`
//! (Phase 72). The in-memory impl is for tests only — production
//! deployments configure [`super::SqliteSessionEventStore`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;

use super::types::StoredEvent;

#[derive(Debug, thiserror::Error)]
pub enum EventStoreError {
    #[error("event store backend error: {0}")]
    Backend(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
}

/// Durable surface behind [`HttpSessionManager`]. Implementations
/// must be idempotent on the `(session_id, seq)` key — a retried
/// append must NOT duplicate the row.
#[async_trait]
pub trait SessionEventStore: Send + Sync {
    /// Persist one frame. Idempotent on `(session_id, seq)`.
    async fn append(
        &self,
        session_id: &str,
        seq: u64,
        frame: &Value,
    ) -> Result<(), EventStoreError>;

    /// Return rows with `seq > min_seq`, ordered by `seq` ascending,
    /// capped at `max_rows`. Empty vector when no gap.
    async fn tail_after(
        &self,
        session_id: &str,
        min_seq: u64,
        max_rows: usize,
    ) -> Result<Vec<StoredEvent>, EventStoreError>;

    /// Wipe every event + subscription row for `session_id`.
    /// Returns the number of event rows removed.
    async fn drop_session(&self, session_id: &str) -> Result<u64, EventStoreError>;

    /// Drop events older than `before_ms` (epoch millis). Returns
    /// the row count removed. Subscriptions are NOT touched here —
    /// they live as long as their session.
    async fn purge_older_than(&self, before_ms: i64) -> Result<u64, EventStoreError>;

    /// Cap enforcement: keep only the last `keep` events for
    /// `session_id`, drop the older ones. Returns rows removed.
    async fn purge_oldest_for_session(
        &self,
        session_id: &str,
        keep: u64,
    ) -> Result<u64, EventStoreError>;

    /// Replace the entire subscription set for `session_id`.
    async fn put_subscriptions(
        &self,
        session_id: &str,
        uris: &[String],
    ) -> Result<(), EventStoreError>;

    /// Read the subscription set for `session_id`. Empty when none.
    async fn load_subscriptions(&self, session_id: &str) -> Result<Vec<String>, EventStoreError>;
}

/// Test-only in-memory backend.
#[derive(Debug, Default)]
pub struct MemorySessionEventStore {
    inner: Mutex<MemoryInner>,
}

#[derive(Debug, Default)]
struct MemoryInner {
    events: HashMap<String, BTreeMap<u64, StoredEvent>>,
    subs: HashMap<String, BTreeSet<String>>,
}

impl MemorySessionEventStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl SessionEventStore for MemorySessionEventStore {
    async fn append(
        &self,
        session_id: &str,
        seq: u64,
        frame: &Value,
    ) -> Result<(), EventStoreError> {
        let mut g = self.inner.lock();
        let row = StoredEvent {
            seq,
            frame: frame.clone(),
            created_at_ms: now_ms(),
        };
        // INSERT OR IGNORE semantics: keep the existing row.
        g.events
            .entry(session_id.to_string())
            .or_default()
            .entry(seq)
            .or_insert(row);
        Ok(())
    }

    async fn tail_after(
        &self,
        session_id: &str,
        min_seq: u64,
        max_rows: usize,
    ) -> Result<Vec<StoredEvent>, EventStoreError> {
        let g = self.inner.lock();
        let Some(map) = g.events.get(session_id) else {
            return Ok(Vec::new());
        };
        Ok(map
            .range((
                std::ops::Bound::Excluded(min_seq),
                std::ops::Bound::Unbounded,
            ))
            .take(max_rows)
            .map(|(_, ev)| ev.clone())
            .collect())
    }

    async fn drop_session(&self, session_id: &str) -> Result<u64, EventStoreError> {
        let mut g = self.inner.lock();
        let n = g
            .events
            .remove(session_id)
            .map(|m| m.len() as u64)
            .unwrap_or(0);
        g.subs.remove(session_id);
        Ok(n)
    }

    async fn purge_older_than(&self, before_ms: i64) -> Result<u64, EventStoreError> {
        let mut g = self.inner.lock();
        let mut removed = 0u64;
        for map in g.events.values_mut() {
            let stale: Vec<u64> = map
                .iter()
                .filter(|(_, ev)| ev.created_at_ms < before_ms)
                .map(|(seq, _)| *seq)
                .collect();
            for seq in stale {
                map.remove(&seq);
                removed += 1;
            }
        }
        Ok(removed)
    }

    async fn purge_oldest_for_session(
        &self,
        session_id: &str,
        keep: u64,
    ) -> Result<u64, EventStoreError> {
        let mut g = self.inner.lock();
        let Some(map) = g.events.get_mut(session_id) else {
            return Ok(0);
        };
        let total = map.len() as u64;
        if total <= keep {
            return Ok(0);
        }
        let drop_n = (total - keep) as usize;
        let to_drop: Vec<u64> = map.keys().take(drop_n).copied().collect();
        for seq in to_drop {
            map.remove(&seq);
        }
        Ok(drop_n as u64)
    }

    async fn put_subscriptions(
        &self,
        session_id: &str,
        uris: &[String],
    ) -> Result<(), EventStoreError> {
        let mut g = self.inner.lock();
        let set = g.subs.entry(session_id.to_string()).or_default();
        set.clear();
        for u in uris {
            set.insert(u.clone());
        }
        Ok(())
    }

    async fn load_subscriptions(&self, session_id: &str) -> Result<Vec<String>, EventStoreError> {
        let g = self.inner.lock();
        Ok(g.subs
            .get(session_id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default())
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
    use serde_json::json;

    #[tokio::test]
    async fn append_then_tail_after_returns_strict_gap() {
        let s = MemorySessionEventStore::new();
        for seq in 1..=5 {
            s.append("sid", seq, &json!({"n": seq})).await.unwrap();
        }
        let out = s.tail_after("sid", 2, 100).await.unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].seq, 3);
        assert_eq!(out[2].seq, 5);
    }

    #[tokio::test]
    async fn append_idempotent_on_seq_collision() {
        let s = MemorySessionEventStore::new();
        s.append("sid", 1, &json!({"v": "first"})).await.unwrap();
        s.append("sid", 1, &json!({"v": "second"})).await.unwrap();
        let out = s.tail_after("sid", 0, 100).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].frame, json!({"v": "first"}));
    }

    #[tokio::test]
    async fn drop_session_clears_events_and_subs() {
        let s = MemorySessionEventStore::new();
        s.append("sid", 1, &json!({})).await.unwrap();
        s.put_subscriptions("sid", &["res://a".into()])
            .await
            .unwrap();
        let removed = s.drop_session("sid").await.unwrap();
        assert_eq!(removed, 1);
        assert!(s.tail_after("sid", 0, 10).await.unwrap().is_empty());
        assert!(s.load_subscriptions("sid").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn purge_oldest_for_session_keeps_n() {
        let s = MemorySessionEventStore::new();
        for seq in 1..=10 {
            s.append("sid", seq, &json!({})).await.unwrap();
        }
        let removed = s.purge_oldest_for_session("sid", 4).await.unwrap();
        assert_eq!(removed, 6);
        let kept = s.tail_after("sid", 0, 100).await.unwrap();
        assert_eq!(kept.len(), 4);
        assert_eq!(kept[0].seq, 7);
    }

    #[tokio::test]
    async fn subscriptions_replace_semantics() {
        let s = MemorySessionEventStore::new();
        s.put_subscriptions("sid", &["a".into(), "b".into()])
            .await
            .unwrap();
        s.put_subscriptions("sid", &["b".into(), "c".into()])
            .await
            .unwrap();
        let mut got = s.load_subscriptions("sid").await.unwrap();
        got.sort();
        assert_eq!(got, vec!["b", "c"]);
    }
}
