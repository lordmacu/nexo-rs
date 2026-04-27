//! `AuditLogStore` trait + an in-memory impl for tests. The
//! production SQLite impl lives in `sqlite_store.rs` (Phase 76.11
//! step 3).

use async_trait::async_trait;
use parking_lot::Mutex;
use thiserror::Error;

use super::types::{AuditFilter, AuditRow};

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encoded args/result too large: {0} bytes (cap {1})")]
    Oversized(usize, usize),
    #[error("writer channel closed")]
    Closed,
    #[error("backend: {0}")]
    Backend(String),
}

/// Append-only audit-log store. Implementations must be safe to
/// share across threads (`Send + Sync`) — the writer worker holds
/// an `Arc<dyn AuditLogStore>` and may `append` from any task.
#[async_trait]
pub trait AuditLogStore: Send + Sync {
    /// Persist a batch of rows. Idempotent on `call_id`: a second
    /// append with the same `call_id` MUST overwrite (concrete
    /// SQLite uses `INSERT OR REPLACE`).
    async fn append(&self, rows: Vec<AuditRow>) -> Result<(), AuditError>;

    /// Newest-first slice of rows matching `filter`, capped by
    /// `limit` (the impl must additionally respect a hard ceiling
    /// — production SQLite caps at 1000).
    async fn tail(&self, filter: &AuditFilter, limit: usize) -> Result<Vec<AuditRow>, AuditError>;

    /// Number of rows matching `filter`.
    async fn count(&self, filter: &AuditFilter) -> Result<u64, AuditError>;

    /// Remove rows whose `started_at_ms` is older than `cutoff_ms`.
    /// Returns the count removed. Used by the background pruner.
    async fn prune_older_than(&self, cutoff_ms: i64) -> Result<u64, AuditError>;
}

// --- In-memory impl (tests) --------------------------------------

/// `AuditLogStore` backed by a `Vec<AuditRow>` behind a
/// `parking_lot::Mutex`. NOT for production — tests only. Mirrors
/// the SQLite semantics (idempotent upsert, newest-first tail,
/// filter-then-limit).
#[derive(Default)]
pub struct MemoryAuditLogStore {
    rows: Mutex<Vec<AuditRow>>,
}

impl MemoryAuditLogStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn matches(row: &AuditRow, filter: &AuditFilter) -> bool {
    if let Some(t) = filter.tenant.as_deref() {
        if row.tenant != t {
            return false;
        }
    }
    if let Some(t) = filter.tool_name.as_deref() {
        if row.tool_name.as_deref() != Some(t) {
            return false;
        }
    }
    if let Some(o) = filter.outcome {
        if row.outcome != o {
            return false;
        }
    }
    if let Some(since) = filter.since_ms {
        if row.started_at_ms < since {
            return false;
        }
    }
    if let Some(until) = filter.until_ms {
        if row.started_at_ms >= until {
            return false;
        }
    }
    true
}

#[async_trait]
impl AuditLogStore for MemoryAuditLogStore {
    async fn append(&self, rows: Vec<AuditRow>) -> Result<(), AuditError> {
        let mut g = self.rows.lock();
        for row in rows {
            // Idempotent on call_id (mirror INSERT OR REPLACE).
            if let Some(idx) = g.iter().position(|r| r.call_id == row.call_id) {
                g[idx] = row;
            } else {
                g.push(row);
            }
        }
        Ok(())
    }

    async fn tail(&self, filter: &AuditFilter, limit: usize) -> Result<Vec<AuditRow>, AuditError> {
        let g = self.rows.lock();
        let mut out: Vec<AuditRow> = g.iter().filter(|r| matches(r, filter)).cloned().collect();
        // Newest-first by `started_at_ms` desc.
        out.sort_by(|a, b| b.started_at_ms.cmp(&a.started_at_ms));
        out.truncate(limit);
        Ok(out)
    }

    async fn count(&self, filter: &AuditFilter) -> Result<u64, AuditError> {
        let g = self.rows.lock();
        Ok(g.iter().filter(|r| matches(r, filter)).count() as u64)
    }

    async fn prune_older_than(&self, cutoff_ms: i64) -> Result<u64, AuditError> {
        let mut g = self.rows.lock();
        let before = g.len();
        g.retain(|r| r.started_at_ms >= cutoff_ms);
        Ok((before - g.len()) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::audit_log::types::AuditOutcome;

    fn row(call_id: &str, tenant: &str, tool: &str, started_ms: i64) -> AuditRow {
        AuditRow {
            call_id: call_id.into(),
            request_id: None,
            session_id: None,
            tenant: tenant.into(),
            subject: None,
            auth_method: "static_token".into(),
            method: "tools/call".into(),
            tool_name: Some(tool.into()),
            args_hash: None,
            args_size_bytes: 0,
            started_at_ms: started_ms,
            completed_at_ms: Some(started_ms + 1),
            duration_ms: Some(1),
            outcome: AuditOutcome::Ok,
            error_code: None,
            error_message: None,
            result_size_bytes: None,
            retry_after_ms: None,
        }
    }

    #[tokio::test]
    async fn append_then_tail_round_trip() {
        let store = MemoryAuditLogStore::new();
        store.append(vec![row("a", "t1", "echo", 1)]).await.unwrap();
        let out = store.tail(&AuditFilter::default(), 10).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].call_id, "a");
    }

    #[tokio::test]
    async fn tail_is_newest_first() {
        let store = MemoryAuditLogStore::new();
        store
            .append(vec![row("a", "t", "x", 100), row("b", "t", "x", 200)])
            .await
            .unwrap();
        let out = store.tail(&AuditFilter::default(), 10).await.unwrap();
        assert_eq!(out[0].call_id, "b");
        assert_eq!(out[1].call_id, "a");
    }

    #[tokio::test]
    async fn idempotent_append_overwrites_on_call_id() {
        let store = MemoryAuditLogStore::new();
        let mut r1 = row("a", "t", "x", 100);
        r1.error_message = Some("first".into());
        store.append(vec![r1.clone()]).await.unwrap();
        let mut r2 = r1.clone();
        r2.error_message = Some("second".into());
        store.append(vec![r2]).await.unwrap();
        let out = store.tail(&AuditFilter::default(), 10).await.unwrap();
        assert_eq!(out.len(), 1, "second append must REPLACE not duplicate");
        assert_eq!(out[0].error_message.as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn filter_by_tenant() {
        let store = MemoryAuditLogStore::new();
        store
            .append(vec![
                row("a", "t1", "x", 1),
                row("b", "t2", "x", 2),
                row("c", "t1", "x", 3),
            ])
            .await
            .unwrap();
        let f = AuditFilter {
            tenant: Some("t1".into()),
            ..Default::default()
        };
        let out = store.tail(&f, 10).await.unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.tenant == "t1"));
    }

    #[tokio::test]
    async fn filter_by_outcome() {
        let store = MemoryAuditLogStore::new();
        let mut r1 = row("a", "t", "x", 1);
        r1.outcome = AuditOutcome::Ok;
        let mut r2 = row("b", "t", "x", 2);
        r2.outcome = AuditOutcome::Timeout;
        store.append(vec![r1, r2]).await.unwrap();
        let f = AuditFilter {
            outcome: Some(AuditOutcome::Timeout),
            ..Default::default()
        };
        let out = store.tail(&f, 10).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].call_id, "b");
    }

    #[tokio::test]
    async fn filter_by_time_range() {
        let store = MemoryAuditLogStore::new();
        store
            .append(vec![
                row("a", "t", "x", 100),
                row("b", "t", "x", 200),
                row("c", "t", "x", 300),
            ])
            .await
            .unwrap();
        let f = AuditFilter {
            since_ms: Some(150),
            until_ms: Some(250),
            ..Default::default()
        };
        let out = store.tail(&f, 10).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].call_id, "b");
    }

    #[tokio::test]
    async fn count_matches_filter() {
        let store = MemoryAuditLogStore::new();
        store
            .append(vec![
                row("a", "t1", "x", 1),
                row("b", "t1", "x", 2),
                row("c", "t2", "x", 3),
            ])
            .await
            .unwrap();
        let f = AuditFilter {
            tenant: Some("t1".into()),
            ..Default::default()
        };
        assert_eq!(store.count(&f).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn prune_drops_only_old_rows() {
        let store = MemoryAuditLogStore::new();
        store
            .append(vec![
                row("a", "t", "x", 100),
                row("b", "t", "x", 200),
                row("c", "t", "x", 300),
            ])
            .await
            .unwrap();
        let n = store.prune_older_than(250).await.unwrap();
        assert_eq!(n, 2);
        assert_eq!(store.count(&AuditFilter::default()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn tail_respects_limit() {
        let store = MemoryAuditLogStore::new();
        for i in 0..50 {
            store
                .append(vec![row(&format!("r{i}"), "t", "x", i)])
                .await
                .unwrap();
        }
        let out = store.tail(&AuditFilter::default(), 10).await.unwrap();
        assert_eq!(out.len(), 10);
    }
}
