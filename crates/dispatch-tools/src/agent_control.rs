//! Phase 67.G.2 — agent control tools: cancel / pause / resume /
//! update_budget.
//!
//! Each function is a thin façade over `AgentRegistry` +
//! `DriverOrchestrator`:
//! - cancel_agent: orch.cancel_goal + registry.set_status(Cancelled).
//! - pause_agent: orch.pause_goal  + registry.set_status(Paused).
//! - resume_agent: orch.resume_goal + registry.set_status(Running).
//! - update_budget: registry.set_max_turns. Only-grow guard
//!   compares against the live snapshot's `usage.turns` so callers
//!   cannot shrink the budget below the work already done (would
//!   exhaust immediately).
//!
//! Shape mirrors `program_phase` so the agent loop can register all
//! of them uniformly: take a typed input, return a typed output the
//! tool wrapper serialises into Value.

use std::sync::Arc;

use nexo_agent_registry::{AgentRegistry, AgentRunStatus};
use nexo_driver_loop::DriverOrchestrator;
use nexo_driver_types::GoalId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentControlError {
    #[error("registry: {0}")]
    Registry(String),
}

#[derive(Clone, Debug, Deserialize)]
pub struct CancelAgentInput {
    pub goal_id: GoalId,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CancelAgentOutput {
    pub goal_id: GoalId,
    pub cancelled: bool,
    /// `None` when the goal_id was unknown.
    pub previous_status: Option<AgentRunStatus>,
}

pub async fn cancel_agent(
    input: CancelAgentInput,
    orchestrator: Arc<DriverOrchestrator>,
    registry: Arc<AgentRegistry>,
) -> Result<CancelAgentOutput, AgentControlError> {
    let prev = registry.handle(input.goal_id).map(|h| h.status);
    if prev.is_none() {
        return Ok(CancelAgentOutput {
            goal_id: input.goal_id,
            cancelled: false,
            previous_status: None,
        });
    }
    let signalled = orchestrator.cancel_goal(input.goal_id);
    // Persist Cancelled status. release() also pops the next-up
    // queued goal but only when the prev status was non-terminal;
    // we use set_status here because the run_goal loop will publish
    // its own GoalCompleted event and we don't want to race a queue
    // promotion before the loop has actually wound down.
    registry
        .set_status(input.goal_id, AgentRunStatus::Cancelled)
        .await
        .map_err(|e| AgentControlError::Registry(e.to_string()))?;
    Ok(CancelAgentOutput {
        goal_id: input.goal_id,
        cancelled: signalled || prev.is_some(),
        previous_status: prev,
    })
}

#[derive(Clone, Debug, Deserialize)]
pub struct PauseAgentInput {
    pub goal_id: GoalId,
}

#[derive(Clone, Debug, Serialize)]
pub struct PauseAgentOutput {
    pub goal_id: GoalId,
    pub paused: bool,
}

pub async fn pause_agent(
    input: PauseAgentInput,
    orchestrator: Arc<DriverOrchestrator>,
    registry: Arc<AgentRegistry>,
) -> Result<PauseAgentOutput, AgentControlError> {
    let signalled = orchestrator.pause_goal(input.goal_id);
    if signalled {
        registry
            .set_status(input.goal_id, AgentRunStatus::Paused)
            .await
            .map_err(|e| AgentControlError::Registry(e.to_string()))?;
    }
    Ok(PauseAgentOutput {
        goal_id: input.goal_id,
        paused: signalled,
    })
}

pub async fn resume_agent(
    input: PauseAgentInput,
    orchestrator: Arc<DriverOrchestrator>,
    registry: Arc<AgentRegistry>,
) -> Result<PauseAgentOutput, AgentControlError> {
    let signalled = orchestrator.resume_goal(input.goal_id);
    if signalled {
        registry
            .set_status(input.goal_id, AgentRunStatus::Running)
            .await
            .map_err(|e| AgentControlError::Registry(e.to_string()))?;
    }
    Ok(PauseAgentOutput {
        goal_id: input.goal_id,
        paused: !signalled,
    })
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateBudgetInput {
    pub goal_id: GoalId,
    pub max_turns: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UpdateBudgetOutput {
    Updated {
        goal_id: GoalId,
        max_turns: u32,
    },
    Rejected {
        goal_id: GoalId,
        reason: String,
    },
    NotFound {
        goal_id: GoalId,
    },
}

pub async fn update_budget(
    input: UpdateBudgetInput,
    registry: Arc<AgentRegistry>,
) -> Result<UpdateBudgetOutput, AgentControlError> {
    let Some(handle) = registry.handle(input.goal_id) else {
        return Ok(UpdateBudgetOutput::NotFound {
            goal_id: input.goal_id,
        });
    };
    let Some(new_max) = input.max_turns else {
        return Ok(UpdateBudgetOutput::Rejected {
            goal_id: input.goal_id,
            reason: "max_turns required".into(),
        });
    };
    // Only grow: cannot drop below current work or below what the
    // budget already enforces internally.
    let current_max = handle.snapshot.max_turns;
    let used = handle.snapshot.usage.turns;
    if new_max <= current_max || new_max <= used {
        return Ok(UpdateBudgetOutput::Rejected {
            goal_id: input.goal_id,
            reason: format!(
                "update_budget only grows: new={new_max} <= current_max={current_max} or used={used}"
            ),
        });
    }
    registry.set_max_turns(input.goal_id, new_max);
    Ok(UpdateBudgetOutput::Updated {
        goal_id: input.goal_id,
        max_turns: new_max,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_agent_registry::{AgentHandle, AgentSnapshot, MemoryAgentRegistryStore};
    use nexo_driver_types::BudgetUsage;
    use uuid::Uuid;

    async fn registry_with(turn_index: u32, max_turns: u32) -> (Arc<AgentRegistry>, GoalId) {
        let reg = Arc::new(AgentRegistry::new(
            Arc::new(MemoryAgentRegistryStore::default()),
            4,
        ));
        let id = GoalId(Uuid::new_v4());
        let h = AgentHandle {
            goal_id: id,
            phase_id: "67.10".into(),
            status: AgentRunStatus::Running,
            origin: None,
            dispatcher: None,
            started_at: chrono::Utc::now(),
            finished_at: None,
            snapshot: AgentSnapshot {
                turn_index,
                max_turns,
                usage: BudgetUsage {
                    turns: turn_index,
                    ..Default::default()
                },
                ..AgentSnapshot::default()
            },
        };
        reg.admit(h, true).await.unwrap();
        reg.set_max_turns(id, max_turns);
        (reg, id)
    }

    #[tokio::test]
    async fn update_budget_grows_above_current() {
        let (reg, id) = registry_with(5, 20).await;
        let out = update_budget(
            UpdateBudgetInput {
                goal_id: id,
                max_turns: Some(40),
            },
            reg.clone(),
        )
        .await
        .unwrap();
        assert!(matches!(
            out,
            UpdateBudgetOutput::Updated { max_turns: 40, .. }
        ));
        assert_eq!(reg.snapshot(id).unwrap().max_turns, 40);
    }

    #[tokio::test]
    async fn update_budget_rejects_shrink() {
        let (reg, id) = registry_with(5, 20).await;
        let out = update_budget(
            UpdateBudgetInput {
                goal_id: id,
                max_turns: Some(10),
            },
            reg,
        )
        .await
        .unwrap();
        assert!(matches!(out, UpdateBudgetOutput::Rejected { .. }));
    }

    #[tokio::test]
    async fn update_budget_rejects_below_used() {
        let (reg, id) = registry_with(15, 20).await;
        let out = update_budget(
            UpdateBudgetInput {
                goal_id: id,
                max_turns: Some(15),
            },
            reg,
        )
        .await
        .unwrap();
        assert!(matches!(out, UpdateBudgetOutput::Rejected { .. }));
    }

    #[tokio::test]
    async fn update_budget_not_found_for_unknown_goal() {
        let reg = Arc::new(AgentRegistry::new(
            Arc::new(MemoryAgentRegistryStore::default()),
            4,
        ));
        let out = update_budget(
            UpdateBudgetInput {
                goal_id: GoalId(Uuid::new_v4()),
                max_turns: Some(10),
            },
            reg,
        )
        .await
        .unwrap();
        assert!(matches!(out, UpdateBudgetOutput::NotFound { .. }));
    }
}
