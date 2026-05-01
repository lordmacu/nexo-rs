//! Pluggable capture/restore of agent runtime state outside the memdir
//! and the SQLite memory tables.
//!
//! Two artifacts live here:
//!
//! - **extract cursor** — the post-turn extractor's progress marker
//!   (`crates/driver-loop/src/extract_memories.rs`). Today this is
//!   in-process state; a `Some(value)` capture is preserved when the
//!   runtime grows a persistent cursor.
//! - **dream_run row** — the most recent row from
//!   `crates/agent-registry/src/dream_run.rs::dream_runs` for the
//!   agent. Carries `priorMtime`, fork label, files touched.
//!
//! The crate stays decoupled from `nexo-driver-loop` and
//! `nexo-agent-registry` by going through this trait. Boot wiring
//! supplies the real implementation; tests use [`NoopStateProvider`].

use async_trait::async_trait;
use serde_json::Value;

use crate::error::SnapshotError;
use crate::id::AgentId;

#[async_trait]
pub trait StateProvider: Send + Sync + 'static {
    /// Snapshot-time read of the post-turn extractor cursor for `agent_id`.
    /// `Ok(None)` is fine: it just means no cursor exists yet and the
    /// bundle ships nothing for this artifact.
    async fn capture_extract_cursor(
        &self,
        agent_id: &AgentId,
    ) -> Result<Option<Value>, SnapshotError>;

    /// Snapshot-time read of the agent's most recent dream run row.
    async fn capture_last_dream_run(
        &self,
        agent_id: &AgentId,
    ) -> Result<Option<Value>, SnapshotError>;

    /// Restore-time write back to the extractor cursor sink.
    async fn restore_extract_cursor(
        &self,
        agent_id: &AgentId,
        value: Value,
    ) -> Result<(), SnapshotError>;

    /// Restore-time write back to the dream-run audit row.
    async fn restore_dream_run(
        &self,
        agent_id: &AgentId,
        value: Value,
    ) -> Result<(), SnapshotError>;
}

/// Sentinel implementation: returns `None` on capture, `Ok(())` on
/// restore. Boot wire replaces it with the real provider; tests and
/// builders default to this so a half-wired runtime never deadlocks
/// on a missing dependency.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopStateProvider;

#[async_trait]
impl StateProvider for NoopStateProvider {
    async fn capture_extract_cursor(
        &self,
        _agent_id: &AgentId,
    ) -> Result<Option<Value>, SnapshotError> {
        Ok(None)
    }

    async fn capture_last_dream_run(
        &self,
        _agent_id: &AgentId,
    ) -> Result<Option<Value>, SnapshotError> {
        Ok(None)
    }

    async fn restore_extract_cursor(
        &self,
        _agent_id: &AgentId,
        _value: Value,
    ) -> Result<(), SnapshotError> {
        Ok(())
    }

    async fn restore_dream_run(
        &self,
        _agent_id: &AgentId,
        _value: Value,
    ) -> Result<(), SnapshotError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn noop_provider_returns_none_on_capture() {
        let p = NoopStateProvider;
        let id = "ana".to_string();
        assert!(p.capture_extract_cursor(&id).await.unwrap().is_none());
        assert!(p.capture_last_dream_run(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn noop_provider_accepts_restore_payloads() {
        let p = NoopStateProvider;
        let id = "ana".to_string();
        p.restore_extract_cursor(&id, serde_json::json!({"k": 1}))
            .await
            .unwrap();
        p.restore_dream_run(&id, serde_json::json!({"k": 2}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn dyn_provider_can_be_held_as_arc() {
        let p: Arc<dyn StateProvider> = Arc::new(NoopStateProvider);
        let id = "ana".to_string();
        assert!(p.capture_extract_cursor(&id).await.unwrap().is_none());
    }
}
