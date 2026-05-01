//! Phase 80.1.b — driver-loop ↔ nexo-dream interface contracts.
//!
//! Defined here (in the low-level types crate) to avoid the cycle
//! `nexo-driver-loop ↔ nexo-dream`: both crates depend on
//! `nexo-driver-types`, so the interface lives upstream of both.
//!
//! `nexo-dream` impls `AutoDreamHook for AutoDreamRunner`, doing
//! the `RunOutcome → AutoDreamOutcomeKind` conversion internally.
//! `nexo-driver-loop` calls only the trait method — it never sees
//! `RunOutcome`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::goal::GoalId;

/// Outcome of one `check_and_run` invocation, lossy-collapsed for
/// the lightweight per-turn event. Detailed run state lives in the
/// 80.18 `dream_runs` audit row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoDreamOutcomeKind {
    Completed,
    SkippedDisabled,
    SkippedKairosActive,
    SkippedRemoteMode,
    SkippedAutoMemoryDisabled,
    SkippedTimeGate,
    SkippedScanThrottle,
    SkippedSessionGate,
    LockBlocked,
    Errored,
    TimedOut,
    EscapeAudit,
}

/// Per-turn input to the autoDream runner. Caller (driver-loop's
/// per-turn loop) builds this from goal-level state.
///
/// Slim shape — does NOT carry the parent `AgentContext` or
/// `ChatRequest`. The runner clones an operator-supplied
/// `parent_ctx_template` + builds a fresh `ChatRequest` per fork
/// (mirror Phase 77.5 `ExtractMemories`; same trade-off accepting
/// no parent prompt-cache share).
#[derive(Debug, Clone)]
pub struct DreamContext {
    /// Phase 80.1.b.b.b.c — owning agent identifier. Populated by
    /// the orchestrator's `run_turn` from
    /// `goal.metadata["agent_id"]`. Routing key for the
    /// multi-runner orchestrator: an empty string means the goal
    /// did not declare its owning agent and the orchestrator skips
    /// `auto_dream` dispatch with a warn.
    pub agent_id: String,
    pub goal_id: GoalId,
    pub session_id: String,
    pub transcript_dir: PathBuf,
    /// Phase 80.15 future — when `assistant_mode: true` on the
    /// binding, KAIROS runs the dream from a disk skill so
    /// autoDream skips. Hardcoded `false` until 80.15 ships.
    pub kairos_active: bool,
    /// Driver-loop runtime flag — `true` when running in
    /// remote-control mode. Hardcoded `false` until exposed.
    pub remote_mode: bool,
}

/// Trait the driver-loop orchestrator calls. Concrete impl lives in
/// `nexo-dream` (`impl AutoDreamHook for AutoDreamRunner`).
#[async_trait]
pub trait AutoDreamHook: Send + Sync + 'static {
    /// Per-turn check + optional fork. Returns the lightweight
    /// outcome kind for telemetry. Detailed run state lives in
    /// the 80.18 `dream_runs` audit row.
    async fn check_and_run(&self, ctx: &DreamContext) -> AutoDreamOutcomeKind;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_kind_serde_round_trip() {
        for k in [
            AutoDreamOutcomeKind::Completed,
            AutoDreamOutcomeKind::SkippedDisabled,
            AutoDreamOutcomeKind::SkippedKairosActive,
            AutoDreamOutcomeKind::SkippedRemoteMode,
            AutoDreamOutcomeKind::SkippedAutoMemoryDisabled,
            AutoDreamOutcomeKind::SkippedTimeGate,
            AutoDreamOutcomeKind::SkippedScanThrottle,
            AutoDreamOutcomeKind::SkippedSessionGate,
            AutoDreamOutcomeKind::LockBlocked,
            AutoDreamOutcomeKind::Errored,
            AutoDreamOutcomeKind::TimedOut,
            AutoDreamOutcomeKind::EscapeAudit,
        ] {
            let json = serde_json::to_string(&k).unwrap();
            let back: AutoDreamOutcomeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, back);
        }
    }
}
