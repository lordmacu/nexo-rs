//! Goal + budget — what the harness is trying to achieve and how
//! much it can cost.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::acceptance::AcceptanceCriterion;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GoalId(pub Uuid);

impl GoalId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for GoalId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub id: GoalId,
    pub description: String,
    /// Every criterion must pass for the goal to be accepted.
    pub acceptance: Vec<AcceptanceCriterion>,
    pub budget: BudgetGuards,
    /// Working directory hint — the worktree that 67.4 will create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Free-form k/v metadata. Harness-specific keys live here so the
    /// core trait stays small.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetGuards {
    pub max_turns: u32,
    /// Wall-clock budget across every turn of one goal.
    #[serde(with = "humantime_serde")]
    pub max_wall_time: Duration,
    pub max_tokens: u64,
    pub max_consecutive_denies: u32,
}

impl BudgetGuards {
    /// Returns `Some(axis)` when `usage` has hit any limit, else `None`.
    pub fn is_exhausted(&self, usage: &BudgetUsage) -> Option<BudgetAxis> {
        if usage.turns >= self.max_turns {
            Some(BudgetAxis::Turns)
        } else if usage.wall_time >= self.max_wall_time {
            Some(BudgetAxis::WallTime)
        } else if usage.tokens >= self.max_tokens {
            Some(BudgetAxis::Tokens)
        } else if usage.consecutive_denies >= self.max_consecutive_denies {
            Some(BudgetAxis::ConsecutiveDenies)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetUsage {
    pub turns: u32,
    #[serde(with = "humantime_serde")]
    pub wall_time: Duration,
    pub tokens: u64,
    pub consecutive_denies: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetAxis {
    Turns,
    WallTime,
    Tokens,
    ConsecutiveDenies,
}
