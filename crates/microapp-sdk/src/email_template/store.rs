//! SQLite store for per-tenant email templates. Lives on the
//! marketing extension's identity pool to share the same
//! per-tenant SQLite file the spam filter / scoring config /
//! marketing state caches use.

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use super::blocks::EmailBlock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmailTemplate {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub blocks: Vec<EmailBlock>,
    /// Optional page-level background colour (`#rrggbb`)
    /// applied to the email's outer `<body>` + the
    /// `100%`-wide wrapper table around the 600 px card.
    /// `None` ⇒ render uses the conventional `#f5f5f5`
    /// "card on page" default. Operator-set values reach
    /// recipients verbatim (not just the preview).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_background: Option<String>,
    /// Optional page-level background image URL. Combines with
    /// `page_background` colour (image stacks on top, colour
    /// shows through transparency + acts as fallback when the
    /// recipient's client blocks remote images). `http(s)://`
    /// only — `sanitize_url` rejects everything else at render
    /// time. `None` ⇒ colour-only background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_background_image: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Error)]
pub enum TemplateStoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("invalid blocks json: {0}")]
    InvalidJson(String),
    #[error("template not found: {0:?}")]
    NotFound(String),
    #[error("template name required")]
    MissingName,
}

const MIGRATION_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS marketing_email_templates (
    tenant_id        TEXT NOT NULL,
    id               TEXT NOT NULL,
    name             TEXT NOT NULL,
    blocks_json      TEXT NOT NULL,
    page_background  TEXT,
    page_background_image TEXT,
    updated_at_ms    INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_marketing_email_templates_tenant_name
    ON marketing_email_templates(tenant_id, name);
"#;

/// Idempotent ALTER for existing DBs that pre-date the
/// page_background column. New DBs get it via MIGRATION_SQL;
/// duplicate-column error on those is the success signal.
const MIGRATION_SQL_ADD_PAGE_BACKGROUND: &str =
    "ALTER TABLE marketing_email_templates ADD COLUMN page_background TEXT";

/// Idempotent ALTER for the page_background_image column —
/// added in a later phase than `page_background`, so existing
/// DBs that already ran the previous migration only need this
/// new column.
const MIGRATION_SQL_ADD_PAGE_BACKGROUND_IMAGE: &str =
    "ALTER TABLE marketing_email_templates ADD COLUMN page_background_image TEXT";

pub async fn migrate(pool: &SqlitePool) -> Result<(), TemplateStoreError> {
    sqlx::query(MIGRATION_SQL).execute(pool).await?;
    for stmt in [
        MIGRATION_SQL_ADD_PAGE_BACKGROUND,
        MIGRATION_SQL_ADD_PAGE_BACKGROUND_IMAGE,
    ] {
        if let Err(e) = sqlx::query(stmt).execute(pool).await {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(TemplateStoreError::Sqlite(e));
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct EmailTemplateStore {
    pool: SqlitePool,
}

impl EmailTemplateStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn list(&self, tenant_id: &str) -> Result<Vec<EmailTemplate>, TemplateStoreError> {
        let rows = sqlx::query(
            "SELECT id, name, blocks_json, page_background, page_background_image, updated_at_ms \
             FROM marketing_email_templates \
             WHERE tenant_id = ? \
             ORDER BY updated_at_ms DESC",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let blocks_json: String = r.try_get("blocks_json")?;
            let blocks: Vec<EmailBlock> = serde_json::from_str(&blocks_json)
                .map_err(|e| TemplateStoreError::InvalidJson(e.to_string()))?;
            // Migrate legacy flat-blocks templates to the
            // Row/Column model on read so the editor always
            // sees Rows. Idempotent on already-Row data.
            let blocks = super::blocks::migrate_legacy_to_rows(blocks);
            out.push(EmailTemplate {
                id: r.try_get("id")?,
                tenant_id: tenant_id.to_string(),
                name: r.try_get("name")?,
                blocks,
                page_background: r.try_get("page_background")?,
                page_background_image: r.try_get("page_background_image")?,
                updated_at_ms: r.try_get("updated_at_ms")?,
            });
        }
        Ok(out)
    }

    pub async fn get(
        &self,
        tenant_id: &str,
        id: &str,
    ) -> Result<Option<EmailTemplate>, TemplateStoreError> {
        let row = sqlx::query(
            "SELECT name, blocks_json, page_background, page_background_image, updated_at_ms \
             FROM marketing_email_templates \
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(tenant_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let blocks_json: String = row.try_get("blocks_json")?;
        let blocks: Vec<EmailBlock> = serde_json::from_str(&blocks_json)
            .map_err(|e| TemplateStoreError::InvalidJson(e.to_string()))?;
        let blocks = super::blocks::migrate_legacy_to_rows(blocks);
        Ok(Some(EmailTemplate {
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
            name: row.try_get("name")?,
            blocks,
            page_background: row.try_get("page_background")?,
            page_background_image: row.try_get("page_background_image")?,
            updated_at_ms: row.try_get("updated_at_ms")?,
        }))
    }

    /// Insert a new template. Generates a fresh id; caller can
    /// override by passing one through `id`. Idempotent on
    /// `(tenant_id, id)` collision.
    pub async fn create(
        &self,
        tenant_id: &str,
        name: &str,
        blocks: &[EmailBlock],
        page_background: Option<&str>,
        page_background_image: Option<&str>,
        now_ms: i64,
    ) -> Result<EmailTemplate, TemplateStoreError> {
        if name.trim().is_empty() {
            return Err(TemplateStoreError::MissingName);
        }
        let id = format!("tpl-{}", Uuid::new_v4());
        let blocks_json = serde_json::to_string(blocks)
            .map_err(|e| TemplateStoreError::InvalidJson(e.to_string()))?;
        sqlx::query(
            "INSERT INTO marketing_email_templates \
                 (tenant_id, id, name, blocks_json, page_background, page_background_image, updated_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(tenant_id)
        .bind(&id)
        .bind(name.trim())
        .bind(&blocks_json)
        .bind(page_background)
        .bind(page_background_image)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(EmailTemplate {
            id,
            tenant_id: tenant_id.to_string(),
            name: name.trim().to_string(),
            blocks: blocks.to_vec(),
            page_background: page_background.map(|s| s.to_string()),
            page_background_image: page_background_image.map(|s| s.to_string()),
            updated_at_ms: now_ms,
        })
    }

    pub async fn update(
        &self,
        tenant_id: &str,
        id: &str,
        name: &str,
        blocks: &[EmailBlock],
        page_background: Option<&str>,
        page_background_image: Option<&str>,
        now_ms: i64,
    ) -> Result<EmailTemplate, TemplateStoreError> {
        if name.trim().is_empty() {
            return Err(TemplateStoreError::MissingName);
        }
        let blocks_json = serde_json::to_string(blocks)
            .map_err(|e| TemplateStoreError::InvalidJson(e.to_string()))?;
        let res = sqlx::query(
            "UPDATE marketing_email_templates \
             SET name = ?, blocks_json = ?, page_background = ?, page_background_image = ?, updated_at_ms = ? \
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(name.trim())
        .bind(&blocks_json)
        .bind(page_background)
        .bind(page_background_image)
        .bind(now_ms)
        .bind(tenant_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(TemplateStoreError::NotFound(id.to_string()));
        }
        Ok(EmailTemplate {
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
            name: name.trim().to_string(),
            blocks: blocks.to_vec(),
            page_background: page_background.map(|s| s.to_string()),
            page_background_image: page_background_image.map(|s| s.to_string()),
            updated_at_ms: now_ms,
        })
    }

    pub async fn delete(&self, tenant_id: &str, id: &str) -> Result<bool, TemplateStoreError> {
        let res = sqlx::query(
            "DELETE FROM marketing_email_templates \
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(tenant_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::email_template::blocks::TextAlign;
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

    fn fixture_blocks() -> Vec<EmailBlock> {
        vec![
            EmailBlock::Heading {
                text: "Hola {{name}}".into(),
                text_html: None,
                level: 1,
                color: None,
                align: TextAlign::Center,
            },
            EmailBlock::Paragraph {
                text: "Gracias por escribirnos.".into(),
                text_html: None,
                color: None,
                align: TextAlign::Left,
                font_size: 16,
            },
        ]
    }

    #[tokio::test]
    async fn create_then_list_returns_row() {
        let s = EmailTemplateStore::new(pool().await);
        let created = s
            .create("t1", "Welcome", &fixture_blocks(), None, None, 1_000)
            .await
            .unwrap();
        assert_eq!(created.name, "Welcome");
        let list = s.list("t1").await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, created.id);
        assert_eq!(list[0].blocks.len(), 2);
    }

    #[tokio::test]
    async fn update_replaces_blocks_and_name() {
        let s = EmailTemplateStore::new(pool().await);
        let created = s
            .create("t1", "Welcome", &fixture_blocks(), None, None, 1_000)
            .await
            .unwrap();
        let new_blocks = vec![EmailBlock::Spacer { height_px: 32 }];
        let updated = s
            .update(
                "t1",
                &created.id,
                "Welcome v2",
                &new_blocks,
                None,
                None,
                2_000,
            )
            .await
            .unwrap();
        assert_eq!(updated.name, "Welcome v2");
        assert_eq!(updated.blocks.len(), 1);
        assert_eq!(updated.updated_at_ms, 2_000);
    }

    #[tokio::test]
    async fn update_unknown_returns_not_found() {
        let s = EmailTemplateStore::new(pool().await);
        let err = s
            .update("t1", "ghost-id", "x", &fixture_blocks(), None, None, 1)
            .await
            .unwrap_err();
        assert!(matches!(err, TemplateStoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn delete_removes_row() {
        let s = EmailTemplateStore::new(pool().await);
        let c = s
            .create("t1", "x", &fixture_blocks(), None, None, 1)
            .await
            .unwrap();
        assert!(s.delete("t1", &c.id).await.unwrap());
        assert!(s.list("t1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn tenant_isolation() {
        let s = EmailTemplateStore::new(pool().await);
        s.create("t1", "n", &fixture_blocks(), None, None, 1)
            .await
            .unwrap();
        s.create("t2", "n", &fixture_blocks(), None, None, 1)
            .await
            .unwrap();
        assert_eq!(s.list("t1").await.unwrap().len(), 1);
        assert_eq!(s.list("t2").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn empty_name_rejected() {
        let s = EmailTemplateStore::new(pool().await);
        let err = s
            .create("t1", "  ", &fixture_blocks(), None, None, 1)
            .await
            .unwrap_err();
        assert!(matches!(err, TemplateStoreError::MissingName));
    }
}
