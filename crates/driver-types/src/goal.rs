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
    /// core trait stays small. Convention keys (read by upstream
    /// subsystems):
    ///
    /// - `agent_id: String` — owning agent. Phase 80.1.b.b.b.c uses
    ///   this key to route per-turn `AutoDreamHook` dispatch to the
    ///   correct runner. Use [`Goal::with_agent_id`] /
    ///   [`Goal::agent_id`] to set/read it instead of touching the
    ///   map directly.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

impl Goal {
    /// Phase 80.1.b.b.b.c — store the owning agent id under the
    /// canonical `agent_id` metadata key. Required for goals that
    /// should trigger `auto_dream` consolidation; harmless on goals
    /// that do not.
    pub fn with_agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.metadata.insert(
            "agent_id".to_string(),
            serde_json::Value::String(agent_id.into()),
        );
        self
    }

    /// Read the canonical `agent_id` metadata key. `None` when the
    /// key is absent, the wrong shape (non-string), or empty.
    pub fn agent_id(&self) -> Option<&str> {
        self.metadata
            .get("agent_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acceptance::AcceptanceCriterion;

    fn sample_goal() -> Goal {
        Goal {
            id: GoalId::new(),
            description: "test".into(),
            acceptance: Vec::<AcceptanceCriterion>::new(),
            budget: BudgetGuards {
                max_turns: 5,
                max_wall_time: Duration::from_secs(60),
                max_tokens: 1_000,
                max_consecutive_denies: 3,
                max_consecutive_errors: 3,
            },
            workspace: None,
            metadata: serde_json::Map::new(),
        }
    }

    #[test]
    fn with_agent_id_populates_metadata() {
        let g = sample_goal().with_agent_id("ana");
        assert_eq!(g.agent_id(), Some("ana"));
        assert_eq!(
            g.metadata.get("agent_id"),
            Some(&serde_json::Value::String("ana".into()))
        );
    }

    #[test]
    fn agent_id_returns_none_when_metadata_missing() {
        let g = sample_goal();
        assert_eq!(g.agent_id(), None);
    }

    #[test]
    fn agent_id_returns_none_when_value_is_wrong_shape() {
        let mut g = sample_goal();
        g.metadata
            .insert("agent_id".to_string(), serde_json::json!(42));
        assert_eq!(g.agent_id(), None);
    }

    #[test]
    fn agent_id_returns_none_when_value_is_empty_string() {
        let mut g = sample_goal();
        g.metadata.insert(
            "agent_id".to_string(),
            serde_json::Value::String(String::new()),
        );
        assert_eq!(g.agent_id(), None);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetGuards {
    pub max_turns: u32,
    /// Wall-clock budget across every turn of one goal.
    #[serde(with = "humantime_serde")]
    pub max_wall_time: Duration,
    pub max_tokens: u64,
    pub max_consecutive_denies: u32,
    /// Phase 67.8 — cap on consecutive transient errors classified by
    /// the replay policy as `FreshSessionRetry`. `0` disables the
    /// axis (effectively infinite). `#[serde(default)]` so payloads
    /// from 67.0–67.7 deserialise with the helper-default of 5.
    #[serde(default = "default_max_consecutive_errors")]
    pub max_consecutive_errors: u32,
}

fn default_max_consecutive_errors() -> u32 {
    5
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
        } else if self.max_consecutive_errors > 0
            && usage.consecutive_errors >= self.max_consecutive_errors
        {
            Some(BudgetAxis::ConsecutiveErrors)
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
    /// Phase 67.8 — count of consecutive `FreshSessionRetry` decisions
    /// the replay policy made for this goal. Reset by any successful
    /// turn (`Done` or `NeedsRetry`). `#[serde(default)]` for
    /// backward-compat with 67.0–67.7 payloads.
    #[serde(default)]
    pub consecutive_errors: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetAxis {
    Turns,
    WallTime,
    Tokens,
    ConsecutiveDenies,
    /// Phase 67.8.
    ConsecutiveErrors,
}
