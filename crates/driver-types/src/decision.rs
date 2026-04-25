//! Decisions taken by the driver during an attempt — every
//! permission-tool call records one of these.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::goal::GoalId;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecisionId(pub Uuid);

impl DecisionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for DecisionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub id: DecisionId,
    pub goal_id: GoalId,
    pub turn_index: u32,
    /// `"Edit"`, `"Bash"`, `"Write"`, etc. — whatever the CLI proposed.
    pub tool: String,
    /// JSON view of the tool input. Truncation is the persister's job.
    pub input: serde_json::Value,
    pub choice: DecisionChoice,
    /// Driver's natural-language rationale (LLM output).
    pub rationale: String,
    pub decided_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionChoice {
    /// Tool may proceed unchanged.
    Allow,
    /// Tool must NOT proceed; `message` is fed back as a permission-
    /// tool error so the CLI can reformulate.
    Deny { message: String },
    /// Pass-through with operator note. Reserved for shadow-mode
    /// (Phase 67.11) — logs without enforcing.
    Observe { note: String },
}
