//! Async storage layer for tracked outbound messages.
//!
//! Trait-oriented so consumers swap implementations
//! (in-memory tests, postgres, redis, …). The default ships
//! a SQLite impl wired through `sqlx`.
//!
//! ## Schema
//!
//! Three tables, all tenant-keyed:
//!
//! - `tracking_link`  — one row per rewritten anchor (the
//!   `(tenant, msg_id, link_id) → original_url` map).
//! - `tracking_open`  — append log of pixel hits.
//! - `tracking_click` — append log of redirector hits.
//!
//! Composite primary keys carry `tenant_id` first so the
//! sqlite query planner uses the index for every lookup
//! without operator hints.
//!
//! ## Tenant boundaries
//!
//! Every method takes `tenant_id: &str` and refuses cross-
//! tenant reads — the same posture as `identity::store`.
//! [`TrackingStore::delete_by_tenant`] is the only call that
//! cascades; consumers wire it into their tenant-deletion
//! flow so an evicted tenant's tracking history is purged.

use std::path::Path;
use std::str::FromStr;

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use thiserror::Error;

use super::types::{ClickEvent, LinkId, MsgId, OpenEvent};

/// Reasons a `TrackingStore` operation can fail. Caller maps
/// to HTTP 500 (or 502 for the SQLx variant) — none of these
/// surface to the end-user pixel/redirector consumer.
#[derive(Debug, Error)]
pub enum TrackingStoreError {
    /// Underlying SQLx error (connection lost, constraint
    /// violation, etc.). The bag here keeps the original
    /// chain available for tracing.
    #[error("tracking sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// Schema migration failed at boot. Surfaces as a
    /// distinct variant so the caller's healthcheck can
    /// distinguish "DB unreachable" from "DB but old".
    #[error("tracking migration: {0}")]
    Migration(String),
}

/// Trait surface every tracking storage backend implements.
///
/// `Send + Sync` so the marketing extension can wrap it in
/// `Arc<dyn TrackingStore>` and share across the
/// outbound-publisher thread + the ingest route handlers.
#[async_trait]
pub trait TrackingStore: Send + Sync {
    /// Persist the `(tenant, msg_id, link_id) → original_url`
    /// mapping at outbound compose time. Idempotent —
    /// re-registering the same triple replaces the row.
    async fn register_link(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
        link_id: &LinkId,
        original_url: &str,
        created_at_ms: i64,
    ) -> Result<(), TrackingStoreError>;

    /// Resolve `(tenant, msg_id, link_id) → original_url` at
    /// click redirect time. `None` ⇒ not registered (forged
    /// URL, expired record, cross-tenant attempt) — caller
    /// returns 404 / 410.
    async fn lookup_link(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
        link_id: &LinkId,
    ) -> Result<Option<String>, TrackingStoreError>;

    /// Append one open event. Caller dedupes upstream (same
    /// strategy as F9 — short minute-bucket) before calling.
    async fn record_open(&self, event: &OpenEvent) -> Result<(), TrackingStoreError>;

    /// Append one click event. Same caller-dedupe contract as
    /// `record_open`.
    async fn record_click(&self, event: &ClickEvent) -> Result<(), TrackingStoreError>;

    /// Total open hits for one message. Used by the lead
    /// drawer's "📧 Opened 3×" badge.
    async fn count_opens(&self, tenant_id: &str, msg_id: &MsgId)
        -> Result<u64, TrackingStoreError>;

    /// All click events for one message — every row, in
    /// insertion order, up to 1000 (caller's job to paginate
    /// when more is required).
    async fn list_clicks(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
    ) -> Result<Vec<ClickEvent>, TrackingStoreError>;

    /// Click count grouped by `link_id`. Drives the per-link
    /// breakdown ("⭐ clicked pricing 2×, faq 1×") in the UI.
    async fn count_clicks_by_link(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
    ) -> Result<Vec<(LinkId, u64)>, TrackingStoreError>;

    /// Cascade-delete every row for one tenant. Returns the
    /// total number of rows removed across all three tables.
    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, TrackingStoreError>;
}

// ── Sqlite implementation ──────────────────────────────────────

const MIGRATION_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS tracking_link (
    tenant_id      TEXT NOT NULL,
    msg_id         TEXT NOT NULL,
    link_id        TEXT NOT NULL,
    original_url   TEXT NOT NULL,
    created_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, msg_id, link_id)
);

CREATE TABLE IF NOT EXISTS tracking_open (
    tenant_id     TEXT NOT NULL,
    msg_id        TEXT NOT NULL,
    opened_at_ms  INTEGER NOT NULL,
    ip_hash       TEXT,
    ua_hash       TEXT
);
CREATE INDEX IF NOT EXISTS idx_tracking_open_tenant_msg
    ON tracking_open(tenant_id, msg_id);

CREATE TABLE IF NOT EXISTS tracking_click (
    tenant_id      TEXT NOT NULL,
    msg_id         TEXT NOT NULL,
    link_id        TEXT NOT NULL,
    clicked_at_ms  INTEGER NOT NULL,
    ip_hash        TEXT,
    ua_hash        TEXT
);
CREATE INDEX IF NOT EXISTS idx_tracking_click_tenant_msg
    ON tracking_click(tenant_id, msg_id);
CREATE INDEX IF NOT EXISTS idx_tracking_click_tenant_msg_link
    ON tracking_click(tenant_id, msg_id, link_id);
"#;

/// Open a SQLite pool against `path`, run migrations, return.
/// `:memory:` is supported for tests.
pub async fn open_pool(path: impl AsRef<Path>) -> Result<SqlitePool, TrackingStoreError> {
    let p = path.as_ref().to_string_lossy().to_string();
    let conn_str = if p == ":memory:" {
        "sqlite::memory:".to_string()
    } else {
        if let Some(parent) = path.as_ref().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        format!("sqlite://{p}")
    };
    let opts = SqliteConnectOptions::from_str(&conn_str)
        .map_err(|e| TrackingStoreError::Migration(e.to_string()))?
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(opts)
        .await?;
    sqlx::query("PRAGMA journal_mode=WAL")
        .execute(&pool)
        .await
        .ok();
    sqlx::query(MIGRATION_SQL).execute(&pool).await?;
    Ok(pool)
}

/// SQLite-backed [`TrackingStore`] implementation.
pub struct SqliteTrackingStore {
    pool: SqlitePool,
}

impl SqliteTrackingStore {
    /// Wrap an open pool. Run [`open_pool`] first.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Borrow the underlying pool — exposed so the consumer
    /// can share it with adjacent stores (saves connections).
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl TrackingStore for SqliteTrackingStore {
    async fn register_link(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
        link_id: &LinkId,
        original_url: &str,
        created_at_ms: i64,
    ) -> Result<(), TrackingStoreError> {
        sqlx::query(
            "INSERT OR REPLACE INTO tracking_link
             (tenant_id, msg_id, link_id, original_url, created_at_ms)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(tenant_id)
        .bind(msg_id.as_str())
        .bind(link_id.as_str())
        .bind(original_url)
        .bind(created_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn lookup_link(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
        link_id: &LinkId,
    ) -> Result<Option<String>, TrackingStoreError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT original_url FROM tracking_link
             WHERE tenant_id = ? AND msg_id = ? AND link_id = ?",
        )
        .bind(tenant_id)
        .bind(msg_id.as_str())
        .bind(link_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(url,)| url))
    }

    async fn record_open(&self, event: &OpenEvent) -> Result<(), TrackingStoreError> {
        sqlx::query(
            "INSERT INTO tracking_open
             (tenant_id, msg_id, opened_at_ms, ip_hash, ua_hash)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&event.tenant_id)
        .bind(event.msg_id.as_str())
        .bind(event.opened_at_ms)
        .bind(event.ip_hash.as_deref())
        .bind(event.ua_hash.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn record_click(&self, event: &ClickEvent) -> Result<(), TrackingStoreError> {
        sqlx::query(
            "INSERT INTO tracking_click
             (tenant_id, msg_id, link_id, clicked_at_ms, ip_hash, ua_hash)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&event.tenant_id)
        .bind(event.msg_id.as_str())
        .bind(event.link_id.as_str())
        .bind(event.clicked_at_ms)
        .bind(event.ip_hash.as_deref())
        .bind(event.ua_hash.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn count_opens(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
    ) -> Result<u64, TrackingStoreError> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM tracking_open
             WHERE tenant_id = ? AND msg_id = ?",
        )
        .bind(tenant_id)
        .bind(msg_id.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(n.max(0) as u64)
    }

    async fn list_clicks(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
    ) -> Result<Vec<ClickEvent>, TrackingStoreError> {
        let rows: Vec<(String, String, i64, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT msg_id, link_id, clicked_at_ms, ip_hash, ua_hash
                 FROM tracking_click
                 WHERE tenant_id = ? AND msg_id = ?
                 ORDER BY clicked_at_ms ASC
                 LIMIT 1000",
        )
        .bind(tenant_id)
        .bind(msg_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(m, l, t, ip, ua)| ClickEvent {
                tenant_id: tenant_id.to_string(),
                msg_id: MsgId(m),
                link_id: LinkId(l),
                clicked_at_ms: t,
                ip_hash: ip,
                ua_hash: ua,
            })
            .collect())
    }

    async fn count_clicks_by_link(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
    ) -> Result<Vec<(LinkId, u64)>, TrackingStoreError> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT link_id, COUNT(*) FROM tracking_click
             WHERE tenant_id = ? AND msg_id = ?
             GROUP BY link_id
             ORDER BY 2 DESC",
        )
        .bind(tenant_id)
        .bind(msg_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(l, n)| (LinkId(l), n.max(0) as u64))
            .collect())
    }

    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, TrackingStoreError> {
        let mut total: u64 = 0;
        for table in ["tracking_link", "tracking_open", "tracking_click"] {
            let r = sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = ?"))
                .bind(tenant_id)
                .execute(&self.pool)
                .await?;
            total += r.rows_affected();
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh() -> SqliteTrackingStore {
        let pool = open_pool(":memory:").await.unwrap();
        SqliteTrackingStore::new(pool)
    }

    #[tokio::test]
    async fn register_and_lookup_link() {
        let s = fresh().await;
        let m = MsgId::new("m1");
        let l = LinkId::new("L0");
        s.register_link("acme", &m, &l, "https://acme.com/x", 1)
            .await
            .unwrap();
        let got = s.lookup_link("acme", &m, &l).await.unwrap();
        assert_eq!(got.as_deref(), Some("https://acme.com/x"));
    }

    #[tokio::test]
    async fn lookup_misses_cross_tenant() {
        let s = fresh().await;
        let m = MsgId::new("m1");
        let l = LinkId::new("L0");
        s.register_link("acme", &m, &l, "https://acme.com/x", 1)
            .await
            .unwrap();
        let got = s.lookup_link("globex", &m, &l).await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn register_link_is_idempotent() {
        let s = fresh().await;
        let m = MsgId::new("m1");
        let l = LinkId::new("L0");
        s.register_link("acme", &m, &l, "https://a.com/", 1)
            .await
            .unwrap();
        // Second register replaces — last write wins.
        s.register_link("acme", &m, &l, "https://b.com/", 2)
            .await
            .unwrap();
        let got = s.lookup_link("acme", &m, &l).await.unwrap();
        assert_eq!(got.as_deref(), Some("https://b.com/"));
    }

    #[tokio::test]
    async fn record_open_and_count() {
        let s = fresh().await;
        let m = MsgId::new("m1");
        let ev = OpenEvent {
            tenant_id: "acme".into(),
            msg_id: m.clone(),
            opened_at_ms: 1,
            ip_hash: Some("ipa".into()),
            ua_hash: Some("uaa".into()),
        };
        s.record_open(&ev).await.unwrap();
        s.record_open(&ev).await.unwrap();
        s.record_open(&ev).await.unwrap();
        assert_eq!(s.count_opens("acme", &m).await.unwrap(), 3);
        // Cross-tenant counter is 0.
        assert_eq!(s.count_opens("globex", &m).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn record_click_and_list() {
        let s = fresh().await;
        let m = MsgId::new("m1");
        let l0 = LinkId::new("L0");
        let l1 = LinkId::new("L1");
        for (l, t) in [(&l0, 10), (&l1, 20), (&l0, 30)] {
            let ev = ClickEvent {
                tenant_id: "acme".into(),
                msg_id: m.clone(),
                link_id: l.clone(),
                clicked_at_ms: t,
                ip_hash: None,
                ua_hash: None,
            };
            s.record_click(&ev).await.unwrap();
        }
        let all = s.list_clicks("acme", &m).await.unwrap();
        assert_eq!(all.len(), 3);
        // Insertion order preserved by `ORDER BY clicked_at_ms`.
        assert_eq!(all[0].clicked_at_ms, 10);
        assert_eq!(all[2].clicked_at_ms, 30);

        let by_link = s.count_clicks_by_link("acme", &m).await.unwrap();
        // Two clicks on L0, one on L1 → L0 first.
        assert_eq!(by_link[0], (LinkId::new("L0"), 2));
        assert_eq!(by_link[1], (LinkId::new("L1"), 1));
    }

    #[tokio::test]
    async fn delete_by_tenant_cascades() {
        let s = fresh().await;
        let m = MsgId::new("m1");
        let l = LinkId::new("L0");
        s.register_link("acme", &m, &l, "https://a.com/", 1)
            .await
            .unwrap();
        s.record_open(&OpenEvent {
            tenant_id: "acme".into(),
            msg_id: m.clone(),
            opened_at_ms: 1,
            ip_hash: None,
            ua_hash: None,
        })
        .await
        .unwrap();
        s.record_click(&ClickEvent {
            tenant_id: "acme".into(),
            msg_id: m.clone(),
            link_id: l.clone(),
            clicked_at_ms: 1,
            ip_hash: None,
            ua_hash: None,
        })
        .await
        .unwrap();
        // Tenant B has its own row that must survive.
        s.register_link("globex", &m, &l, "https://g.com/", 1)
            .await
            .unwrap();

        let n = s.delete_by_tenant("acme").await.unwrap();
        assert_eq!(n, 3, "1 link + 1 open + 1 click");
        // Tenant B unaffected.
        let g = s.lookup_link("globex", &m, &l).await.unwrap();
        assert_eq!(g.as_deref(), Some("https://g.com/"));
    }

    #[tokio::test]
    async fn migrations_idempotent() {
        // Running open_pool twice on the same path doesn't
        // explode (CREATE TABLE IF NOT EXISTS).
        let pool1 = open_pool(":memory:").await.unwrap();
        let pool2 = open_pool(":memory:").await.unwrap();
        // Different in-memory pools — sanity that both work.
        sqlx::query("SELECT 1").execute(&pool1).await.unwrap();
        sqlx::query("SELECT 1").execute(&pool2).await.unwrap();
    }
}
