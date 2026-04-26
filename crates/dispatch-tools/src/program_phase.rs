//! Phase 67.E.1 — `program_phase` tool entry-point.
//!
//! Inputs: `phase_id` (required), optional `acceptance_override`.
//! The handler:
//!
//! 1. Reads `PHASES.md` via the project tracker to derive the goal
//!    description (sub-phase title + body).
//! 2. Validates the request through `DispatchGate`.
//! 3. Constructs a `Goal` with the dispatcher / origin metadata.
//! 4. Asks `AgentRegistry::admit` for a slot. Cap reached + queue
//!    enabled → returns `Queued`; cap reached + queue disabled →
//!    returns `Rejected`.
//! 5. Spawns the goal via `DriverOrchestrator::spawn_goal` if
//!    admitted; otherwise leaves the registry entry as `Queued` and
//!    relies on `release()` to surface it later.
//!
//! Returns `ProgramPhaseOutput` so the calling agent can echo
//! `goal_id` / `status` back to the chat.
//!
//! Hook + completion-router wiring lands in 67.F.x; this step
//! exposes the dispatch surface itself.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use nexo_agent_registry::{
    AdmitOutcome, AgentHandle, AgentRegistry, AgentRunStatus, AgentSnapshot,
};
use nexo_config::DispatchPolicy;
use nexo_driver_claude::{DispatcherIdentity, OriginChannel};
use nexo_driver_loop::DriverOrchestrator;
use nexo_driver_types::{AcceptanceCriterion, BudgetGuards, Goal, GoalId};
use nexo_project_tracker::tracker::ProjectTracker;
use serde::{Deserialize, Serialize};

use crate::policy_gate::{
    CapSnapshot, DispatchDenied, DispatchGate, DispatchKind, DispatchRequest,
};

/// Tool input.
#[derive(Clone, Debug, Deserialize)]
pub struct ProgramPhaseInput {
    pub phase_id: String,
    /// When set, replaces the auto-derived acceptance list. The
    /// auto-list is `cargo build --workspace && cargo test
    /// --workspace`; an operator override might pin a smaller crate
    /// for a fast iteration loop.
    #[serde(default)]
    pub acceptance_override: Option<Vec<AcceptanceCriterion>>,
    /// Bump default budget knobs. `None` keeps the orchestrator
    /// default.
    #[serde(default)]
    pub budget_override: Option<BudgetOverride>,
    /// B4 — hooks attached at dispatch time. Common usage from a
    /// chat tool call is `[{ id: "h1", on: "done", action: {
    /// kind: "notify_origin" } }]`. The handler stores them in the
    /// HookRegistry under the new goal id; the completion router
    /// fires them on goal transitions.
    #[serde(default)]
    pub hooks: Vec<crate::hooks::types::CompletionHook>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BudgetOverride {
    pub max_turns: Option<u32>,
    #[serde(default, with = "humantime_serde::option")]
    pub max_wall_time: Option<Duration>,
    pub max_tokens: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProgramPhaseOutput {
    Dispatched {
        goal_id: GoalId,
        phase_id: String,
    },
    Queued {
        goal_id: GoalId,
        phase_id: String,
        position: usize,
    },
    Rejected {
        phase_id: String,
        reason: String,
    },
    NotFound {
        phase_id: String,
    },
    Forbidden {
        phase_id: String,
        reason: String,
    },
    NotTracked,
}

#[derive(Debug, thiserror::Error)]
pub enum ProgramPhaseError {
    #[error("tracker: {0}")]
    Tracker(String),
    #[error("registry: {0}")]
    Registry(String),
}

/// Default budget — modest enough to fit a single dev session,
/// generous enough to ship a sub-phase. 67.E.1 does not yet read
/// `program_phase.yaml`; that wiring lands when the tool is
/// registered into the runtime by the binary (67.H.1).
pub fn apply_default_budget(ov: Option<BudgetOverride>) -> BudgetGuards {
    apply_budget_override(default_budget(), ov)
}

pub fn apply_default_acceptance() -> Vec<AcceptanceCriterion> {
    default_acceptance()
}

fn default_budget() -> BudgetGuards {
    BudgetGuards {
        max_turns: 40,
        max_wall_time: Duration::from_secs(60 * 60 * 4),
        max_tokens: 2_000_000,
        max_consecutive_denies: 3,
        max_consecutive_errors: 5,
    }
}

fn apply_budget_override(mut budget: BudgetGuards, ov: Option<BudgetOverride>) -> BudgetGuards {
    if let Some(o) = ov {
        if let Some(t) = o.max_turns {
            budget.max_turns = t;
        }
        if let Some(t) = o.max_wall_time {
            budget.max_wall_time = t;
        }
        if let Some(t) = o.max_tokens {
            budget.max_tokens = t;
        }
    }
    budget
}

fn default_acceptance() -> Vec<AcceptanceCriterion> {
    vec![
        AcceptanceCriterion::shell("cargo build --workspace"),
        AcceptanceCriterion::shell("cargo test --workspace"),
    ]
}

/// Dispatch implementation, decoupled from the `ToolHandler` trait
/// so tests can drive it directly. The runtime registration that
/// adapts this into a `nexo_core::ToolHandler` lands in 67.E.x once
/// the dispatcher identity / origin context plumbing is in place.
#[allow(clippy::too_many_arguments)]
pub async fn program_phase_dispatch(
    input: ProgramPhaseInput,
    tracker: &dyn ProjectTracker,
    orchestrator: Arc<DriverOrchestrator>,
    registry: Arc<AgentRegistry>,
    policy: &DispatchPolicy,
    require_trusted: bool,
    sender_trusted: bool,
    dispatcher: DispatcherIdentity,
    origin: Option<OriginChannel>,
    caps: CapSnapshot,
    hook_registry: Option<Arc<crate::hooks::HookRegistry>>,
) -> Result<ProgramPhaseOutput, ProgramPhaseError> {
    // Tracker: fetch the sub-phase. Missing PHASES.md → NotTracked.
    let sub = match tracker.phase_detail(&input.phase_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Ok(ProgramPhaseOutput::NotFound {
                phase_id: input.phase_id,
            });
        }
        Err(nexo_project_tracker::TrackerError::NotTracked(_)) => {
            return Ok(ProgramPhaseOutput::NotTracked);
        }
        Err(e) => return Err(ProgramPhaseError::Tracker(e.to_string())),
    };

    let request = DispatchRequest {
        kind: DispatchKind::Write,
        phase_id: &input.phase_id,
        policy,
        require_trusted,
        sender_trusted,
        caps,
    };
    if let Err(denied) = DispatchGate::check(&request) {
        return Ok(match denied {
            DispatchDenied::CapabilityNone
            | DispatchDenied::CapabilityReadOnly
            | DispatchDenied::SenderNotTrusted
            | DispatchDenied::PhaseForbidden(_)
            | DispatchDenied::PhaseNotAllowed(_) => ProgramPhaseOutput::Forbidden {
                phase_id: input.phase_id,
                reason: denied.to_string(),
            },
            DispatchDenied::DispatcherCapReached { .. }
            | DispatchDenied::SenderCapReached { .. }
            | DispatchDenied::GlobalCapReached { .. } => ProgramPhaseOutput::Rejected {
                phase_id: input.phase_id,
                reason: denied.to_string(),
            },
        });
    }

    // Build the Goal.
    let description = match &sub.body {
        Some(body) if !body.trim().is_empty() => format!("{}\n\n{}", sub.title, body),
        _ => sub.title.clone(),
    };
    // Acceptance precedence:
    //   1. caller-supplied `acceptance_override`
    //   2. acceptance bullets parsed out of the sub-phase body
    //   3. workspace defaults (cargo build + cargo test)
    let acceptance = if let Some(ov) = input.acceptance_override.clone() {
        ov
    } else if let Some(parsed) = sub.acceptance.clone() {
        parsed.into_iter().map(AcceptanceCriterion::shell).collect()
    } else {
        default_acceptance()
    };
    let budget = apply_budget_override(default_budget(), input.budget_override.clone());

    // B1 — stamp origin + dispatcher into goal.metadata so
    // attempt.rs can lift them into the SessionBinding when the
    // first turn lands. Persists across daemon restart so reattach
    // can find the chat that triggered the goal.
    let mut metadata = serde_json::Map::new();
    if let Some(o) = &origin {
        metadata.insert(
            "origin_channel".into(),
            serde_json::to_value(o).unwrap_or(serde_json::Value::Null),
        );
    }
    metadata.insert(
        "dispatcher".into(),
        serde_json::to_value(&dispatcher).unwrap_or(serde_json::Value::Null),
    );

    let goal = Goal {
        id: GoalId::new(),
        description,
        acceptance,
        budget,
        workspace: None,
        metadata,
    };
    let goal_id = goal.id;

    // Register in agent-registry. Cap-reached + queue enabled → goal
    // is parked as Queued; the orchestrator's release() callback
    // pops the next-up via promote_queued. Cap-reached + queue
    // disabled → Rejected via DispatchGate above; we should never
    // see Rejected here, but match it defensively.
    let handle = AgentHandle {
        goal_id,
        phase_id: input.phase_id.clone(),
        status: AgentRunStatus::Running,
        origin: origin.clone(),
        dispatcher: Some(dispatcher.clone()),
        started_at: Utc::now(),
        finished_at: None,
        snapshot: AgentSnapshot {
            max_turns: goal.budget.max_turns,
            ..AgentSnapshot::default()
        },
    };
    let outcome = registry
        .admit(handle, caps.queue_when_full)
        .await
        .map_err(|e| ProgramPhaseError::Registry(e.to_string()))?;

    // B4 — attach hooks before spawn so the completion router
    // sees them when the goal terminates. Done for both Admitted
    // (will spawn) and Queued (will spawn after promote).
    if let Some(hr) = &hook_registry {
        for hook in input.hooks.clone() {
            hr.add(goal_id, hook);
        }
    }
    match outcome {
        AdmitOutcome::Admitted => {
            registry.set_max_turns(goal_id, goal.budget.max_turns);
            // Fire-and-forget. Caller does not await the join handle —
            // the registry + driver events are how we observe the run.
            let _ = orchestrator.clone().spawn_goal(goal);
            Ok(ProgramPhaseOutput::Dispatched {
                goal_id,
                phase_id: input.phase_id,
            })
        }
        AdmitOutcome::Queued { position } => Ok(ProgramPhaseOutput::Queued {
            goal_id,
            phase_id: input.phase_id,
            position,
        }),
        AdmitOutcome::Rejected => Ok(ProgramPhaseOutput::Rejected {
            phase_id: input.phase_id,
            reason: "registry rejected (cap reached + queue disabled)".into(),
        }),
    }
}
