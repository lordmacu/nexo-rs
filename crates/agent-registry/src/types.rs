//! Public types for the agent registry.

use std::time::Duration;

use chrono::{DateTime, Utc};
use nexo_driver_claude::{DispatcherIdentity, OriginChannel};
use nexo_driver_types::{AcceptanceVerdict, BudgetUsage, GoalId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// State of a tracked agent / goal.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    /// Spawned and consuming budget.
    Running,
    /// Admitted but waiting for a slot under the global concurrency cap.
    Queued,
    /// Pause requested — loop will hold before the next turn.
    Paused,
    /// Terminal: accepted by acceptance verdict.
    Done,
    /// Terminal: budget exhausted, escalated, or replay-policy gave up.
    Failed,
    /// Terminal: explicit cancel via `cancel_agent`.
    Cancelled,
    /// Bookkeeping marker for goals that were Running when the daemon
    /// died and could not be reattached on boot.
    LostOnRestart,
}

impl AgentRunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AgentRunStatus::Done
                | AgentRunStatus::Failed
                | AgentRunStatus::Cancelled
                | AgentRunStatus::LostOnRestart
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AgentRunStatus::Running => "running",
            AgentRunStatus::Queued => "queued",
            AgentRunStatus::Paused => "paused",
            AgentRunStatus::Done => "done",
            AgentRunStatus::Failed => "failed",
            AgentRunStatus::Cancelled => "cancelled",
            AgentRunStatus::LostOnRestart => "lost_on_restart",
        }
    }
}

/// Live snapshot of one agent — refreshed on every `attempt.completed`
/// event by the registry's event subscriber (67.B.3). Atomic via
/// `ArcSwap` so readers never block writers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub turn_index: u32,
    pub max_turns: u32,
    pub usage: BudgetUsage,
    pub last_acceptance: Option<AcceptanceVerdict>,
    pub last_decision_summary: Option<String>,
    pub last_event_at: DateTime<Utc>,
    pub last_diff_stat: Option<String>,
    pub last_progress_text: Option<String>,
}

impl Default for AgentSnapshot {
    fn default() -> Self {
        Self {
            turn_index: 0,
            max_turns: 0,
            usage: BudgetUsage::default(),
            last_acceptance: None,
            last_decision_summary: None,
            last_event_at: Utc::now(),
            last_diff_stat: None,
            last_progress_text: None,
        }
    }
}

/// One row of the registry — lifecycle + identity + snapshot.
/// `JoinHandle` is held internally by the registry, not exposed
/// here; consumers only see the persistable shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentHandle {
    pub goal_id: GoalId,
    pub phase_id: String,
    pub status: AgentRunStatus,
    pub origin: Option<OriginChannel>,
    pub dispatcher: Option<DispatcherIdentity>,
    pub started_at: DateTime<Utc>,
    /// `Some` once the goal reached a terminal state.
    pub finished_at: Option<DateTime<Utc>>,
    pub snapshot: AgentSnapshot,
}

impl AgentHandle {
    pub fn elapsed(&self) -> Duration {
        let end = self.finished_at.unwrap_or_else(Utc::now);
        let secs = (end - self.started_at).num_seconds().max(0) as u64;
        Duration::from_secs(secs)
    }
}

/// Compact list-row used by `list_agents`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentSummary {
    pub goal_id: GoalId,
    pub phase_id: String,
    pub status: AgentRunStatus,
    pub turn: String,
    #[serde(with = "humantime_serde")]
    pub wall: Duration,
    pub origin: String,
}

impl AgentSummary {
    pub fn from_handle(h: &AgentHandle) -> Self {
        let origin = match &h.origin {
            Some(o) => format!("{}:{}@{}", o.plugin, o.instance, o.sender_id),
            None => "—".into(),
        };
        let turn = if h.snapshot.max_turns > 0 {
            format!("{}/{}", h.snapshot.turn_index, h.snapshot.max_turns)
        } else {
            format!("{}", h.snapshot.turn_index)
        };
        Self {
            goal_id: h.goal_id,
            phase_id: h.phase_id.clone(),
            status: h.status,
            turn,
            wall: h.elapsed(),
            origin,
        }
    }
}

#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("goal {0:?} not found in registry")]
    NotFound(GoalId),
    #[error("registry full: {0} active goals, max {1}")]
    CapReached(u32, u32),
    #[error("invalid status transition: {from:?} → {to:?}")]
    InvalidTransition { from: AgentRunStatus, to: AgentRunStatus },
    #[error(transparent)]
    Store(#[from] crate::store::AgentRegistryStoreError),
}
