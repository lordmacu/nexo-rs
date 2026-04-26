//! Phase 67.G.3 — in-memory map keyed by `GoalId` of every hook
//! attached to that goal. Persistence + restart safety lives in the
//! idempotency store (67.F.3); this map is the live "who's
//! listening on this goal" surface that `add_hook` / `remove_hook`
//! / `agent_hooks_list` consume.
//!
//! Kept inside the hooks module rather than in `agent-registry` so
//! the registry crate stays focused on goal lifecycle and doesn't
//! grow a hook-shaped surface that's only consumed here.

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use nexo_driver_types::GoalId;
use parking_lot::Mutex;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use thiserror::Error;

use super::types::CompletionHook;

#[derive(Debug, Error)]
pub enum HookStoreError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("json: {0}")]
    Json(String),
}

/// Persistence layer the registry mirrors writes through. The
/// SQLite impl uses one row per (goal_id, hook_id). Reloading
/// reads all rows and rehydrates the in-memory map — used by
/// `main.rs` after a daemon restart so attached hooks survive.
#[async_trait]
pub trait HookRegistryStore: Send + Sync + 'static {
    async fn upsert(&self, goal_id: GoalId, hook: &CompletionHook) -> Result<(), HookStoreError>;
    async fn remove(&self, goal_id: GoalId, hook_id: &str) -> Result<(), HookStoreError>;
    async fn drop_goal(&self, goal_id: GoalId) -> Result<(), HookStoreError>;
    async fn load_all(&self) -> Result<Vec<(GoalId, CompletionHook)>, HookStoreError>;
}

/// SQLite-backed store. Schema:
///
/// ```sql
/// CREATE TABLE hook_registry (
///   goal_id  TEXT NOT NULL,
///   hook_id  TEXT NOT NULL,
///   payload  TEXT NOT NULL,
///   PRIMARY KEY (goal_id, hook_id)
/// );
/// ```
pub struct SqliteHookRegistryStore {
    pool: SqlitePool,
}

impl SqliteHookRegistryStore {
    pub async fn open(path: &str) -> Result<Self, HookStoreError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(if path == ":memory:" { 1 } else { 4 })
            .connect_with(opts)
            .await?;
        if path != ":memory:" {
            sqlx::query("PRAGMA journal_mode = WAL")
                .execute(&pool)
                .await?;
            sqlx::query("PRAGMA synchronous = NORMAL")
                .execute(&pool)
                .await?;
        }
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS hook_registry (\
                goal_id TEXT NOT NULL,\
                hook_id TEXT NOT NULL,\
                payload TEXT NOT NULL,\
                PRIMARY KEY (goal_id, hook_id)\
             )",
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }

    pub async fn open_memory() -> Result<Self, HookStoreError> {
        Self::open(":memory:").await
    }
}

#[async_trait]
impl HookRegistryStore for SqliteHookRegistryStore {
    async fn upsert(&self, goal_id: GoalId, hook: &CompletionHook) -> Result<(), HookStoreError> {
        let payload = serde_json::to_string(hook).map_err(|e| HookStoreError::Json(e.to_string()))?;
        sqlx::query(
            "INSERT INTO hook_registry (goal_id, hook_id, payload) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(goal_id, hook_id) DO UPDATE SET payload = excluded.payload",
        )
        .bind(goal_id.0.to_string())
        .bind(&hook.id)
        .bind(payload)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    async fn remove(&self, goal_id: GoalId, hook_id: &str) -> Result<(), HookStoreError> {
        sqlx::query("DELETE FROM hook_registry WHERE goal_id = ?1 AND hook_id = ?2")
            .bind(goal_id.0.to_string())
            .bind(hook_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    async fn drop_goal(&self, goal_id: GoalId) -> Result<(), HookStoreError> {
        sqlx::query("DELETE FROM hook_registry WHERE goal_id = ?1")
            .bind(goal_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    async fn load_all(&self) -> Result<Vec<(GoalId, CompletionHook)>, HookStoreError> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT goal_id, payload FROM hook_registry",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (gid, payload) in rows {
            let goal = uuid::Uuid::parse_str(&gid)
                .map_err(|e| HookStoreError::Json(format!("uuid: {e}")))?;
            let hook: CompletionHook = serde_json::from_str(&payload)
                .map_err(|e| HookStoreError::Json(e.to_string()))?;
            out.push((GoalId(goal), hook));
        }
        Ok(out)
    }
}

#[derive(Clone, Default)]
pub struct HookRegistry {
    inner: Arc<DashMap<GoalId, Mutex<Vec<CompletionHook>>>>,
    /// Optional persistent mirror. `None` keeps in-memory-only
    /// behaviour (back-compat with tests + dev). When set, every
    /// add/remove/drop is written through; reload on boot rehydrates
    /// the inner map.
    store: Option<Arc<dyn HookRegistryStore>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry that mirrors writes to a persistent store.
    pub fn with_store(store: Arc<dyn HookRegistryStore>) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            store: Some(store),
        }
    }

    /// Reload every persisted hook into the in-memory map. Call
    /// once at boot, after the agent-registry reattach completes,
    /// so attached hooks survive daemon restart.
    pub async fn reload_from_store(&self) -> Result<usize, HookStoreError> {
        let Some(store) = &self.store else {
            return Ok(0);
        };
        let rows = store.load_all().await?;
        let n = rows.len();
        for (goal_id, hook) in rows {
            let entry = self.inner.entry(goal_id).or_default();
            let mut v = entry.value().lock();
            if !v.iter().any(|h| h.id == hook.id) {
                v.push(hook);
            }
        }
        Ok(n)
    }

    /// Append a hook. Returns the position the new hook was placed at.
    pub fn add(&self, goal_id: GoalId, hook: CompletionHook) -> usize {
        let store = self.store.clone();
        let hook_for_store = hook.clone();
        let entry = self.inner.entry(goal_id).or_default();
        let mut v = entry.value().lock();
        v.push(hook);
        let pos = v.len() - 1;
        drop(v);
        if let Some(store) = store {
            tokio::spawn(async move {
                if let Err(e) = store.upsert(goal_id, &hook_for_store).await {
                    tracing::warn!(target: "hook.registry", "store upsert failed: {e}");
                }
            });
        }
        pos
    }

    /// B19 — append a hook only when no hook with the same `id` is
    /// already attached. Idempotent: a replay of the auto-audit
    /// hook attachment finds the slot taken and skips. Returns
    /// `Some(position)` when the hook was inserted, `None` when
    /// a duplicate id was rejected.
    pub fn add_unique(&self, goal_id: GoalId, hook: CompletionHook) -> Option<usize> {
        let store = self.store.clone();
        let hook_for_store = hook.clone();
        let entry = self.inner.entry(goal_id).or_default();
        let mut v = entry.value().lock();
        if v.iter().any(|h| h.id == hook.id) {
            return None;
        }
        v.push(hook);
        let pos = v.len() - 1;
        drop(v);
        if let Some(store) = store {
            tokio::spawn(async move {
                if let Err(e) = store.upsert(goal_id, &hook_for_store).await {
                    tracing::warn!(target: "hook.registry", "store upsert failed: {e}");
                }
            });
        }
        Some(pos)
    }

    /// Diagnostic accessor — number of hooks attached to `goal_id`.
    pub fn count(&self, goal_id: GoalId) -> usize {
        self.inner
            .get(&goal_id)
            .map(|e| e.value().lock().len())
            .unwrap_or(0)
    }

    /// Remove the hook with the given `id`. Returns `true` when a
    /// hook was actually removed.
    pub fn remove(&self, goal_id: GoalId, hook_id: &str) -> bool {
        let removed = {
            let Some(entry) = self.inner.get(&goal_id) else {
                return false;
            };
            let mut v = entry.value().lock();
            let before = v.len();
            v.retain(|h| h.id != hook_id);
            before != v.len()
        };
        if removed {
            if let Some(store) = self.store.clone() {
                let id = hook_id.to_string();
                tokio::spawn(async move {
                    if let Err(e) = store.remove(goal_id, &id).await {
                        tracing::warn!(target: "hook.registry", "store remove failed: {e}");
                    }
                });
            }
        }
        removed
    }

    /// Snapshot of every hook attached to `goal_id` in declaration
    /// order.
    pub fn list(&self, goal_id: GoalId) -> Vec<CompletionHook> {
        self.inner
            .get(&goal_id)
            .map(|e| e.value().lock().clone())
            .unwrap_or_default()
    }

    /// Drop every hook tied to a goal. Called when the goal reaches
    /// a terminal state and the orchestrator evicts.
    pub fn drop_goal(&self, goal_id: GoalId) {
        self.inner.remove(&goal_id);
        if let Some(store) = self.store.clone() {
            tokio::spawn(async move {
                if let Err(e) = store.drop_goal(goal_id).await {
                    tracing::warn!(target: "hook.registry", "store drop_goal failed: {e}");
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::types::{HookAction, HookTrigger};
    use uuid::Uuid;

    fn hk(id: &str) -> CompletionHook {
        CompletionHook {
            id: id.into(),
            on: HookTrigger::Done,
            action: HookAction::NotifyOrigin,
        }
    }

    #[test]
    fn add_list_remove_round_trip() {
        let reg = HookRegistry::new();
        let g = GoalId(Uuid::new_v4());
        reg.add(g, hk("a"));
        reg.add(g, hk("b"));
        assert_eq!(reg.list(g).len(), 2);
        assert!(reg.remove(g, "a"));
        let l = reg.list(g);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].id, "b");
        assert!(!reg.remove(g, "missing"));
    }

    #[test]
    fn drop_goal_clears_entries() {
        let reg = HookRegistry::new();
        let g = GoalId(Uuid::new_v4());
        reg.add(g, hk("a"));
        reg.drop_goal(g);
        assert!(reg.list(g).is_empty());
    }

    #[tokio::test]
    async fn add_unique_rejects_duplicate_id() {
        let reg = HookRegistry::new();
        let g = GoalId(Uuid::new_v4());
        assert!(reg.add_unique(g, hk("dup")).is_some());
        assert!(reg.add_unique(g, hk("dup")).is_none());
        assert_eq!(reg.list(g).len(), 1);
    }

    #[tokio::test]
    async fn persistence_round_trip_through_sqlite() {
        // B17 — hooks attached against a registry with a store
        // survive a fresh registry built against the same store.
        let store = Arc::new(SqliteHookRegistryStore::open_memory().await.unwrap())
            as Arc<dyn HookRegistryStore>;
        let g = GoalId(Uuid::new_v4());
        let reg1 = HookRegistry::with_store(store.clone());
        reg1.add(g, hk("h1"));
        // Spawned writes are async — give the store a beat.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let reg2 = HookRegistry::with_store(store.clone());
        let n = reg2.reload_from_store().await.unwrap();
        assert_eq!(n, 1);
        let l = reg2.list(g);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].id, "h1");
    }
}
