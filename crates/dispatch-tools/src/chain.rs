//! Phase 67.G.1 — `program_phase_chain` and `program_phase_parallel`
//! orchestration helpers.
//!
//! Both build on `program_phase_dispatch` so the gate / registry /
//! tracker plumbing stays in one place. Difference:
//!
//! - `program_phase_parallel` dispatches every phase up-front. The
//!   AgentRegistry handles capacity: phases beyond the global cap
//!   land as `Queued` rather than failing. Returns one
//!   `ProgramPhaseOutput` per requested phase.
//! - `program_phase_chain` dispatches only the first phase and
//!   returns the full sequence. The caller (or hook layer)
//!   attaches a `dispatch_phase` hook to each spawned goal so the
//!   next phase fires when the previous succeeds. Done that way
//!   instead of looping in this helper because chains can outlive
//!   any single tool call — the daemon needs to keep going if the
//!   process restarts mid-chain.

use std::sync::Arc;

use nexo_agent_registry::AgentRegistry;
use nexo_config::DispatchPolicy;
use nexo_driver_claude::{DispatcherIdentity, OriginChannel};
use nexo_driver_loop::DriverOrchestrator;
use nexo_project_tracker::tracker::ProjectTracker;
use serde::{Deserialize, Serialize};

use crate::hooks::types::{CompletionHook, HookAction, HookTrigger};
use crate::policy_gate::CapSnapshot;
use crate::program_phase::{
    program_phase_dispatch, ProgramPhaseError, ProgramPhaseInput, ProgramPhaseOutput,
};

#[derive(Clone, Debug, Deserialize)]
pub struct ProgramPhaseParallelInput {
    pub phases: Vec<String>,
    /// Cap on how many of `phases` to dispatch in this call. `None`
    /// means no extra cap on top of the registry's global cap.
    #[serde(default)]
    pub max_concurrent: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProgramPhaseParallelOutput {
    pub results: Vec<ProgramPhaseOutput>,
}

/// Dispatch every phase in `input.phases` independently, respecting
/// the registry's global cap (over-cap entries land as `Queued`).
/// Errors short-circuit the batch — partial successes still come
/// back via the assembled `Vec` up to the failing call.
#[allow(clippy::too_many_arguments)]
pub async fn program_phase_parallel(
    input: ProgramPhaseParallelInput,
    tracker: &dyn ProjectTracker,
    orchestrator: Arc<DriverOrchestrator>,
    registry: Arc<AgentRegistry>,
    policy: &DispatchPolicy,
    require_trusted: bool,
    sender_trusted: bool,
    dispatcher: DispatcherIdentity,
    origin: Option<OriginChannel>,
    base_caps: CapSnapshot,
) -> Result<ProgramPhaseParallelOutput, ProgramPhaseError> {
    let local_cap = input.max_concurrent.unwrap_or(u32::MAX);
    let mut results = Vec::with_capacity(input.phases.len());
    let mut admitted: u32 = 0;
    for phase in input.phases {
        let mut caps = base_caps;
        // Burn local-cap slots ourselves; the gate's global cap is
        // re-read on each call from the live registry counters
        // (caller is expected to refresh `base_caps.global_running`).
        if admitted >= local_cap {
            results.push(ProgramPhaseOutput::Rejected {
                phase_id: phase,
                reason: "max_concurrent reached for this call".into(),
            });
            continue;
        }
        caps.global_running = base_caps.global_running.saturating_add(admitted);
        let out = program_phase_dispatch(
            ProgramPhaseInput {
                phase_id: phase,
                acceptance_override: None,
                budget_override: None,
                hooks: Vec::new(),
            },
            tracker,
            orchestrator.clone(),
            registry.clone(),
            policy,
            require_trusted,
            sender_trusted,
            dispatcher.clone(),
            origin.clone(),
            caps,
            None,
        )
        .await?;
        if matches!(
            out,
            ProgramPhaseOutput::Dispatched { .. } | ProgramPhaseOutput::Queued { .. }
        ) {
            admitted += 1;
        }
        results.push(out);
    }
    Ok(ProgramPhaseParallelOutput { results })
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProgramPhaseChainInput {
    pub phases: Vec<String>,
    /// When true, the synthesised `dispatch_phase` hooks have
    /// `only_if = Done` so the chain stops on the first failure.
    /// When false, the hooks use `only_if = Done` as well (chains
    /// without a Done guard are nonsense — failure should escalate
    /// to the operator, not silently keep chaining).
    #[serde(default = "default_stop_on_fail")]
    pub stop_on_fail: bool,
}

fn default_stop_on_fail() -> bool {
    true
}

#[derive(Clone, Debug, Serialize)]
pub struct ProgramPhaseChainOutput {
    /// Result of dispatching the first phase. The remaining
    /// phases follow via the synthesised hooks.
    pub first: ProgramPhaseOutput,
    /// Hooks the caller should attach to the first goal so the
    /// chain progresses. Each `DispatchPhase` action targets the
    /// next phase; the caller (runtime / `add_hook` tool) attaches
    /// these to the freshly-spawned goal in the order returned.
    pub chain_hooks: Vec<CompletionHook>,
    pub stop_on_fail: bool,
}

/// Build a chain: dispatch the first phase and return the
/// `dispatch_phase` hooks the caller attaches to the spawned goal
/// so each subsequent phase fires when the previous one finishes.
/// We keep hook-attachment out of this helper because the tool
/// surface for that lives in `add_hook` (67.G.2) and the runtime
/// is the right place to bind hook identity to the registered
/// goal.
#[allow(clippy::too_many_arguments)]
pub async fn program_phase_chain(
    input: ProgramPhaseChainInput,
    tracker: &dyn ProjectTracker,
    orchestrator: Arc<DriverOrchestrator>,
    registry: Arc<AgentRegistry>,
    policy: &DispatchPolicy,
    require_trusted: bool,
    sender_trusted: bool,
    dispatcher: DispatcherIdentity,
    origin: Option<OriginChannel>,
    caps: CapSnapshot,
) -> Result<ProgramPhaseChainOutput, ProgramPhaseError> {
    if input.phases.is_empty() {
        return Ok(ProgramPhaseChainOutput {
            first: ProgramPhaseOutput::NotFound {
                phase_id: "".into(),
            },
            chain_hooks: Vec::new(),
            stop_on_fail: input.stop_on_fail,
        });
    }
    let mut iter = input.phases.into_iter();
    let head = iter.next().unwrap();
    let first = program_phase_dispatch(
        ProgramPhaseInput {
            phase_id: head,
            acceptance_override: None,
            budget_override: None,
            hooks: Vec::new(),
        },
        tracker,
        orchestrator.clone(),
        registry.clone(),
        policy,
        require_trusted,
        sender_trusted,
        dispatcher.clone(),
        origin.clone(),
        caps,
        None,
    )
    .await?;

    let chain_hooks: Vec<CompletionHook> = iter
        .enumerate()
        .map(|(i, phase)| CompletionHook {
            id: format!("chain-{}", i + 1),
            on: HookTrigger::Done,
            action: HookAction::DispatchPhase {
                phase_id: phase,
                only_if: HookTrigger::Done,
            },
        })
        .collect();

    Ok(ProgramPhaseChainOutput {
        first,
        chain_hooks,
        stop_on_fail: input.stop_on_fail,
    })
}
