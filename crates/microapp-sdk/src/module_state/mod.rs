//! Per-tenant on/off toggle for a microapp module.
//!
//! Operator-facing kill switch so the operator can disable
//! every automated effect of a microapp while they tune
//! configuration, edit prompts, swap a connector, etc., without
//! losing inbound traffic during the edit window.
//!
//! ## Pause semantics
//!
//! "Disabled" means **don't fire automated effects**. The
//! microapp consumer decides which effects gate on this — the
//! pattern is to read the cached `enabled` flag at the entry
//! point of every automated handler / cron sweep / notification
//! publisher and short-circuit when `false`. Manual operator
//! actions (compose, approve, manual transition) keep working;
//! the toggle only halts AUTOMATED behaviour.
//!
//! What still happens during pause is up to the microapp:
//! typically inbound is still received + persisted (so the
//! operator's queue keeps populating and they have visibility
//! into what arrived during the pause).
//!
//! ## Surface
//!
//! - [`ModuleState`] — the wire shape: `tenant_id`, `enabled`,
//!   `paused_reason`, `updated_at_ms`.
//! - [`ModuleStateStore`] — SQLite-backed CRUD. Defaults a
//!   missing row to enabled so a fresh tenant doesn't have to
//!   click the switch first.
//! - [`ModuleStateCache`] — hot-path `RwLock<HashMap>` cache
//!   read on every gating decision. Admin writes invalidate.
//!
//! ## Table name
//!
//! The store takes a `table_name` at construction time so each
//! microapp owns its own table — keeps multiple microapps
//! sharing the same SQLite pool isolated, and lets a microapp
//! preserve data on lift (e.g. marketing keeps the existing
//! `marketing_state` table). Validated against
//! `^[a-z][a-z0-9_]{0,63}$` to prevent SQL injection through
//! the `format!()`-built statements.
//!
//! Lifted from the agent-creator-microapp's marketing
//! extension (`MarketingState`); nothing there was marketing-
//! specific. Any microapp that wants an operator pause switch
//! consumes this module via the `module-state` feature.

#![allow(missing_docs)]

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModuleState {
    pub tenant_id: String,
    pub enabled: bool,
    /// Free-form note the operator left when toggling off so
    /// teammates know why the module is paused. `None` /
    /// empty when enabled.
    pub paused_reason: Option<String>,
    pub updated_at_ms: i64,
}

impl ModuleState {
    /// Default state for a tenant we haven't persisted yet —
    /// a fresh row is implicitly enabled so the operator
    /// doesn't have to click the switch on first install.
    pub fn enabled_for(tenant_id: &str, now_ms: i64) -> Self {
        Self {
            tenant_id: tenant_id.to_string(),
            enabled: true,
            paused_reason: None,
            updated_at_ms: now_ms,
        }
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("invalid table name: {0:?} (must match [a-z][a-z0-9_]{{0,63}})")]
    InvalidTableName(String),
}

/// Validate a table name against the allowlist regex —
/// `format!()`-built SQL needs the table identifier guarded
/// because `sqlx::query` only parameterises values, not
/// identifiers.
fn validate_table(name: &str) -> Result<(), StateError> {
    if name.is_empty() || name.len() > 64 {
        return Err(StateError::InvalidTableName(name.to_string()));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return Err(StateError::InvalidTableName(name.to_string()));
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(StateError::InvalidTableName(name.to_string()));
        }
    }
    Ok(())
}

/// Run the CREATE TABLE migration. Idempotent — safe on every
/// boot. Caller passes the same `table_name` they'll use for
/// the store.
pub async fn migrate(pool: &SqlitePool, table_name: &str) -> Result<(), StateError> {
    validate_table(table_name)?;
    let stmt = format!(
        r#"
CREATE TABLE IF NOT EXISTS {table_name} (
    tenant_id     TEXT PRIMARY KEY,
    enabled       INTEGER NOT NULL DEFAULT 1,
    paused_reason TEXT,
    updated_at_ms INTEGER NOT NULL
);
"#
    );
    sqlx::query(&stmt).execute(pool).await?;
    Ok(())
}

#[derive(Clone)]
pub struct ModuleStateStore {
    pool: SqlitePool,
    table: String,
}

impl ModuleStateStore {
    /// Construct a store backed by `pool` writing to `table_name`.
    /// Returns `Err(InvalidTableName)` when the name doesn't
    /// match the allowlist regex.
    pub fn new(pool: SqlitePool, table_name: impl Into<String>) -> Result<Self, StateError> {
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

    pub async fn get(&self, tenant_id: &str, now_ms: i64) -> Result<ModuleState, StateError> {
        let stmt = format!(
            r#"SELECT enabled, paused_reason, updated_at_ms
               FROM {} WHERE tenant_id = ?"#,
            self.table,
        );
        let row = sqlx::query(&stmt)
            .bind(tenant_id)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(ModuleState::enabled_for(tenant_id, now_ms));
        };
        Ok(ModuleState {
            tenant_id: tenant_id.to_string(),
            enabled: row.try_get::<i64, _>("enabled")? != 0,
            paused_reason: row.try_get("paused_reason")?,
            updated_at_ms: row.try_get("updated_at_ms")?,
        })
    }

    pub async fn put(&self, state: &ModuleState) -> Result<(), StateError> {
        let stmt = format!(
            r#"INSERT INTO {}
                 (tenant_id, enabled, paused_reason, updated_at_ms)
               VALUES (?, ?, ?, ?)
               ON CONFLICT(tenant_id) DO UPDATE SET
                  enabled       = excluded.enabled,
                  paused_reason = excluded.paused_reason,
                  updated_at_ms = excluded.updated_at_ms"#,
            self.table,
        );
        sqlx::query(&stmt)
            .bind(&state.tenant_id)
            .bind(state.enabled as i64)
            .bind(state.paused_reason.as_deref())
            .bind(state.updated_at_ms)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Hot-path cache: tenant_id → ModuleState. Read on every
/// gating decision (broker hop, draft generate handler, cron
/// sweep tool, notification publisher). Admin writes
/// invalidate so the next read reflects disk truth.
#[derive(Clone)]
pub struct ModuleStateCache {
    store: ModuleStateStore,
    inner: Arc<RwLock<HashMap<String, ModuleState>>>,
}

impl ModuleStateCache {
    pub fn new(store: ModuleStateStore) -> Self {
        Self {
            store,
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn store(&self) -> &ModuleStateStore {
        &self.store
    }

    /// True when the tenant is enabled (default for un-set
    /// rows). On store failure, defaults to `true` so a
    /// transient SQLite hiccup never silently disables a
    /// production tenant — fail-open is the right default for
    /// a kill switch (the operator can re-toggle if a real
    /// pause is intended).
    pub async fn is_enabled(&self, tenant_id: &str, now_ms: i64) -> bool {
        self.get(tenant_id, now_ms).await.enabled
    }

    pub async fn get(&self, tenant_id: &str, now_ms: i64) -> ModuleState {
        if let Some(hit) = self.inner.read().await.get(tenant_id).cloned() {
            return hit;
        }
        match self.store.get(tenant_id, now_ms).await {
            Ok(state) => {
                self.inner
                    .write()
                    .await
                    .insert(tenant_id.to_string(), state.clone());
                state
            }
            Err(e) => {
                tracing::warn!(
                    target: "sdk.module_state",
                    tenant_id,
                    error = %e,
                    "module_state load failed; defaulting to enabled",
                );
                ModuleState::enabled_for(tenant_id, now_ms)
            }
        }
    }

    pub async fn invalidate(&self, tenant_id: &str) {
        self.inner.write().await.remove(tenant_id);
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
        migrate(&p, "test_state").await.unwrap();
        p
    }

    #[tokio::test]
    async fn defaults_to_enabled_when_no_row() {
        let store = ModuleStateStore::new(pool().await, "test_state").unwrap();
        let s = store.get("acme", 1).await.unwrap();
        assert!(s.enabled);
        assert!(s.paused_reason.is_none());
    }

    #[tokio::test]
    async fn put_disabled_then_get_round_trips() {
        let store = ModuleStateStore::new(pool().await, "test_state").unwrap();
        let mut s = ModuleState::enabled_for("acme", 1);
        s.enabled = false;
        s.paused_reason = Some("tuning rules".into());
        s.updated_at_ms = 100;
        store.put(&s).await.unwrap();
        let back = store.get("acme", 999).await.unwrap();
        assert!(!back.enabled);
        assert_eq!(back.paused_reason.as_deref(), Some("tuning rules"));
        assert_eq!(back.updated_at_ms, 100);
    }

    #[tokio::test]
    async fn cache_invalidation_picks_up_new_value() {
        let store = ModuleStateStore::new(pool().await, "test_state").unwrap();
        let cache = ModuleStateCache::new(store.clone());

        // First read populates the cache with default-enabled.
        assert!(cache.is_enabled("acme", 1).await);

        // Backend write that bypasses the cache.
        let mut s = ModuleState::enabled_for("acme", 1);
        s.enabled = false;
        store.put(&s).await.unwrap();

        // Without invalidation: still cached as enabled.
        assert!(cache.is_enabled("acme", 2).await);

        // Invalidate → next read reflects disk truth.
        cache.invalidate("acme").await;
        assert!(!cache.is_enabled("acme", 3).await);
    }

    #[test]
    fn validate_table_accepts_safe_names() {
        for ok in &["module_state", "marketing_state", "x_y_z_1"] {
            assert!(validate_table(ok).is_ok(), "should accept: {ok}");
        }
    }

    #[test]
    fn validate_table_rejects_injection_attempts() {
        for bad in &[
            "",
            "Module_State",          // uppercase
            "1state",                // leading digit
            "module-state",          // hyphen
            "module state",          // space
            "module_state; DROP --", // SQL injection
            "'state'",               // quotes
            &"a".repeat(65),         // too long
        ] {
            assert!(validate_table(bad).is_err(), "should reject: {bad:?}");
        }
    }

    #[tokio::test]
    async fn store_constructor_rejects_bad_table_name() {
        // Build a dangling pool just to exercise the constructor
        // — we never call query() so the schema doesn't matter.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        assert!(ModuleStateStore::new(pool, "bad-name").is_err());
    }
}
