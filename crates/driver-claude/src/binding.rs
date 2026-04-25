//! Session-binding store. Keeps `(goal_id → claude session_id)` so the
//! next turn against the same goal can `--resume <id>`.
//!
//! Phase 67.1 ships `MemoryBindingStore`; Phase 67.2 adds
//! `SqliteBindingStore` (gated behind the `sqlite` feature) plus four
//! trait extensions with backward-compatible defaults.

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use nexo_driver_types::GoalId;
use serde::{Deserialize, Serialize};

use crate::error::ClaudeError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBinding {
    pub goal_id: GoalId,
    pub session_id: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Bumped on every observed event (`touch`) — separate from the
    /// structural `updated_at` so an idle-TTL filter does not reset on
    /// a no-op upsert.
    ///
    /// `#[serde(default)]` so payloads serialised by 67.1 (which lacked
    /// this field) deserialise to epoch; the store implementations are
    /// responsible for normalising it on the next `upsert`.
    #[serde(default)]
    pub last_active_at: DateTime<Utc>,
}

impl SessionBinding {
    /// Build a fresh binding with `created_at`, `updated_at`, and
    /// `last_active_at` set to "now".
    pub fn new(
        goal_id: GoalId,
        session_id: impl Into<String>,
        model: Option<String>,
        workspace: Option<PathBuf>,
    ) -> Self {
        let now = Utc::now();
        Self {
            goal_id,
            session_id: session_id.into(),
            model,
            workspace,
            created_at: now,
            updated_at: now,
            last_active_at: now,
        }
    }
}

#[async_trait]
pub trait SessionBindingStore: Send + Sync + 'static {
    // 67.1 surface — required.
    async fn get(&self, goal_id: GoalId) -> Result<Option<SessionBinding>, ClaudeError>;
    async fn upsert(&self, binding: SessionBinding) -> Result<(), ClaudeError>;
    async fn clear(&self, goal_id: GoalId) -> Result<(), ClaudeError>;

    // 67.2 — defaults preserve `MemoryBindingStore` behaviour.

    /// Mark the binding for `goal_id` as invalidated (e.g. Claude
    /// rejected the session id mid-turn). Default delegates to
    /// [`clear`]; the SQLite impl flips a flag and keeps the row for
    /// forensics.
    async fn mark_invalid(&self, goal_id: GoalId) -> Result<(), ClaudeError> {
        self.clear(goal_id).await
    }

    /// Refresh `last_active_at` without rewriting structural fields.
    /// Default no-op (the in-memory impl already bumps timestamps via
    /// upsert in practice).
    async fn touch(&self, _goal_id: GoalId) -> Result<(), ClaudeError> {
        Ok(())
    }

    /// Delete every binding whose `last_active_at` is strictly older
    /// than `cutoff`. Returns the number of rows removed. Default
    /// no-op — only persistent stores bother.
    async fn purge_older_than(&self, _cutoff: DateTime<Utc>) -> Result<u64, ClaudeError> {
        Ok(0)
    }

    /// Snapshot of bindings considered active (not invalidated and
    /// inside any configured TTL window). Default returns empty.
    async fn list_active(&self) -> Result<Vec<SessionBinding>, ClaudeError> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
pub struct MemoryBindingStore {
    inner: DashMap<GoalId, SessionBinding>,
}

impl MemoryBindingStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionBindingStore for MemoryBindingStore {
    async fn get(&self, goal_id: GoalId) -> Result<Option<SessionBinding>, ClaudeError> {
        Ok(self.inner.get(&goal_id).map(|b| b.value().clone()))
    }

    async fn upsert(&self, mut binding: SessionBinding) -> Result<(), ClaudeError> {
        let now = Utc::now();
        // Preserve `created_at` from the prior row if any; bump
        // `updated_at` and `last_active_at` unconditionally.
        if let Some(prior) = self.inner.get(&binding.goal_id) {
            binding.created_at = prior.value().created_at;
        }
        binding.updated_at = now;
        binding.last_active_at = now;
        self.inner.insert(binding.goal_id, binding);
        Ok(())
    }

    async fn clear(&self, goal_id: GoalId) -> Result<(), ClaudeError> {
        self.inner.remove(&goal_id);
        Ok(())
    }
}
