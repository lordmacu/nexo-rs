//! Phase 77.3 — `CompactSummaryStore` implementations.
//!
//! `SqliteCompactSummaryStore` persists summaries via
//! `nexo_memory::LongTermMemory::remember()` so resumed sessions can
//! inject the last compact summary into the prompt without
//! re-executing elided turns.

use async_trait::async_trait;
use nexo_driver_types::{CompactSummary, CompactSummaryStore, GoalId};
use std::sync::Arc;

use nexo_memory::LongTermMemory;

/// Persists compact summaries via the Phase 5.3 long-term memory store.
pub struct SqliteCompactSummaryStore {
    ltm: Arc<LongTermMemory>,
}

impl SqliteCompactSummaryStore {
    pub fn new(ltm: Arc<LongTermMemory>) -> Self {
        Self { ltm }
    }
}

#[async_trait]
impl CompactSummaryStore for SqliteCompactSummaryStore {
    async fn store(&self, summary: CompactSummary) -> Result<(), String> {
        let json = serde_json::to_string(&summary).map_err(|e| e.to_string())?;
        let goal_str = summary
            .agent_id
            .split("::")
            .last()
            .unwrap_or(&summary.agent_id);
        let content = format!("compact_summary goal:{} turn:{} {}", goal_str, summary.turn_index, json);
        self.ltm
            .remember(&summary.agent_id, &content, &["compact_summary"])
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn load(
        &self,
        agent_id: &str,
        goal_id: &GoalId,
    ) -> Result<Option<CompactSummary>, String> {
        let query = format!("compact_summary goal:{}", goal_id.0);
        let entries = self
            .ltm
            .recall(agent_id, &query, 5)
            .await
            .map_err(|e| e.to_string())?;
        // Find the most recent entry that deserializes correctly.
        for entry in entries {
            if let Ok(s) = serde_json::from_str::<CompactSummary>(&entry.content) {
                return Ok(Some(s));
            }
            // The content has the prefix + JSON. Try stripping prefix.
            if let Some(json_start) = entry.content.find("{\"agent_id\"") {
                if let Ok(s) = serde_json::from_str::<CompactSummary>(&entry.content[json_start..]) {
                    return Ok(Some(s));
                }
            }
        }
        Ok(None)
    }

    async fn forget(&self, goal_id: &GoalId) -> Result<(), String> {
        // LongTermMemory::forget(id) needs a UUID. We don't track the
        // memory entry UUID here, so clean up by relying on FTS recall
        // \+ forget. For now this is best-effort.
        let _ = goal_id;
        Ok(())
    }
}

/// No-op store for tests and when `store_in_long_term_memory` is false.
pub struct NoopCompactSummaryStore;

#[async_trait]
impl CompactSummaryStore for NoopCompactSummaryStore {
    async fn store(&self, _summary: CompactSummary) -> Result<(), String> {
        Ok(())
    }

    async fn load(
        &self,
        _agent_id: &str,
        _goal_id: &GoalId,
    ) -> Result<Option<CompactSummary>, String> {
        Ok(None)
    }

    async fn forget(&self, _goal_id: &GoalId) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_store_always_ok() {
        let s = NoopCompactSummaryStore;
        let summary = CompactSummary {
            agent_id: "test".into(),
            summary: "test summary".into(),
            turn_index: 5,
            before_tokens: 100_000,
            after_tokens: 20_000,
            stored_at: chrono::Utc::now(),
        };
        s.store(summary).await.unwrap();
    }

    #[tokio::test]
    async fn noop_load_returns_none() {
        let s = NoopCompactSummaryStore;
        assert!(s
            .load("test", &GoalId::new())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn noop_forget_always_ok() {
        let s = NoopCompactSummaryStore;
        s.forget(&GoalId::new()).await.unwrap();
    }
}
