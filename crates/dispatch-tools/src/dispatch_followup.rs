//! Phase 67.E.2 — `dispatch_followup` tool. Mirrors `program_phase`
//! but pulls the goal description from a FOLLOWUPS.md item (by
//! `code`, e.g. `H-1`) instead of a PHASES.md sub-phase. Useful for
//! launching hardening / refactor tasks the operator is already
//! tracking as deferred work.
//!
//! Resolved follow-ups (status = Resolved) are rejected — there is
//! no point dispatching a task that's already done. The caller can
//! still reach historical context through the read-only
//! `followup_detail` tool.

use std::sync::Arc;

use chrono::Utc;
use nexo_agent_registry::{
    AdmitOutcome, AgentHandle, AgentRegistry, AgentRunStatus, AgentSnapshot,
};
use nexo_config::DispatchPolicy;
use nexo_driver_claude::{DispatcherIdentity, OriginChannel};
use nexo_driver_loop::DriverOrchestrator;
use nexo_driver_types::{Goal, GoalId};
use nexo_project_tracker::tracker::ProjectTracker;
use nexo_project_tracker::types::FollowUpStatus;
use serde::{Deserialize, Serialize};

use crate::policy_gate::{
    CapSnapshot, DispatchDenied, DispatchGate, DispatchKind, DispatchRequest,
};
use crate::program_phase::{
    apply_default_acceptance, apply_default_budget, BudgetOverride, ProgramPhaseError,
};

#[derive(Clone, Debug, Deserialize)]
pub struct DispatchFollowupInput {
    pub code: String,
    /// Same override semantics as `program_phase`.
    #[serde(default)]
    pub acceptance_override: Option<Vec<nexo_driver_types::AcceptanceCriterion>>,
    #[serde(default)]
    pub budget_override: Option<BudgetOverride>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DispatchFollowupOutput {
    Dispatched {
        goal_id: GoalId,
        code: String,
    },
    Queued {
        goal_id: GoalId,
        code: String,
        position: usize,
    },
    Rejected {
        code: String,
        reason: String,
    },
    NotFound {
        code: String,
    },
    /// The follow-up exists but is already resolved.
    AlreadyResolved {
        code: String,
    },
    Forbidden {
        code: String,
        reason: String,
    },
    NotTracked,
}

/// Identity-style synthesis of the phase id used by the gate when a
/// follow-up code is dispatched. Format: `followup:<code>`. Lets
/// `allowed_phase_ids` / `forbidden_phase_ids` filter follow-ups
/// independently from real PHASES.md ids — operators can write
/// `forbidden_phase_ids: ["followup:*"]` to block the whole class.
pub fn followup_phase_id(code: &str) -> String {
    format!("followup:{code}")
}

#[allow(clippy::too_many_arguments)]
pub async fn dispatch_followup_call(
    input: DispatchFollowupInput,
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
) -> Result<DispatchFollowupOutput, ProgramPhaseError> {
    let items = match tracker.followups().await {
        Ok(v) => v,
        Err(nexo_project_tracker::TrackerError::NotTracked(_)) => {
            return Ok(DispatchFollowupOutput::NotTracked);
        }
        Err(e) => return Err(ProgramPhaseError::Tracker(e.to_string())),
    };
    let Some(item) = items.into_iter().find(|i| i.code == input.code) else {
        return Ok(DispatchFollowupOutput::NotFound { code: input.code });
    };
    if item.status == FollowUpStatus::Resolved {
        return Ok(DispatchFollowupOutput::AlreadyResolved { code: input.code });
    }

    let synth_id = followup_phase_id(&input.code);
    let request = DispatchRequest {
        kind: DispatchKind::Write,
        phase_id: &synth_id,
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
            | DispatchDenied::PhaseNotAllowed(_) => DispatchFollowupOutput::Forbidden {
                code: input.code,
                reason: denied.to_string(),
            },
            DispatchDenied::DispatcherCapReached { .. }
            | DispatchDenied::SenderCapReached { .. }
            | DispatchDenied::GlobalCapReached { .. } => DispatchFollowupOutput::Rejected {
                code: input.code,
                reason: denied.to_string(),
            },
        });
    }

    let description = format!(
        "[follow-up {}] {} — {}\n\n{}",
        item.code, item.section, item.title, item.body
    );
    let acceptance = input
        .acceptance_override
        .clone()
        .unwrap_or_else(apply_default_acceptance);
    let budget = apply_default_budget(input.budget_override.clone());

    let goal = Goal {
        id: GoalId::new(),
        description,
        acceptance,
        budget,
        workspace: None,
        metadata: serde_json::Map::from_iter([(
            "followup_code".into(),
            serde_json::Value::String(input.code.clone()),
        )]),
    };
    let goal_id = goal.id;

    let handle = AgentHandle {
        goal_id,
        phase_id: synth_id.clone(),
        status: AgentRunStatus::Running,
        origin: origin.clone(),
        dispatcher: Some(dispatcher.clone()),
        started_at: Utc::now(),
        finished_at: None,
        snapshot: AgentSnapshot {
            max_turns: goal.budget.max_turns,
            ..AgentSnapshot::default()
        },
        plan_mode: None,
    };
    let outcome = registry
        .admit(handle, caps.queue_when_full)
        .await
        .map_err(|e| ProgramPhaseError::Registry(e.to_string()))?;

    match outcome {
        AdmitOutcome::Admitted => {
            registry.set_max_turns(goal_id, goal.budget.max_turns);
            // B18 — same auto-audit policy as program_phase_dispatch.
            // Operator wiring sets DispatchToolContext.audit_before_done;
            // we surface the flag here via the optional hook_registry —
            // when callers passed one, attach an audit hook so
            // dispatch_followup goals get the same audit-before-done
            // pass program_phase goals get.
            if let Some(hr) = &hook_registry {
                hr.add_unique(
                    goal_id,
                    crate::hooks::types::CompletionHook {
                        id: format!("auto-audit-{}", goal_id.0.simple()),
                        on: crate::hooks::types::HookTrigger::Done,
                        action: crate::hooks::types::HookAction::DispatchAudit {
                            only_if: crate::hooks::types::HookTrigger::Done,
                        },
                    },
                );
            }
            std::mem::drop(orchestrator.clone().spawn_goal(goal));
            Ok(DispatchFollowupOutput::Dispatched {
                goal_id,
                code: input.code,
            })
        }
        AdmitOutcome::Queued { position } => Ok(DispatchFollowupOutput::Queued {
            goal_id,
            code: input.code,
            position,
        }),
        AdmitOutcome::Rejected => Ok(DispatchFollowupOutput::Rejected {
            code: input.code,
            reason: "registry rejected (cap reached + queue disabled)".into(),
        }),
    }
}
