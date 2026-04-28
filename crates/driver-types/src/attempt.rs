//! Attempt + compact + reset payloads.

use serde::{Deserialize, Serialize};

use crate::acceptance::{AcceptanceFailure, AcceptanceVerdict};
use crate::cancel::CancellationToken;
use crate::decision::Decision;
use crate::goal::{BudgetAxis, BudgetUsage, Goal, GoalId};

#[derive(Clone, Debug)]
pub struct AttemptParams {
    pub goal: Goal,
    /// 0 for the first attempt, monotonic across the goal's lifetime.
    pub turn_index: u32,
    /// Total budget consumption across every prior turn of this goal.
    pub usage: BudgetUsage,
    /// Decisions taken on prior turns. Harness passes them to the CLI
    /// as memory hints / system prompt context.
    pub prior_decisions: Vec<Decision>,
    /// Cooperative cancellation. Harness MUST poll between events.
    pub cancel: CancellationToken,
    /// Free-form per-attempt extras (e.g. `{"resume_session_id":"..."}`).
    pub extras: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptResult {
    pub goal_id: GoalId,
    pub turn_index: u32,
    pub outcome: AttemptOutcome,
    #[serde(default)]
    pub decisions_recorded: Vec<Decision>,
    pub usage_after: BudgetUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<AcceptanceVerdict>,
    /// Final assistant text shown to the model — what the CLI claimed
    /// when it stopped this turn. Used to compose the next prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_text: Option<String>,
    /// Opaque-to-core metadata (session ids, raw token counts, etc.).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub harness_extras: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// CLI claimed completion AND every `AcceptanceCriterion` passed.
    Done,
    /// CLI said done but acceptance failed. Driver MUST schedule the
    /// next turn with `failures` fed back as user message.
    NeedsRetry { failures: Vec<AcceptanceFailure> },
    /// CLI handed control back without claiming done — mid-conversation
    /// pause, e.g. waiting on a long tool result.
    Continue { reason: String },
    /// Runtime-local park request emitted when the model calls Sleep.
    Sleep { duration_ms: u64, reason: String },
    /// Budget exhausted on one of the four axes — driver routes to
    /// 67.10 escalate.
    BudgetExhausted { axis: BudgetAxis },
    /// External cancellation (`CancellationToken`).
    Cancelled,
    /// Harness asks the driver to escalate to operator.
    Escalate { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactParams {
    pub goal_id: GoalId,
    /// Hint — `"focus on test failures"` / `"drop unrelated"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompactResult {
    Compacted { tokens_saved: u64, summary: String },
    Skipped { reason: String },
}

impl CompactResult {
    pub fn skipped(reason: impl Into<String>) -> Self {
        Self::Skipped {
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<GoalId>,
    pub reason: ResetReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetReason {
    New,
    Reset,
    Idle,
    Daily,
    Compaction,
    Deleted,
    Unknown,
}
