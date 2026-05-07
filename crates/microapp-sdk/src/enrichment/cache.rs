use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use thiserror::Error;

/// Stored enrichment payload — opaque JSON keyed by
/// `(tenant_id, domain)`. Consumers serialise their own
/// enrichment shape (e.g. `Company` from
/// `nexo-tool-meta::marketing`) into the `payload_json`
/// before insert.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedEntry {
    pub tenant_id: String,
    pub domain: String,
    pub payload_json: String,
    pub stored_at_ms: i64,
    pub expires_at_ms: i64,
    /// Optional ETag from the upstream HTTP source (e.g. the
    /// scraper's last response). Allows revalidate-vs-refetch.
    pub etag: Option<String>,
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("invalid domain: {0:?}")]
    InvalidDomain(String),
}

#[async_trait]
pub trait EnrichmentCache: Send + Sync {
    /// Lookup `(tenant_id, domain)`. Returns `Ok(None)` when
    /// the row doesn't exist OR the row is past its
    /// `expires_at_ms` (caller treats both as a miss).
    async fn get(
        &self,
        tenant_id: &str,
        domain: &str,
        now_ms: i64,
    ) -> Result<Option<CachedEntry>, CacheError>;

    /// Upsert a fresh enrichment. `ttl_ms` set to ~30 days for
    /// corporate domains, infinite (i64::MAX) for personal
    /// providers per convention.
    async fn put(
        &self,
        tenant_id: &str,
        domain: &str,
        payload_json: &str,
        ttl_ms: i64,
        etag: Option<String>,
        now_ms: i64,
    ) -> Result<CachedEntry, CacheError>;

    /// Force a domain re-fetch on next `get` by removing the
    /// row. Useful for operator-triggered "refresh enrichment"
    /// actions.
    async fn invalidate(
        &self,
        tenant_id: &str,
        domain: &str,
    ) -> Result<bool, CacheError>;

    /// Wipe every cached row for a tenant (called on tenant
    /// deletion).
    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, CacheError>;
}

const MIGRATION_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS enrichment_cache (
    tenant_id      TEXT NOT NULL,
    domain         TEXT NOT NULL,
    payload_json   TEXT NOT NULL,
    stored_at_ms   INTEGER NOT NULL,
    expires_at_ms  INTEGER NOT NULL,
    etag           TEXT,
    PRIMARY KEY (tenant_id, domain)
);
CREATE INDEX IF NOT EXISTS idx_enrichment_cache_expiry
    ON enrichment_cache(expires_at_ms);
"#;

/// Run the migration against a sqlite pool. Idempotent — safe
/// to call on every boot.
pub async fn migrate(pool: &SqlitePool) -> Result<(), CacheError> {
    sqlx::query(MIGRATION_SQL).execute(pool).await?;
    Ok(())
}

#[derive(Clone)]
pub struct SqliteEnrichmentCache {
    pool: SqlitePool,
}

impl SqliteEnrichmentCache {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl EnrichmentCache for SqliteEnrichmentCache {
    async fn get(
        &self,
        tenant_id: &str,
        domain: &str,
        now_ms: i64,
    ) -> Result<Option<CachedEntry>, CacheError> {
        validate_domain(domain)?;
        let row = sqlx::query_as::<_, CachedEntryRow>(
            "SELECT tenant_id, domain, payload_json, stored_at_ms, expires_at_ms, etag \
             FROM enrichment_cache WHERE tenant_id = ? AND domain = ?",
        )
        .bind(tenant_id)
        .bind(domain.to_ascii_lowercase())
        .fetch_optional(&self.pool)
        .await?;
        // Treat expired rows as a miss without deleting them
        // — caller can decide whether to refresh + put (the
        // common case) or invalidate explicitly.
        Ok(row
            .map(CachedEntryRow::into_entry)
            .filter(|e| e.expires_at_ms > now_ms))
    }

    async fn put(
        &self,
        tenant_id: &str,
        domain: &str,
        payload_json: &str,
        ttl_ms: i64,
        etag: Option<String>,
        now_ms: i64,
    ) -> Result<CachedEntry, CacheError> {
        validate_domain(domain)?;
        let lower = domain.to_ascii_lowercase();
        let expires = now_ms.saturating_add(ttl_ms);
        sqlx::query(
            "INSERT INTO enrichment_cache \
             (tenant_id, domain, payload_json, stored_at_ms, expires_at_ms, etag) \
             VALUES (?,?,?,?,?,?) \
             ON CONFLICT(tenant_id, domain) DO UPDATE SET \
              payload_json=excluded.payload_json, \
              stored_at_ms=excluded.stored_at_ms, \
              expires_at_ms=excluded.expires_at_ms, \
              etag=excluded.etag",
        )
        .bind(tenant_id)
        .bind(&lower)
        .bind(payload_json)
        .bind(now_ms)
        .bind(expires)
        .bind(etag.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(CachedEntry {
            tenant_id: tenant_id.to_string(),
            domain: lower,
            payload_json: payload_json.to_string(),
            stored_at_ms: now_ms,
            expires_at_ms: expires,
            etag,
        })
    }

    async fn invalidate(
        &self,
        tenant_id: &str,
        domain: &str,
    ) -> Result<bool, CacheError> {
        let r = sqlx::query(
            "DELETE FROM enrichment_cache WHERE tenant_id = ? AND domain = ?",
        )
        .bind(tenant_id)
        .bind(domain.to_ascii_lowercase())
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, CacheError> {
        let r = sqlx::query("DELETE FROM enrichment_cache WHERE tenant_id = ?")
            .bind(tenant_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}

#[derive(Debug, sqlx::FromRow)]
struct CachedEntryRow {
    tenant_id: String,
    domain: String,
    payload_json: String,
    stored_at_ms: i64,
    expires_at_ms: i64,
    etag: Option<String>,
}

impl CachedEntryRow {
    fn into_entry(self) -> CachedEntry {
        CachedEntry {
            tenant_id: self.tenant_id,
            domain: self.domain,
            payload_json: self.payload_json,
            stored_at_ms: self.stored_at_ms,
            expires_at_ms: self.expires_at_ms,
            etag: self.etag,
        }
    }
}

fn validate_domain(d: &str) -> Result<(), CacheError> {
    if d.is_empty() || d.contains(' ') || d.len() > 253 {
        return Err(CacheError::InvalidDomain(d.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn fresh() -> SqliteEnrichmentCache {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        migrate(&pool).await.unwrap();
        SqliteEnrichmentCache::new(pool)
    }

    #[tokio::test]
    async fn put_then_get_returns_value() {
        let c = fresh().await;
        c.put("acme", "globex.io", "{\"x\":1}", 60_000, None, 0)
            .await
            .unwrap();
        let got = c.get("acme", "globex.io", 1_000).await.unwrap().unwrap();
        assert_eq!(got.payload_json, "{\"x\":1}");
    }

    #[tokio::test]
    async fn expired_row_returns_none() {
        let c = fresh().await;
        c.put("acme", "globex.io", "{}", 1_000, None, 0).await.unwrap();
        let miss = c.get("acme", "globex.io", 5_000).await.unwrap();
        assert!(miss.is_none());
    }

    #[tokio::test]
    async fn tenant_isolation_same_domain() {
        let c = fresh().await;
        c.put("acme", "shared.io", "{\"a\":1}", 60_000, None, 0)
            .await
            .unwrap();
        c.put("globex", "shared.io", "{\"g\":1}", 60_000, None, 0)
            .await
            .unwrap();
        let acme = c.get("acme", "shared.io", 0).await.unwrap().unwrap();
        let globex = c.get("globex", "shared.io", 0).await.unwrap().unwrap();
        assert_ne!(acme.payload_json, globex.payload_json);
    }

    #[tokio::test]
    async fn invalidate_removes_row() {
        let c = fresh().await;
        c.put("acme", "globex.io", "{}", 60_000, None, 0).await.unwrap();
        let removed = c.invalidate("acme", "globex.io").await.unwrap();
        assert!(removed);
        let miss = c.get("acme", "globex.io", 0).await.unwrap();
        assert!(miss.is_none());
    }

    #[tokio::test]
    async fn delete_by_tenant_scoped() {
        let c = fresh().await;
        c.put("acme", "a.io", "{}", 60_000, None, 0).await.unwrap();
        c.put("acme", "b.io", "{}", 60_000, None, 0).await.unwrap();
        c.put("globex", "a.io", "{}", 60_000, None, 0).await.unwrap();
        let n = c.delete_by_tenant("acme").await.unwrap();
        assert_eq!(n, 2);
        let globex = c.get("globex", "a.io", 0).await.unwrap();
        assert!(globex.is_some());
    }

    #[tokio::test]
    async fn put_normalises_domain_case() {
        let c = fresh().await;
        c.put("acme", "Globex.IO", "{}", 60_000, None, 0).await.unwrap();
        let got = c.get("acme", "globex.io", 0).await.unwrap();
        assert!(got.is_some());
    }

    #[tokio::test]
    async fn put_rejects_empty_domain() {
        let c = fresh().await;
        let r = c.put("acme", "", "{}", 60_000, None, 0).await;
        assert!(matches!(r, Err(CacheError::InvalidDomain(_))));
    }

    #[tokio::test]
    async fn etag_round_trips() {
        let c = fresh().await;
        c.put("acme", "globex.io", "{}", 60_000, Some("abc123".into()), 0)
            .await
            .unwrap();
        let got = c.get("acme", "globex.io", 0).await.unwrap().unwrap();
        assert_eq!(got.etag.as_deref(), Some("abc123"));
    }
}
