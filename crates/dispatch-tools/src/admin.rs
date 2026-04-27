//! Phase 67.G.4 — operator-only admin tools.
//!
//! These don't pass through `DispatchGate` because they aren't
//! per-binding actions; instead they're gated by the
//! `is_admin` flag on the agent's policy snapshot. The runtime
//! ToolRegistry filter (67.D.3) drops them from non-admin
//! sessions so the LLM never even sees them. The server-side
//! function-level `is_admin: bool` argument is a defense in
//! depth — it should never be reachable for non-admin callers.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use nexo_agent_registry::AgentRegistry;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdminError {
    #[error("forbidden: caller is not admin")]
    Forbidden,
    #[error("registry: {0}")]
    Registry(String),
}

#[derive(Clone, Debug, Deserialize)]
pub struct SetConcurrencyCapInput {
    pub max_concurrent_agents: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetConcurrencyCapOutput {
    pub previous: u32,
    pub current: u32,
}

pub async fn set_concurrency_cap(
    input: SetConcurrencyCapInput,
    registry: Arc<AgentRegistry>,
    is_admin: bool,
) -> Result<SetConcurrencyCapOutput, AdminError> {
    if !is_admin {
        return Err(AdminError::Forbidden);
    }
    let prev = registry.cap();
    let curr = registry.set_cap(input.max_concurrent_agents);
    Ok(SetConcurrencyCapOutput {
        previous: prev,
        current: curr,
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct FlushAgentQueueOutput {
    pub drained: u64,
}

pub async fn flush_agent_queue(
    registry: Arc<AgentRegistry>,
    is_admin: bool,
) -> Result<FlushAgentQueueOutput, AdminError> {
    if !is_admin {
        return Err(AdminError::Forbidden);
    }
    let n = registry
        .flush_queue()
        .await
        .map_err(|e| AdminError::Registry(e.to_string()))?;
    Ok(FlushAgentQueueOutput { drained: n })
}

#[derive(Clone, Debug, Deserialize)]
pub struct EvictCompletedInput {
    /// Drop terminal goals whose `finished_at` is older than this
    /// many seconds. Default 24h.
    #[serde(default = "default_older_than_secs")]
    pub older_than_secs: u64,
}

fn default_older_than_secs() -> u64 {
    60 * 60 * 24
}

#[derive(Clone, Debug, Serialize)]
pub struct EvictCompletedOutput {
    pub evicted: u64,
}

pub async fn evict_completed(
    input: EvictCompletedInput,
    registry: Arc<AgentRegistry>,
    is_admin: bool,
) -> Result<EvictCompletedOutput, AdminError> {
    if !is_admin {
        return Err(AdminError::Forbidden);
    }
    let cutoff = Utc::now()
        - chrono::Duration::from_std(Duration::from_secs(input.older_than_secs))
            .unwrap_or_else(|_| chrono::Duration::days(7));
    let n = registry
        .evict_terminal_older_than(cutoff)
        .await
        .map_err(|e| AdminError::Registry(e.to_string()))?;
    Ok(EvictCompletedOutput { evicted: n })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use nexo_agent_registry::{
        AgentHandle, AgentRunStatus, AgentSnapshot, MemoryAgentRegistryStore,
    };
    use nexo_driver_types::GoalId;
    use uuid::Uuid;

    fn handle(status: AgentRunStatus, finished: Option<chrono::DateTime<Utc>>) -> AgentHandle {
        AgentHandle {
            goal_id: GoalId(Uuid::new_v4()),
            phase_id: "x".into(),
            status,
            origin: None,
            dispatcher: None,
            started_at: Utc::now(),
            finished_at: finished,
            snapshot: AgentSnapshot::default(),
        plan_mode: None,
        }
    }

    #[tokio::test]
    async fn set_concurrency_cap_forbidden_when_not_admin() {
        let reg = Arc::new(AgentRegistry::new(
            Arc::new(MemoryAgentRegistryStore::default()),
            4,
        ));
        let err = set_concurrency_cap(
            SetConcurrencyCapInput {
                max_concurrent_agents: 8,
            },
            reg,
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AdminError::Forbidden));
    }

    #[tokio::test]
    async fn set_concurrency_cap_admin_updates_value() {
        let reg = Arc::new(AgentRegistry::new(
            Arc::new(MemoryAgentRegistryStore::default()),
            4,
        ));
        let out = set_concurrency_cap(
            SetConcurrencyCapInput {
                max_concurrent_agents: 8,
            },
            reg.clone(),
            true,
        )
        .await
        .unwrap();
        assert_eq!(out.previous, 4);
        assert_eq!(out.current, 8);
        assert_eq!(reg.cap(), 8);
    }

    #[tokio::test]
    async fn flush_agent_queue_drains_queued_marks_cancelled() {
        let reg = Arc::new(AgentRegistry::new(
            Arc::new(MemoryAgentRegistryStore::default()),
            1,
        ));
        // First admit fills the cap; second goes to the queue.
        reg.admit(handle(AgentRunStatus::Running, None), true)
            .await
            .unwrap();
        reg.admit(handle(AgentRunStatus::Running, None), true)
            .await
            .unwrap();
        let out = flush_agent_queue(reg.clone(), true).await.unwrap();
        assert_eq!(out.drained, 1);
    }

    #[tokio::test]
    async fn evict_completed_drops_old_terminal_rows() {
        let reg = Arc::new(AgentRegistry::new(
            Arc::new(MemoryAgentRegistryStore::default()),
            4,
        ));
        let recent = handle(AgentRunStatus::Done, Some(Utc::now()));
        let old = handle(
            AgentRunStatus::Done,
            Some(Utc::now() - ChronoDuration::days(30)),
        );
        let recent_id = recent.goal_id;
        let old_id = old.goal_id;
        // admit() preserves finished_at from the handle into the
        // entry meta, so a 30-day-old finished_at survives. We call
        // set_status(Done) afterwards to flip status without
        // overwriting finished_at (set_status only fills when None).
        reg.admit(recent, true).await.unwrap();
        reg.set_status(recent_id, AgentRunStatus::Done)
            .await
            .unwrap();
        reg.admit(old, true).await.unwrap();
        reg.set_status(old_id, AgentRunStatus::Done).await.unwrap();

        let out = evict_completed(
            EvictCompletedInput {
                older_than_secs: 60 * 60 * 24, // 1d
            },
            reg.clone(),
            true,
        )
        .await
        .unwrap();
        assert!(out.evicted >= 1);
        // Recent row still present.
        assert!(reg.handle(recent_id).is_some());
    }

    #[tokio::test]
    async fn evict_forbidden_when_not_admin() {
        let reg = Arc::new(AgentRegistry::new(
            Arc::new(MemoryAgentRegistryStore::default()),
            4,
        ));
        let err = evict_completed(EvictCompletedInput { older_than_secs: 1 }, reg, false)
            .await
            .unwrap_err();
        assert!(matches!(err, AdminError::Forbidden));
    }
}
