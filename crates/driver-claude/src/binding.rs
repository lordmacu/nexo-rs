//! Session-binding store. Keeps `(goal_id → claude session_id)` so the
//! next turn against the same goal can `--resume <id>`.
//!
//! Phase 67.1 ships only `MemoryBindingStore`. The SQLite-backed
//! impl lands in 67.2 and slots in behind the same trait.

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
}

impl SessionBinding {
    /// Build a fresh binding with `created_at` and `updated_at` set
    /// to "now".
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
        }
    }
}

#[async_trait]
pub trait SessionBindingStore: Send + Sync + 'static {
    async fn get(&self, goal_id: GoalId) -> Result<Option<SessionBinding>, ClaudeError>;
    async fn upsert(&self, binding: SessionBinding) -> Result<(), ClaudeError>;
    async fn clear(&self, goal_id: GoalId) -> Result<(), ClaudeError>;
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
        // Preserve `created_at` from the prior row if any; bump
        // `updated_at` unconditionally.
        if let Some(prior) = self.inner.get(&binding.goal_id) {
            binding.created_at = prior.value().created_at;
        }
        binding.updated_at = Utc::now();
        self.inner.insert(binding.goal_id, binding);
        Ok(())
    }

    async fn clear(&self, goal_id: GoalId) -> Result<(), ClaudeError> {
        self.inner.remove(&goal_id);
        Ok(())
    }
}
