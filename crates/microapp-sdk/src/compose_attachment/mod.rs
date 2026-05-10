//! Per-tenant blob store for compose attachments — PDFs,
//! docs, spreadsheets, archives the operator wants attached
//! to an outbound email. Sibling of `email_template::asset_store`
//! but with a broader MIME whitelist + larger size cap +
//! operator-supplied filename (which `Content-Disposition:
//! attachment; filename="…"` carries downstream).
//!
//! ## Why separate from the inline-image asset store
//!
//! - **Different MIME whitelist** — image asset store
//!   refuses anything that isn't png/jpeg/gif/webp; this
//!   accepts pdf/doc/xlsx/zip/csv/txt/etc. Mixing them
//!   would mean either a permissive image whitelist (CID
//!   security regression) or a permissive attachment
//!   whitelist (every random binary lands in the
//!   media-library grid).
//! - **Different lifecycle** — image assets are referenced
//!   by saved templates and serve via a public URL; compose
//!   attachments ride along inside the email envelope and
//!   never need a public-route mount.
//! - **Different size profile** — 5 MB is too small for a
//!   contract PDF; 20 MB is too lax for inline images that
//!   bloat every send.
//! - **Different metadata** — operators care about the
//!   original filename of a "factura.pdf" so the recipient
//!   sees it, not a SHA-derived stem.
//!
//! ## Storage
//!
//! Bytes live in a `BLOB` column. Same rationale as the
//! email_template asset store: per-tenant isolation falls
//! out of the schema, atomic write per upload, backup is one
//! `.db` file. Up to 20 MB per file; the upload handler
//! caps total per-send via its own multi-attachment guard.
//!
//! De-duped by SHA-256 — re-upload of the same factura.pdf
//! collapses to the existing row even if the operator picked
//! a different filename. First filename wins (INSERT OR
//! IGNORE); operators who want to rename should delete +
//! re-upload.
//!
//! ## Table name
//!
//! Configurable per microapp like
//! [`crate::module_state`] — pass the table name to
//! [`migrate`] and [`ComposeAttachmentStore::new`]. Validated
//! against `^[a-z][a-z0-9_]{0,63}$` to prevent SQL injection
//! through the `format!()`-built statements.
//!
//! Lifted from the agent-creator-microapp's marketing
//! extension. Any microapp that ships an operator-facing
//! compose surface (helpdesk replies, transactional sends,
//! …) consumes this via the `compose-attachment` feature.

#![allow(missing_docs)]

use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use thiserror::Error;

/// 20 MB per file. Gmail rejects email payloads above ~25 MB
/// after base64 overhead; capping per-attachment at 20 MB
/// gives ~10 MB headroom for the body + multipart boundaries
/// + chained inline images. Total-attachment-size guard lives
/// at the compose handler (not the store) since one file
/// might be fine but ten of them might not.
pub const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;

/// Whitelist of MIMEs the upload handler accepts. Excludes
/// `application/x-executable` / `application/x-msdownload` /
/// raw `application/octet-stream` + every script type — an
/// operator who sends a `.exe` to a customer is almost
/// certainly making a mistake, and the recipient's mail
/// server will probably bounce it anyway. Operators with a
/// genuine need (rare) can rename to `.zip` first.
pub const ALLOWED_ATTACHMENT_MIMES: &[&str] = &[
    "application/pdf",
    "application/msword",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.ms-excel",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.ms-powerpoint",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/zip",
    "application/x-zip-compressed",
    "application/rtf",
    "text/plain",
    "text/csv",
    "text/markdown",
    // Images allowed too — operators sometimes attach a
    // screenshot rather than embed it inline. The image
    // asset store handles inline images; this path handles
    // "download me" images.
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
];

#[derive(Debug, Error)]
pub enum AttachmentStoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("attachment too large: {size} bytes (max {max})")]
    TooLarge { size: usize, max: usize },
    #[error("mime type not allowed: {0}")]
    MimeNotAllowed(String),
    #[error("empty attachment")]
    Empty,
    #[error("filename required")]
    MissingFilename,
    #[error("invalid table name: {0:?} (must match [a-z][a-z0-9_]{{0,63}})")]
    InvalidTableName(String),
}

/// Validate a table name against the allowlist regex —
/// `format!()`-built SQL needs the table identifier guarded
/// because `sqlx::query` only parameterises values, not
/// identifiers.
fn validate_table(name: &str) -> Result<(), AttachmentStoreError> {
    if name.is_empty() || name.len() > 64 {
        return Err(AttachmentStoreError::InvalidTableName(name.to_string()));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return Err(AttachmentStoreError::InvalidTableName(name.to_string()));
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(AttachmentStoreError::InvalidTableName(name.to_string()));
        }
    }
    Ok(())
}

/// Run the CREATE TABLE migration. Idempotent — safe on every
/// boot. Caller passes the same `table_name` they'll use for
/// the store.
pub async fn migrate(
    pool: &SqlitePool,
    table_name: &str,
) -> Result<(), AttachmentStoreError> {
    validate_table(table_name)?;
    let stmt = format!(
        r#"
CREATE TABLE IF NOT EXISTS {table_name} (
    tenant_id     TEXT NOT NULL,
    sha256        TEXT NOT NULL,
    mime          TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    filename      TEXT NOT NULL,
    bytes         BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, sha256)
);
"#
    );
    sqlx::query(&stmt).execute(pool).await?;
    Ok(())
}

/// One stored attachment with bytes loaded — what the
/// compose handler hands to the outbound publisher.
#[derive(Debug, Clone)]
pub struct StoredAttachment {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub filename: String,
    pub size_bytes: i64,
}

/// Metadata-only row for the library list. Bytes excluded so
/// listing 50 PDFs doesn't pull 50 × 5 MB into the admin
/// response.
#[derive(Debug, Clone)]
pub struct AttachmentMetadata {
    pub sha256: String,
    pub mime: String,
    pub size_bytes: i64,
    pub filename: String,
    pub created_at_ms: i64,
}

#[derive(Clone)]
pub struct ComposeAttachmentStore {
    pool: SqlitePool,
    table: String,
}

impl ComposeAttachmentStore {
    /// Construct a store backed by `pool` writing to
    /// `table_name`. Returns `Err(InvalidTableName)` when the
    /// name doesn't match the allowlist regex.
    pub fn new(
        pool: SqlitePool,
        table_name: impl Into<String>,
    ) -> Result<Self, AttachmentStoreError> {
        let table = table_name.into();
        validate_table(&table)?;
        Ok(Self { pool, table })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    /// Insert (or de-dupe) the bytes. SHA-256 keying means a
    /// re-upload of the same factura.pdf collapses to the
    /// existing row even if the operator picked a different
    /// filename — first filename wins. Operators who want to
    /// rename should delete + re-upload.
    pub async fn put(
        &self,
        tenant_id: &str,
        bytes: &[u8],
        mime: &str,
        filename: &str,
        now_ms: i64,
    ) -> Result<String, AttachmentStoreError> {
        if bytes.is_empty() {
            return Err(AttachmentStoreError::Empty);
        }
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentStoreError::TooLarge {
                size: bytes.len(),
                max: MAX_ATTACHMENT_BYTES,
            });
        }
        if !ALLOWED_ATTACHMENT_MIMES.contains(&mime) {
            return Err(AttachmentStoreError::MimeNotAllowed(mime.to_string()));
        }
        let trimmed_name = filename.trim();
        if trimmed_name.is_empty() {
            return Err(AttachmentStoreError::MissingFilename);
        }
        let sha = sha256_hex(bytes);
        let stmt = format!(
            "INSERT OR IGNORE INTO {} \
                 (tenant_id, sha256, mime, size_bytes, filename, bytes, created_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            self.table,
        );
        sqlx::query(&stmt)
            .bind(tenant_id)
            .bind(&sha)
            .bind(mime)
            .bind(bytes.len() as i64)
            .bind(trimmed_name)
            .bind(bytes)
            .bind(now_ms)
            .execute(&self.pool)
            .await?;
        Ok(sha)
    }

    pub async fn list(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<AttachmentMetadata>, AttachmentStoreError> {
        let stmt = format!(
            "SELECT sha256, mime, size_bytes, filename, created_at_ms \
             FROM {} \
             WHERE tenant_id = ? \
             ORDER BY created_at_ms DESC",
            self.table,
        );
        let rows = sqlx::query(&stmt)
            .bind(tenant_id)
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(AttachmentMetadata {
                sha256: r.try_get("sha256")?,
                mime: r.try_get("mime")?,
                size_bytes: r.try_get("size_bytes")?,
                filename: r.try_get("filename")?,
                created_at_ms: r.try_get("created_at_ms")?,
            });
        }
        Ok(out)
    }

    /// Compose / approve handlers call this to load bytes for
    /// each ref the operator selected. `None` ⇒ the handler
    /// errors with a clear "you deleted this since picking it"
    /// message instead of a 500.
    pub async fn get(
        &self,
        tenant_id: &str,
        sha256: &str,
    ) -> Result<Option<StoredAttachment>, AttachmentStoreError> {
        let stmt = format!(
            "SELECT mime, size_bytes, filename, bytes \
             FROM {} \
             WHERE tenant_id = ? AND sha256 = ?",
            self.table,
        );
        let row = sqlx::query(&stmt)
            .bind(tenant_id)
            .bind(sha256)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some(StoredAttachment {
            bytes: row.try_get("bytes")?,
            mime: row.try_get("mime")?,
            filename: row.try_get("filename")?,
            size_bytes: row.try_get("size_bytes")?,
        }))
    }

    pub async fn delete(
        &self,
        tenant_id: &str,
        sha256: &str,
    ) -> Result<bool, AttachmentStoreError> {
        let stmt = format!(
            "DELETE FROM {} \
             WHERE tenant_id = ? AND sha256 = ?",
            self.table,
        );
        let res = sqlx::query(&stmt)
            .bind(tenant_id)
            .bind(sha256)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
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
        migrate(&p, "test_attachments").await.unwrap();
        p
    }

    fn store(pool: SqlitePool) -> ComposeAttachmentStore {
        ComposeAttachmentStore::new(pool, "test_attachments").unwrap()
    }

    #[tokio::test]
    async fn put_then_get_round_trips() {
        let s = store(pool().await);
        let bytes = b"%PDF-1.4 fake pdf bytes".to_vec();
        let sha = s
            .put("t1", &bytes, "application/pdf", "factura.pdf", 1)
            .await
            .unwrap();
        let got = s.get("t1", &sha).await.unwrap().unwrap();
        assert_eq!(got.bytes, bytes);
        assert_eq!(got.mime, "application/pdf");
        assert_eq!(got.filename, "factura.pdf");
    }

    #[tokio::test]
    async fn dedups_same_bytes_keeps_first_filename() {
        let s = store(pool().await);
        let bytes = b"hello".to_vec();
        let a = s
            .put("t1", &bytes, "application/pdf", "first.pdf", 1)
            .await
            .unwrap();
        let b = s
            .put("t1", &bytes, "application/pdf", "second.pdf", 2)
            .await
            .unwrap();
        assert_eq!(a, b);
        let got = s.get("t1", &a).await.unwrap().unwrap();
        // First wins (INSERT OR IGNORE).
        assert_eq!(got.filename, "first.pdf");
    }

    #[tokio::test]
    async fn rejects_disallowed_mime() {
        let s = store(pool().await);
        let err = s
            .put("t1", b"MZ", "application/x-msdownload", "evil.exe", 1)
            .await
            .unwrap_err();
        assert!(matches!(err, AttachmentStoreError::MimeNotAllowed(_)));
    }

    #[tokio::test]
    async fn rejects_oversize() {
        let s = store(pool().await);
        let big = vec![0u8; MAX_ATTACHMENT_BYTES + 1];
        let err = s
            .put("t1", &big, "application/pdf", "big.pdf", 1)
            .await
            .unwrap_err();
        assert!(matches!(err, AttachmentStoreError::TooLarge { .. }));
    }

    #[tokio::test]
    async fn rejects_empty_filename() {
        let s = store(pool().await);
        let err = s
            .put("t1", b"x", "application/pdf", "   ", 1)
            .await
            .unwrap_err();
        assert!(matches!(err, AttachmentStoreError::MissingFilename));
    }

    #[tokio::test]
    async fn list_newest_first_tenant_scoped() {
        let s = store(pool().await);
        s.put("t1", b"A", "application/pdf", "a.pdf", 100)
            .await
            .unwrap();
        s.put("t1", b"B", "application/pdf", "b.pdf", 300)
            .await
            .unwrap();
        s.put("t2", b"C", "application/pdf", "c.pdf", 999)
            .await
            .unwrap();
        let l = s.list("t1").await.unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].filename, "b.pdf");
        assert_eq!(l[1].filename, "a.pdf");
    }

    #[tokio::test]
    async fn delete_idempotent_and_scoped() {
        let s = store(pool().await);
        let sha = s
            .put("t1", b"x", "application/pdf", "x.pdf", 1)
            .await
            .unwrap();
        // Cross-tenant delete must miss.
        assert!(!s.delete("t2", &sha).await.unwrap());
        assert!(s.get("t1", &sha).await.unwrap().is_some());
        assert!(s.delete("t1", &sha).await.unwrap());
        assert!(s.get("t1", &sha).await.unwrap().is_none());
        // Idempotent re-delete.
        assert!(!s.delete("t1", &sha).await.unwrap());
    }

    #[test]
    fn validate_table_rejects_injection_attempts() {
        assert!(validate_table("").is_err());
        assert!(validate_table("Bad").is_err());
        assert!(validate_table("1state").is_err());
        assert!(validate_table("attachments; DROP --").is_err());
    }
}
