//! Per-tenant store of operator-saved compose drafts —
//! WIP outbounds the operator wants to come back to later
//! (Gmail's "Drafts" folder semantics).
//!
//! A draft holds the full compose form state plus optional
//! builder-mode blocks + attachment refs + template binding,
//! so opening one rehydrates either a rapid-modal or a
//! full-page builder transparently. Form fields are all
//! optional — the operator can save mid-fill and the next
//! load lands them right where they left off.
//!
//! ## Table name
//!
//! Configurable per microapp like
//! [`crate::module_state`] — pass the table name to
//! [`migrate`] and [`ComposeDraftStore::new`]. Validated
//! against `^[a-z][a-z0-9_]{0,63}$` to prevent SQL injection
//! through the `format!()`-built statements.
//!
//! ## Empty-title fallback
//!
//! When the operator saves a draft with neither title nor
//! subject set, [`ComposeDraftStore::new_with_fallback`]
//! lets you pick the sentinel string the list page renders
//! (English: `"(no subject)"`, Spanish: `"(sin asunto)"`,
//! …). The default constructor uses `"(no subject)"`.
//!
//! Lifted from the agent-creator-microapp's marketing
//! extension. Any microapp with an operator compose surface
//! consumes this via the `compose-draft` feature.

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ComposeDraftError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("invalid blocks json: {0}")]
    InvalidBlocksJson(String),
    #[error("invalid template_vars json: {0}")]
    InvalidVarsJson(String),
    #[error("invalid attachment_refs json: {0}")]
    InvalidAttachmentJson(String),
    #[error("draft not found: {0:?}")]
    NotFound(String),
    #[error("invalid table name: {0:?} (must match [a-z][a-z0-9_]{{0,63}})")]
    InvalidTableName(String),
}

fn validate_table(name: &str) -> Result<(), ComposeDraftError> {
    if name.is_empty() || name.len() > 64 {
        return Err(ComposeDraftError::InvalidTableName(name.to_string()));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return Err(ComposeDraftError::InvalidTableName(name.to_string()));
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(ComposeDraftError::InvalidTableName(name.to_string()));
        }
    }
    Ok(())
}

/// Run the CREATE TABLE migration. Idempotent — safe on
/// every boot. Caller passes the same `table_name` they'll
/// use for the store.
pub async fn migrate(
    pool: &SqlitePool,
    table_name: &str,
) -> Result<(), ComposeDraftError> {
    validate_table(table_name)?;
    let stmt = format!(
        r#"
CREATE TABLE IF NOT EXISTS {table_name} (
    tenant_id           TEXT NOT NULL,
    id                  TEXT NOT NULL,
    title               TEXT NOT NULL,
    to_email            TEXT NOT NULL DEFAULT '',
    to_name             TEXT NOT NULL DEFAULT '',
    subject             TEXT NOT NULL DEFAULT '',
    body                TEXT NOT NULL DEFAULT '',
    seller_id           TEXT NOT NULL DEFAULT '',
    with_tracking       INTEGER NOT NULL DEFAULT 1,
    template_id         TEXT,
    template_vars_json  TEXT,
    blocks_json         TEXT,
    attachment_refs_json TEXT,
    -- Free-form mode label persisted so reopening a draft
    -- routes the operator back to the same UI surface they
    -- saved it from (e.g. 'rapid' modal vs 'builder' page).
    mode                TEXT NOT NULL DEFAULT 'rapid',
    created_at_ms       INTEGER NOT NULL,
    updated_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
"#
    );
    sqlx::query(&stmt).execute(pool).await?;
    let idx = format!(
        "CREATE INDEX IF NOT EXISTS idx_{table_name}_updated \
         ON {table_name}(tenant_id, updated_at_ms DESC)"
    );
    sqlx::query(&idx).execute(pool).await?;
    Ok(())
}

/// What "rehydrate this draft into the UI" needs. `mode`
/// drives which page the operator navigates to. The body /
/// blocks split is mode-aware: rapid uses `body`, builder
/// uses `blocks`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeDraft {
    pub id: String,
    /// Operator-friendly title — defaults to subject or the
    /// store's configured fallback so the list page has
    /// something to render even on a barely-filled draft.
    pub title: String,
    #[serde(default)]
    pub to_email: String,
    #[serde(default)]
    pub to_name: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub seller_id: String,
    #[serde(default = "default_true")]
    pub with_tracking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_vars: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<serde_json::Value>,
    #[serde(default)]
    pub attachment_refs: Vec<String>,
    #[serde(default = "default_mode")]
    pub mode: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

fn default_true() -> bool { true }
fn default_mode() -> String { "rapid".to_string() }

/// Patch supplied on POST (create) and PUT (update). All
/// fields are optional so the UI can save partial state — a
/// draft with only `to_email` set is valid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComposeDraftInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub to_email: Option<String>,
    #[serde(default)]
    pub to_name: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub seller_id: Option<String>,
    #[serde(default)]
    pub with_tracking: Option<bool>,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub template_vars: Option<serde_json::Value>,
    #[serde(default)]
    pub blocks: Option<serde_json::Value>,
    #[serde(default)]
    pub attachment_refs: Option<Vec<String>>,
    #[serde(default)]
    pub mode: Option<String>,
}

/// Default fallback for a missing-title draft when the
/// caller doesn't supply their own. English wording so the
/// SDK doesn't carry a locale assumption.
pub const DEFAULT_EMPTY_TITLE: &str = "(no subject)";

#[derive(Clone)]
pub struct ComposeDraftStore {
    pool: SqlitePool,
    table: String,
    empty_title: String,
}

impl ComposeDraftStore {
    /// Construct a store backed by `pool` writing to
    /// `table_name`. Uses [`DEFAULT_EMPTY_TITLE`] as the
    /// fallback when neither the operator nor their subject
    /// supplies a title.
    pub fn new(
        pool: SqlitePool,
        table_name: impl Into<String>,
    ) -> Result<Self, ComposeDraftError> {
        Self::new_with_fallback(pool, table_name, DEFAULT_EMPTY_TITLE)
    }

    /// Same as [`new`] but lets the caller pick the sentinel
    /// for the empty-title fallback (e.g. `"(sin asunto)"`
    /// for a Spanish operator UI).
    pub fn new_with_fallback(
        pool: SqlitePool,
        table_name: impl Into<String>,
        empty_title: impl Into<String>,
    ) -> Result<Self, ComposeDraftError> {
        let table = table_name.into();
        validate_table(&table)?;
        Ok(Self {
            pool,
            table,
            empty_title: empty_title.into(),
        })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub async fn list(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<ComposeDraft>, ComposeDraftError> {
        let stmt = format!(
            "SELECT id, title, to_email, to_name, subject, body, \
                    seller_id, with_tracking, template_id, \
                    template_vars_json, blocks_json, \
                    attachment_refs_json, mode, \
                    created_at_ms, updated_at_ms \
             FROM {} \
             WHERE tenant_id = ? \
             ORDER BY updated_at_ms DESC",
            self.table,
        );
        let rows = sqlx::query(&stmt)
            .bind(tenant_id)
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(row_to_draft(&r)?);
        }
        Ok(out)
    }

    pub async fn get(
        &self,
        tenant_id: &str,
        id: &str,
    ) -> Result<Option<ComposeDraft>, ComposeDraftError> {
        let stmt = format!(
            "SELECT id, title, to_email, to_name, subject, body, \
                    seller_id, with_tracking, template_id, \
                    template_vars_json, blocks_json, \
                    attachment_refs_json, mode, \
                    created_at_ms, updated_at_ms \
             FROM {} \
             WHERE tenant_id = ? AND id = ?",
            self.table,
        );
        let row = sqlx::query(&stmt)
            .bind(tenant_id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        let Some(r) = row else { return Ok(None) };
        Ok(Some(row_to_draft(&r)?))
    }

    pub async fn create(
        &self,
        tenant_id: &str,
        input: &ComposeDraftInput,
        now_ms: i64,
    ) -> Result<ComposeDraft, ComposeDraftError> {
        let id = format!("draft-{}", Uuid::new_v4());
        let title = effective_title(input, &self.empty_title);
        let with_tracking = input.with_tracking.unwrap_or(true);
        let mode = input.mode.clone().unwrap_or_else(|| "rapid".to_string());
        let blocks_json = match &input.blocks {
            Some(v) => Some(
                serde_json::to_string(v)
                    .map_err(|e| ComposeDraftError::InvalidBlocksJson(e.to_string()))?,
            ),
            None => None,
        };
        let vars_json = match &input.template_vars {
            Some(v) => Some(
                serde_json::to_string(v)
                    .map_err(|e| ComposeDraftError::InvalidVarsJson(e.to_string()))?,
            ),
            None => None,
        };
        let attachments_json = match &input.attachment_refs {
            Some(v) => Some(
                serde_json::to_string(v).map_err(|e| {
                    ComposeDraftError::InvalidAttachmentJson(e.to_string())
                })?,
            ),
            None => None,
        };

        let stmt = format!(
            "INSERT INTO {} \
                 (tenant_id, id, title, to_email, to_name, subject, body, \
                  seller_id, with_tracking, template_id, template_vars_json, \
                  blocks_json, attachment_refs_json, mode, \
                  created_at_ms, updated_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            self.table,
        );
        sqlx::query(&stmt)
            .bind(tenant_id)
            .bind(&id)
            .bind(&title)
            .bind(input.to_email.clone().unwrap_or_default())
            .bind(input.to_name.clone().unwrap_or_default())
            .bind(input.subject.clone().unwrap_or_default())
            .bind(input.body.clone().unwrap_or_default())
            .bind(input.seller_id.clone().unwrap_or_default())
            .bind(if with_tracking { 1 } else { 0 })
            .bind(input.template_id.clone())
            .bind(&vars_json)
            .bind(&blocks_json)
            .bind(&attachments_json)
            .bind(&mode)
            .bind(now_ms)
            .bind(now_ms)
            .execute(&self.pool)
            .await?;

        // Re-fetch so the response includes any defaults the
        // schema applied. Cheap and matches CRUD semantics.
        match self.get(tenant_id, &id).await? {
            Some(d) => Ok(d),
            None => Err(ComposeDraftError::NotFound(id)),
        }
    }

    pub async fn update(
        &self,
        tenant_id: &str,
        id: &str,
        input: &ComposeDraftInput,
        now_ms: i64,
    ) -> Result<ComposeDraft, ComposeDraftError> {
        // COALESCE pattern: every update statement uses the
        // input field when non-None and falls back to the
        // existing column otherwise. Cheaper than two queries
        // and keeps the patch semantics clean.
        let blocks_json = match &input.blocks {
            Some(v) => Some(
                serde_json::to_string(v)
                    .map_err(|e| ComposeDraftError::InvalidBlocksJson(e.to_string()))?,
            ),
            None => None,
        };
        let vars_json = match &input.template_vars {
            Some(v) => Some(
                serde_json::to_string(v)
                    .map_err(|e| ComposeDraftError::InvalidVarsJson(e.to_string()))?,
            ),
            None => None,
        };
        let attachments_json = match &input.attachment_refs {
            Some(v) => Some(
                serde_json::to_string(v).map_err(|e| {
                    ComposeDraftError::InvalidAttachmentJson(e.to_string())
                })?,
            ),
            None => None,
        };

        let stmt = format!(
            "UPDATE {} \
             SET title = COALESCE(?, title), \
                 to_email = COALESCE(?, to_email), \
                 to_name = COALESCE(?, to_name), \
                 subject = COALESCE(?, subject), \
                 body = COALESCE(?, body), \
                 seller_id = COALESCE(?, seller_id), \
                 with_tracking = COALESCE(?, with_tracking), \
                 template_id = COALESCE(?, template_id), \
                 template_vars_json = COALESCE(?, template_vars_json), \
                 blocks_json = COALESCE(?, blocks_json), \
                 attachment_refs_json = COALESCE(?, attachment_refs_json), \
                 mode = COALESCE(?, mode), \
                 updated_at_ms = ? \
             WHERE tenant_id = ? AND id = ?",
            self.table,
        );
        let res = sqlx::query(&stmt)
            .bind(input.title.clone())
            .bind(input.to_email.clone())
            .bind(input.to_name.clone())
            .bind(input.subject.clone())
            .bind(input.body.clone())
            .bind(input.seller_id.clone())
            .bind(input.with_tracking.map(|b| if b { 1i64 } else { 0i64 }))
            .bind(input.template_id.clone())
            .bind(&vars_json)
            .bind(&blocks_json)
            .bind(&attachments_json)
            .bind(input.mode.clone())
            .bind(now_ms)
            .bind(tenant_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(ComposeDraftError::NotFound(id.to_string()));
        }
        match self.get(tenant_id, id).await? {
            Some(d) => Ok(d),
            None => Err(ComposeDraftError::NotFound(id.to_string())),
        }
    }

    pub async fn delete(
        &self,
        tenant_id: &str,
        id: &str,
    ) -> Result<bool, ComposeDraftError> {
        let stmt = format!(
            "DELETE FROM {} WHERE tenant_id = ? AND id = ?",
            self.table,
        );
        let res = sqlx::query(&stmt)
            .bind(tenant_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

fn row_to_draft(r: &sqlx::sqlite::SqliteRow) -> Result<ComposeDraft, ComposeDraftError> {
    let with_tracking: i64 = r.try_get("with_tracking")?;
    let template_vars_json: Option<String> = r.try_get("template_vars_json")?;
    let blocks_json: Option<String> = r.try_get("blocks_json")?;
    let attachment_refs_json: Option<String> = r.try_get("attachment_refs_json")?;
    Ok(ComposeDraft {
        id: r.try_get("id")?,
        title: r.try_get("title")?,
        to_email: r.try_get("to_email")?,
        to_name: r.try_get("to_name")?,
        subject: r.try_get("subject")?,
        body: r.try_get("body")?,
        seller_id: r.try_get("seller_id")?,
        with_tracking: with_tracking != 0,
        template_id: r.try_get("template_id")?,
        template_vars: match template_vars_json {
            Some(s) => Some(serde_json::from_str(&s).map_err(|e| {
                ComposeDraftError::InvalidVarsJson(e.to_string())
            })?),
            None => None,
        },
        blocks: match blocks_json {
            Some(s) => Some(serde_json::from_str(&s).map_err(|e| {
                ComposeDraftError::InvalidBlocksJson(e.to_string())
            })?),
            None => None,
        },
        attachment_refs: match attachment_refs_json {
            Some(s) => serde_json::from_str(&s).map_err(|e| {
                ComposeDraftError::InvalidAttachmentJson(e.to_string())
            })?,
            None => Vec::new(),
        },
        mode: r.try_get("mode")?,
        created_at_ms: r.try_get("created_at_ms")?,
        updated_at_ms: r.try_get("updated_at_ms")?,
    })
}

/// Title displayed in the drafts list. Operator-supplied
/// wins; otherwise fall back to subject; otherwise the
/// store's configured empty-title sentinel.
fn effective_title(input: &ComposeDraftInput, empty_title: &str) -> String {
    if let Some(t) = input.title.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return t.to_string();
    }
    if let Some(s) = input.subject.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return s.to_string();
    }
    empty_title.to_string()
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
        migrate(&p, "test_drafts").await.unwrap();
        p
    }

    fn store(p: SqlitePool) -> ComposeDraftStore {
        ComposeDraftStore::new(p, "test_drafts").unwrap()
    }

    #[tokio::test]
    async fn create_then_list_round_trips() {
        let s = store(pool().await);
        let input = ComposeDraftInput {
            to_email: Some("camila@empresa.com".into()),
            subject: Some("Cotización Q1".into()),
            body: Some("Hola Camila…".into()),
            ..Default::default()
        };
        let d = s.create("t1", &input, 1_000).await.unwrap();
        assert!(d.id.starts_with("draft-"));
        assert_eq!(d.subject, "Cotización Q1");
        assert_eq!(d.title, "Cotización Q1");
        let list = s.list("t1").await.unwrap();
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn title_falls_back_to_subject_or_sentinel() {
        let s = store(pool().await);
        let d = s
            .create(
                "t1",
                &ComposeDraftInput {
                    body: Some("…".into()),
                    ..Default::default()
                },
                1,
            )
            .await
            .unwrap();
        assert_eq!(d.title, DEFAULT_EMPTY_TITLE);
    }

    #[tokio::test]
    async fn fallback_string_is_configurable() {
        let p = pool().await;
        let s = ComposeDraftStore::new_with_fallback(p, "test_drafts", "(sin asunto)")
            .unwrap();
        let d = s
            .create("t1", &ComposeDraftInput::default(), 1)
            .await
            .unwrap();
        assert_eq!(d.title, "(sin asunto)");
    }

    #[tokio::test]
    async fn update_patches_only_supplied_fields() {
        let s = store(pool().await);
        let d = s
            .create(
                "t1",
                &ComposeDraftInput {
                    to_email: Some("a@x".into()),
                    subject: Some("S".into()),
                    body: Some("B".into()),
                    ..Default::default()
                },
                1_000,
            )
            .await
            .unwrap();
        // Patch only subject + body — to_email survives.
        let patched = s
            .update(
                "t1",
                &d.id,
                &ComposeDraftInput {
                    subject: Some("S2".into()),
                    body: Some("B2".into()),
                    ..Default::default()
                },
                2_000,
            )
            .await
            .unwrap();
        assert_eq!(patched.subject, "S2");
        assert_eq!(patched.body, "B2");
        assert_eq!(patched.to_email, "a@x");
        assert_eq!(patched.updated_at_ms, 2_000);
    }

    #[tokio::test]
    async fn blocks_and_attachments_round_trip() {
        let s = store(pool().await);
        let blocks = serde_json::json!([{"kind": "heading", "text": "Hi"}]);
        let attachments = vec!["sha-aaa".to_string(), "sha-bbb".to_string()];
        let d = s
            .create(
                "t1",
                &ComposeDraftInput {
                    blocks: Some(blocks.clone()),
                    attachment_refs: Some(attachments.clone()),
                    mode: Some("builder".into()),
                    ..Default::default()
                },
                1,
            )
            .await
            .unwrap();
        let got = s.get("t1", &d.id).await.unwrap().unwrap();
        assert_eq!(got.blocks, Some(blocks));
        assert_eq!(got.attachment_refs, attachments);
        assert_eq!(got.mode, "builder");
    }

    #[tokio::test]
    async fn delete_returns_true_and_idempotent() {
        let s = store(pool().await);
        let d = s
            .create("t1", &ComposeDraftInput::default(), 1)
            .await
            .unwrap();
        assert!(s.delete("t1", &d.id).await.unwrap());
        assert!(!s.delete("t1", &d.id).await.unwrap());
    }

    #[tokio::test]
    async fn tenant_isolation() {
        let s = store(pool().await);
        let d = s
            .create("t1", &ComposeDraftInput::default(), 1)
            .await
            .unwrap();
        assert!(s.get("t2", &d.id).await.unwrap().is_none());
        assert_eq!(s.list("t2").await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_newest_first() {
        let s = store(pool().await);
        let a = s
            .create(
                "t1",
                &ComposeDraftInput {
                    subject: Some("A".into()),
                    ..Default::default()
                },
                100,
            )
            .await
            .unwrap();
        let _b = s
            .create(
                "t1",
                &ComposeDraftInput {
                    subject: Some("B".into()),
                    ..Default::default()
                },
                300,
            )
            .await
            .unwrap();
        // Update A so it sorts back to top.
        s.update(
            "t1",
            &a.id,
            &ComposeDraftInput {
                subject: Some("A2".into()),
                ..Default::default()
            },
            500,
        )
        .await
        .unwrap();
        let list = s.list("t1").await.unwrap();
        assert_eq!(list[0].subject, "A2");
        assert_eq!(list[1].subject, "B");
    }

    #[test]
    fn validate_table_rejects_injection_attempts() {
        assert!(validate_table("").is_err());
        assert!(validate_table("Drafts").is_err());
        assert!(validate_table("1d").is_err());
        assert!(validate_table("d; DROP --").is_err());
    }
}
