//! SQLite-backed cache. One table, one connection pool. Shared across
//! every agent in the process.

use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::types::{WebSearchError, WebSearchResult};

/// Bumped whenever the on-disk format changes. Old entries become
/// misses (their key embeds this number).
const SCHEMA_VERSION: u32 = 1;

pub struct WebSearchCache {
    pool: SqlitePool,
    ttl: Duration,
}

impl WebSearchCache {
    pub async fn open(path: &str, ttl: Duration) -> Result<Self, WebSearchError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        // `:memory:` gives a *per-connection* database; the `CREATE
        // TABLE` on connection A is invisible to connection B. Pin the
        // pool to a single connection in that case (tests only).
        let max_conns = if path == ":memory:" { 1 } else { 4 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max_conns)
            .connect_with(opts)
            .await
            .map_err(|e| WebSearchError::Cache(e.to_string()))?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS web_search_cache (\
                key         TEXT PRIMARY KEY,\
                provider    TEXT NOT NULL,\
                query       TEXT NOT NULL,\
                result_json TEXT NOT NULL,\
                inserted_at INTEGER NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .map_err(|e| WebSearchError::Cache(e.to_string()))?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_web_search_cache_inserted ON web_search_cache(inserted_at)")
            .execute(&pool)
            .await
            .map_err(|e| WebSearchError::Cache(e.to_string()))?;
        Ok(Self { pool, ttl })
    }

    /// In-memory cache used for tests.
    pub async fn open_memory(ttl: Duration) -> Result<Self, WebSearchError> {
        Self::open(":memory:", ttl).await
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn key(provider: &str, query: &str, params_canonical: &str) -> String {
        let mut h = Sha256::new();
        h.update(SCHEMA_VERSION.to_le_bytes());
        h.update(provider.as_bytes());
        h.update(b"\0");
        h.update(query.as_bytes());
        h.update(b"\0");
        h.update(params_canonical.as_bytes());
        hex::encode(h.finalize())
    }

    pub async fn get(&self, key: &str) -> Result<Option<WebSearchResult>, WebSearchError> {
        if self.ttl.as_secs() == 0 {
            return Ok(None);
        }
        let now = chrono::Utc::now().timestamp();
        let cutoff = now - self.ttl.as_secs() as i64;
        let row: Option<(String, i64)> =
            sqlx::query_as("SELECT result_json, inserted_at FROM web_search_cache WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| WebSearchError::Cache(e.to_string()))?;
        match row {
            Some((json, inserted_at)) if inserted_at >= cutoff => {
                let mut parsed: WebSearchResult = serde_json::from_str(&json)
                    .map_err(|e| WebSearchError::Cache(e.to_string()))?;
                parsed.from_cache = true;
                Ok(Some(parsed))
            }
            _ => Ok(None),
        }
    }

    pub async fn put(&self, key: &str, value: &WebSearchResult) -> Result<(), WebSearchError> {
        let json =
            serde_json::to_string(value).map_err(|e| WebSearchError::Cache(e.to_string()))?;
        sqlx::query(
            "INSERT OR REPLACE INTO web_search_cache(key, provider, query, result_json, inserted_at) VALUES(?, ?, ?, ?, ?)",
        )
        .bind(key)
        .bind(&value.provider)
        .bind(&value.query)
        .bind(json)
        .bind(chrono::Utc::now().timestamp())
        .execute(&self.pool)
        .await
        .map_err(|e| WebSearchError::Cache(e.to_string()))?;
        Ok(())
    }

    pub async fn purge_expired(&self) -> Result<u64, WebSearchError> {
        let cutoff = chrono::Utc::now().timestamp() - self.ttl.as_secs() as i64;
        let res = sqlx::query("DELETE FROM web_search_cache WHERE inserted_at < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(|e| WebSearchError::Cache(e.to_string()))?;
        Ok(res.rows_affected())
    }
}
