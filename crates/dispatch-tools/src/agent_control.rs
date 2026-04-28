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
use std::time::Duration;

use chrono::Utc;
use nexo_agent_registry::{AgentRegistry, AgentRunStatus, AskPendingState};
use nexo_driver_loop::DriverOrchestrator;
use nexo_driver_types::GoalId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::hooks::{
    CompletionHook, HookAction, HookDispatcher, HookPayload, HookTransition, HookTrigger,
};

fn spawn_ask_timeout_task(
    goal_id: GoalId,
    question_id: String,
    wait_secs: u64,
    orchestrator: Arc<DriverOrchestrator>,
    registry: Arc<AgentRegistry>,
    hook_dispatcher: Option<Arc<dyn HookDispatcher>>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(wait_secs.max(1))).await;
        let Some(h) = registry.handle(goal_id) else {
            return;
        };
        if h.status != AgentRunStatus::Paused {
            return;
        }
        let pending_matches = h
            .snapshot
            .ask_pending
            .as_ref()
            .map(|p| p.question_id == question_id)
            .unwrap_or(false);
        if !pending_matches {
            return;
        }
        let _ = orchestrator.cancel_goal(goal_id);
        let _ = registry.set_ask_pending(goal_id, None).await;
        let _ = registry
            .set_status(goal_id, AgentRunStatus::Cancelled)
            .await;
        if let (Some(dispatcher), Some(origin)) = (hook_dispatcher, h.origin.clone()) {
            let hook = CompletionHook {
                id: format!("ask-user-timeout-{}", uuid::Uuid::new_v4()),
                on: HookTrigger::Cancelled,
                action: HookAction::NotifyOrigin,
            };
            let payload = HookPayload {
                goal_id,
                phase_id: h.phase_id,
                transition: HookTransition::Cancelled,
                summary: "[abandoned] ask_user_question timeout reached; goal cancelled.".into(),
                elapsed: String::new(),
                diff_stat: None,
                origin: Some(origin),
            };
            let _ = dispatcher.dispatch(&hook, &payload).await;
        }
    });
}

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

fn default_ask_timeout_secs() -> u64 {
    3600
}

#[derive(Clone, Debug, Deserialize)]
pub struct AskUserQuestionInput {
    pub goal_id: GoalId,
    pub question: String,
    #[serde(default = "default_ask_timeout_secs")]
    pub timeout_secs: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AskUserQuestionOutput {
    pub goal_id: GoalId,
    pub paused: bool,
    pub asked: bool,
    pub timeout_secs: u64,
}

pub async fn ask_user_question(
    input: AskUserQuestionInput,
    orchestrator: Arc<DriverOrchestrator>,
    registry: Arc<AgentRegistry>,
    hook_dispatcher: Option<Arc<dyn HookDispatcher>>,
) -> Result<AskUserQuestionOutput, AgentControlError> {
    let question_id = uuid::Uuid::new_v4().to_string();
    let paused = orchestrator.pause_goal(input.goal_id);
    if paused {
        registry
            .set_status(input.goal_id, AgentRunStatus::Paused)
            .await
            .map_err(|e| AgentControlError::Registry(e.to_string()))?;
    }
    registry
        .set_ask_pending(
            input.goal_id,
            Some(AskPendingState {
                question_id: question_id.clone(),
                question: input.question.clone(),
                asked_at: Utc::now(),
                timeout_secs: input.timeout_secs.max(1),
            }),
        )
        .await
        .map_err(|e| AgentControlError::Registry(e.to_string()))?;
    let mut asked = false;
    if let (Some(dispatcher), Some(handle)) =
        (hook_dispatcher.clone(), registry.handle(input.goal_id))
    {
        if let Some(origin) = handle.origin.clone() {
            let hook = CompletionHook {
                id: format!("ask-user-question-{}", uuid::Uuid::new_v4()),
                on: HookTrigger::Progress { every_turns: 1 },
                action: HookAction::NotifyOrigin,
            };
            let payload = HookPayload {
                goal_id: input.goal_id,
                phase_id: handle.phase_id.clone(),
                transition: HookTransition::Progress,
                summary: format!(
                    "[question] {}\n[question_id] {}\n\nReply in this same chat to continue this goal.",
                    input.question, question_id
                ),
                elapsed: String::new(),
                diff_stat: None,
                origin: Some(origin),
            };
            if dispatcher.dispatch(&hook, &payload).await.is_ok() {
                asked = true;
            }
        }
    }

    let timeout_secs = input.timeout_secs.max(1);
    spawn_ask_timeout_task(
        input.goal_id,
        question_id,
        timeout_secs,
        Arc::clone(&orchestrator),
        Arc::clone(&registry),
        hook_dispatcher,
    );

    Ok(AskUserQuestionOutput {
        goal_id: input.goal_id,
        paused,
        asked,
        timeout_secs,
    })
}

/// Phase 77.16 — boot-time timeout rearm after registry reattach.
/// Scans paused goals with `ask_pending` and recreates in-memory timeout
/// tasks with the remaining duration.
pub async fn rearm_ask_user_timeouts(
    orchestrator: Arc<DriverOrchestrator>,
    registry: Arc<AgentRegistry>,
    hook_dispatcher: Option<Arc<dyn HookDispatcher>>,
) -> usize {
    let now = Utc::now();
    let mut armed = 0usize;
    for h in registry.list_handles_in_memory() {
        if h.status != AgentRunStatus::Paused {
            continue;
        }
        let Some(p) = h.snapshot.ask_pending.clone() else {
            continue;
        };
        let elapsed = (now - p.asked_at).num_seconds().max(0) as u64;
        let remaining = p.timeout_secs.saturating_sub(elapsed);
        spawn_ask_timeout_task(
            h.goal_id,
            p.question_id,
            remaining,
            Arc::clone(&orchestrator),
            Arc::clone(&registry),
            hook_dispatcher.clone(),
        );
        armed += 1;
    }
    armed
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
    Updated { goal_id: GoalId, max_turns: u32 },
    Rejected { goal_id: GoalId, reason: String },
    NotFound { goal_id: GoalId },
}

pub async fn update_budget(
    input: UpdateBudgetInput,
    registry: Arc<AgentRegistry>,
    orchestrator: Arc<DriverOrchestrator>,
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
    // B2 — push the new cap to the orchestrator first so we don't
    // touch the snapshot when the goal isn't actually running.
    // set_goal_max_turns returns None when the goal isn't tracked
    // in the orchestrator's cancel_tokens map. Goals that admitted
    // as Queued (registry only, no spawn yet) fall in that bucket;
    // operator should wait until they're Running.
    if orchestrator
        .set_goal_max_turns(input.goal_id, new_max)
        .is_none()
    {
        return Ok(UpdateBudgetOutput::Rejected {
            goal_id: input.goal_id,
            reason: "goal not running in this orchestrator".into(),
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
    use std::path::PathBuf;
    use std::time::Duration;

    use nexo_agent_registry::{AgentHandle, AgentSnapshot, MemoryAgentRegistryStore};
    use nexo_driver_claude::{ClaudeConfig, ClaudeDefaultArgs, MemoryBindingStore, OutputFormat};
    use nexo_driver_loop::{NoopEventSink, WorkspaceManager};
    use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
    use nexo_driver_types::BudgetUsage;
    use uuid::Uuid;

    async fn build_orch() -> Arc<DriverOrchestrator> {
        let dir = std::env::temp_dir().join(format!(
            "nexo-update-budget-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = ClaudeConfig {
            binary: Some(PathBuf::from("bash")),
            default_args: ClaudeDefaultArgs {
                output_format: OutputFormat::StreamJson,
                permission_prompt_tool: None,
                allowed_tools: vec![],
                disallowed_tools: vec![],
                model: None,
            },
            mcp_config: None,
            forced_kill_after: Duration::from_secs(1),
            turn_timeout: Duration::from_secs(10),
        };
        Arc::new(
            DriverOrchestrator::builder()
                .claude_config(cfg)
                .binding_store(Arc::new(MemoryBindingStore::new())
                    as Arc<dyn nexo_driver_claude::SessionBindingStore>)
                .decider(Arc::new(AllowAllDecider) as Arc<dyn PermissionDecider>)
                .workspace_manager(Arc::new(WorkspaceManager::new(&dir)))
                .event_sink(Arc::new(NoopEventSink))
                .bin_path(PathBuf::from("/usr/local/bin/nexo-driver-permission-mcp"))
                .socket_path(dir.join("orch.sock"))
                .build()
                .await
                .unwrap(),
        )
    }

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
            plan_mode: None,
        };
        reg.admit(h, true).await.unwrap();
        reg.set_max_turns(id, max_turns);
        (reg, id)
    }

    #[tokio::test]
    async fn update_budget_rejects_when_goal_not_in_orchestrator() {
        // B2 contract: snapshot-only updates are rejected. The
        // operator update must reach the live loop or it would
        // silently lie. registry_with admits to the registry but
        // never spawns into the orchestrator, so set_goal_max_turns
        // returns None and the function rejects.
        let (reg, id) = registry_with(5, 20).await;
        let out = update_budget(
            UpdateBudgetInput {
                goal_id: id,
                max_turns: Some(40),
            },
            reg.clone(),
            build_orch().await,
        )
        .await
        .unwrap();
        assert!(matches!(out, UpdateBudgetOutput::Rejected { .. }));
        // Snapshot stays at the original 20 because we rejected
        // before touching it.
        assert_eq!(reg.snapshot(id).unwrap().max_turns, 20);
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
            build_orch().await,
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
            build_orch().await,
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
            build_orch().await,
        )
        .await
        .unwrap();
        assert!(matches!(out, UpdateBudgetOutput::NotFound { .. }));
    }
}
