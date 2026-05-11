//! Per-tenant blob store for email-template assets — images
//! the operator drops into a `Image` block. Reuses the same
//! per-tenant SQLite pool the `EmailTemplateStore` lives on so
//! one extension boot opens one DB file per tenant.
//!
//! Bytes live in a `BLOB` column rather than the filesystem
//! because:
//! - sqlx already opens the pool with WAL + serial writes; one
//!   transaction per upload is atomic by construction.
//! - per-tenant isolation falls out of the schema — no path
//!   traversal risk via filesystem layout.
//! - backup/restore copies one `.db` file instead of a tree of
//!   loose images.
//! - typical inline-email images are < 1 MB; SQLite's BLOB
//!   I/O is fine well past that. The upload handler caps at
//!   `MAX_ASSET_BYTES`.
//!
//! Lookup is by SHA-256 (lowercase hex). Two operators
//! uploading the same logo bytes resolve to the same asset
//! row — `INSERT OR IGNORE` keeps the first write and the
//! second falls through to a no-op. The public `GET` route
//! relies on this immutability to serve `Cache-Control:
//! public, max-age=31536000, immutable`.

use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use thiserror::Error;

/// 5 MB cap. Anything larger is almost certainly a screenshot
/// the operator pasted by accident — most mail clients hard-
/// cap inline images well below this. The handler enforces
/// the same number on the multipart side so a forged
/// `Content-Length` can't slip through.
pub const MAX_ASSET_BYTES: usize = 5 * 1024 * 1024;

/// Whitelisted MIME types. PNG / JPEG / GIF / WebP cover
/// every email client; SVG is excluded on purpose because it
/// can carry `<script>` and most clients refuse it anyway.
pub const ALLOWED_MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

#[derive(Debug, Error)]
pub enum AssetStoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("asset too large: {size} bytes (max {max})")]
    TooLarge { size: usize, max: usize },
    #[error("mime type not allowed: {0}")]
    MimeNotAllowed(String),
    #[error("empty asset")]
    Empty,
}

const MIGRATION_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS marketing_email_template_assets (
    tenant_id     TEXT NOT NULL,
    sha256        TEXT NOT NULL,
    mime          TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    bytes         BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, sha256)
);
"#;

pub async fn migrate(pool: &SqlitePool) -> Result<(), AssetStoreError> {
    sqlx::query(MIGRATION_SQL).execute(pool).await?;
    Ok(())
}

/// One stored asset: the bytes the public `GET` streams +
/// the MIME the `Content-Type` header carries.
#[derive(Debug, Clone)]
pub struct StoredAsset {
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// Metadata-only row for the library grid. Bytes are omitted
/// — the grid renders thumbnails via the public URL so the
/// browser cache works the same way the email client's would.
/// Listing 200 assets shouldn't pull 200 MB into the admin
/// response.
#[derive(Debug, Clone)]
pub struct AssetMetadata {
    pub sha256: String,
    pub mime: String,
    pub size_bytes: i64,
    pub created_at_ms: i64,
}

#[derive(Clone)]
pub struct AssetStore {
    pool: SqlitePool,
}

impl AssetStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert (or de-dupe) the bytes. Returns the SHA-256 hex
    /// digest the public URL embeds. `INSERT OR IGNORE` so a
    /// repeat upload of identical bytes is cheap and idempotent.
    pub async fn put(
        &self,
        tenant_id: &str,
        bytes: &[u8],
        mime: &str,
        now_ms: i64,
    ) -> Result<String, AssetStoreError> {
        if bytes.is_empty() {
            return Err(AssetStoreError::Empty);
        }
        if bytes.len() > MAX_ASSET_BYTES {
            return Err(AssetStoreError::TooLarge {
                size: bytes.len(),
                max: MAX_ASSET_BYTES,
            });
        }
        if !ALLOWED_MIMES.contains(&mime) {
            return Err(AssetStoreError::MimeNotAllowed(mime.to_string()));
        }
        let sha = sha256_hex(bytes);
        sqlx::query(
            "INSERT OR IGNORE INTO marketing_email_template_assets \
                 (tenant_id, sha256, mime, size_bytes, bytes, created_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(tenant_id)
        .bind(&sha)
        .bind(mime)
        .bind(bytes.len() as i64)
        .bind(bytes)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(sha)
    }

    /// Library listing — metadata only, newest first. Used by
    /// the operator's media-picker modal to surface every
    /// previously-uploaded asset so the same logo never gets
    /// re-uploaded twice.
    pub async fn list(&self, tenant_id: &str) -> Result<Vec<AssetMetadata>, AssetStoreError> {
        let rows = sqlx::query(
            "SELECT sha256, mime, size_bytes, created_at_ms \
             FROM marketing_email_template_assets \
             WHERE tenant_id = ? \
             ORDER BY created_at_ms DESC",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(AssetMetadata {
                sha256: r.try_get("sha256")?,
                mime: r.try_get("mime")?,
                size_bytes: r.try_get("size_bytes")?,
                created_at_ms: r.try_get("created_at_ms")?,
            });
        }
        Ok(out)
    }

    /// Remove a row. Returns `true` when something was
    /// deleted, `false` when the sha didn't exist (idempotent
    /// 404 → 200 collapse on the handler side).
    ///
    /// Note: an asset deleted here while still referenced by
    /// a saved template will surface as `inline_asset_missing`
    /// the next time the template is sent with `embed=Cid`,
    /// or as a broken `<img>` for `embed=Url`. The handler
    /// can warn the operator about templates that reference
    /// the row before letting them delete (callers thread
    /// the warning through the UI).
    pub async fn delete(&self, tenant_id: &str, sha256: &str) -> Result<bool, AssetStoreError> {
        let res = sqlx::query(
            "DELETE FROM marketing_email_template_assets \
             WHERE tenant_id = ? AND sha256 = ?",
        )
        .bind(tenant_id)
        .bind(sha256)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Public route lookup. `None` means 404; any DB error
    /// bubbles so the route can 500 cleanly.
    pub async fn get(
        &self,
        tenant_id: &str,
        sha256: &str,
    ) -> Result<Option<StoredAsset>, AssetStoreError> {
        let row = sqlx::query(
            "SELECT mime, bytes \
             FROM marketing_email_template_assets \
             WHERE tenant_id = ? AND sha256 = ?",
        )
        .bind(tenant_id)
        .bind(sha256)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some(StoredAsset {
            bytes: row.try_get("bytes")?,
            mime: row.try_get("mime")?,
        }))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        // hex without the `hex` crate — keep deps slim.
        s.push(nibble_to_hex(b >> 4));
        s.push(nibble_to_hex(b & 0x0f));
    }
    s
}

fn nibble_to_hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + n - 10) as char,
        _ => unreachable!(),
    }
}

/// Map a stored MIME to the canonical extension the public
/// URL ends with. Email clients (notably Gmail's HTML
/// rewriter) sometimes refuse to fetch image src URLs that
/// lack a recognized extension — appending one is cheap
/// insurance and the route ignores it on lookup.
pub fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let p = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        migrate(&p).await.unwrap();
        p
    }

    #[tokio::test]
    async fn put_then_get_round_trips() {
        let s = AssetStore::new(pool().await);
        let bytes = b"\x89PNG\r\n\x1a\nfake".to_vec();
        let sha = s.put("t1", &bytes, "image/png", 1).await.unwrap();
        assert_eq!(sha.len(), 64);
        let got = s.get("t1", &sha).await.unwrap().unwrap();
        assert_eq!(got.bytes, bytes);
        assert_eq!(got.mime, "image/png");
    }

    #[tokio::test]
    async fn put_is_idempotent_on_same_bytes() {
        let s = AssetStore::new(pool().await);
        let bytes = b"hello".to_vec();
        let a = s.put("t1", &bytes, "image/png", 1).await.unwrap();
        let b = s.put("t1", &bytes, "image/png", 2).await.unwrap();
        assert_eq!(a, b, "same bytes → same sha");
    }

    #[tokio::test]
    async fn tenant_isolation_on_lookup() {
        let s = AssetStore::new(pool().await);
        let bytes = b"hello".to_vec();
        let sha = s.put("t1", &bytes, "image/png", 1).await.unwrap();
        // Same sha — cross-tenant read must miss.
        assert!(s.get("t2", &sha).await.unwrap().is_none());
        assert!(s.get("t1", &sha).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn rejects_oversize() {
        let s = AssetStore::new(pool().await);
        let big = vec![0u8; MAX_ASSET_BYTES + 1];
        let err = s.put("t1", &big, "image/png", 1).await.unwrap_err();
        assert!(matches!(err, AssetStoreError::TooLarge { .. }));
    }

    #[tokio::test]
    async fn rejects_disallowed_mime() {
        let s = AssetStore::new(pool().await);
        let err = s
            .put("t1", b"<svg/>", "image/svg+xml", 1)
            .await
            .unwrap_err();
        assert!(matches!(err, AssetStoreError::MimeNotAllowed(_)));
    }

    #[tokio::test]
    async fn rejects_empty() {
        let s = AssetStore::new(pool().await);
        let err = s.put("t1", b"", "image/png", 1).await.unwrap_err();
        assert!(matches!(err, AssetStoreError::Empty));
    }

    #[tokio::test]
    async fn list_returns_newest_first() {
        let s = AssetStore::new(pool().await);
        s.put("t1", b"\x89PNG\r\n\x1a\nA", "image/png", 100)
            .await
            .unwrap();
        s.put("t1", b"\x89PNG\r\n\x1a\nB", "image/png", 300)
            .await
            .unwrap();
        s.put("t1", b"\x89PNG\r\n\x1a\nC", "image/png", 200)
            .await
            .unwrap();
        let metas = s.list("t1").await.unwrap();
        assert_eq!(metas.len(), 3);
        // 300 > 200 > 100 → newest first.
        assert_eq!(metas[0].created_at_ms, 300);
        assert_eq!(metas[1].created_at_ms, 200);
        assert_eq!(metas[2].created_at_ms, 100);
        assert!(metas.iter().all(|m| !m.sha256.is_empty()));
        assert!(metas.iter().all(|m| m.mime == "image/png"));
    }

    #[tokio::test]
    async fn list_is_tenant_scoped() {
        let s = AssetStore::new(pool().await);
        s.put("t1", b"hello", "image/png", 1).await.unwrap();
        s.put("t2", b"world", "image/png", 1).await.unwrap();
        assert_eq!(s.list("t1").await.unwrap().len(), 1);
        assert_eq!(s.list("t2").await.unwrap().len(), 1);
        assert_eq!(s.list("t3").await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn delete_removes_row() {
        let s = AssetStore::new(pool().await);
        let sha = s.put("t1", b"hello", "image/png", 1).await.unwrap();
        assert!(s.delete("t1", &sha).await.unwrap());
        assert!(s.get("t1", &sha).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_unknown_returns_false() {
        let s = AssetStore::new(pool().await);
        assert!(!s.delete("t1", &"a".repeat(64)).await.unwrap());
    }

    #[tokio::test]
    async fn delete_is_tenant_scoped() {
        let s = AssetStore::new(pool().await);
        let sha = s.put("t1", b"hello", "image/png", 1).await.unwrap();
        // Cross-tenant delete must NOT touch t1's row.
        assert!(!s.delete("t2", &sha).await.unwrap());
        assert!(s.get("t1", &sha).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn missing_lookup_returns_none() {
        let s = AssetStore::new(pool().await);
        assert!(s.get("t1", "deadbeef").await.unwrap().is_none());
    }

    #[test]
    fn sha256_hex_is_lowercase_64_chars() {
        let h = sha256_hex(b"hello");
        assert_eq!(h.len(), 64);
        assert!(h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn mime_to_ext_covers_allowed_mimes() {
        assert_eq!(mime_to_ext("image/png"), "png");
        assert_eq!(mime_to_ext("image/jpeg"), "jpg");
        assert_eq!(mime_to_ext("image/gif"), "gif");
        assert_eq!(mime_to_ext("image/webp"), "webp");
        assert_eq!(mime_to_ext("image/svg+xml"), "bin");
    }
}
