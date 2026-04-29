use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interaction {
    pub role: Role,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

impl Interaction {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            timestamp: Utc::now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: Uuid,
    pub agent_id: String,
    pub history: Vec<Interaction>,
    pub context: Value,
    pub created_at: DateTime<Utc>,
    pub last_access: DateTime<Utc>,
    max_history_turns: usize,
    /// Phase B — when set, the rolling summary of turns that were
    /// compacted out of `history`. The agent loop prepends it as a
    /// system framing to every subsequent request so the model can
    /// continue without losing the history that was folded away.
    /// Cleared only when the session is reset.
    pub compacted_summary: Option<String>,
    /// Phase 77.6 — memory IDs already surfaced this session, so the
    /// relevance scorer can skip them. Cleared on daemon restart.
    already_surfaced: HashSet<Uuid>,
}

impl Session {
    pub fn new(agent_id: impl Into<String>, max_history_turns: usize) -> Self {
        Self::with_id(Uuid::new_v4(), agent_id, max_history_turns)
    }

    pub fn with_id(id: Uuid, agent_id: impl Into<String>, max_history_turns: usize) -> Self {
        let now = Utc::now();
        Self {
            id,
            agent_id: agent_id.into(),
            history: Vec::new(),
            context: Value::Null,
            created_at: now,
            last_access: now,
            max_history_turns,
            compacted_summary: None,
            already_surfaced: HashSet::new(),
        }
    }

    /// Phase B — apply a compaction result. Drops `history[..tail_start]`
    /// and stores the summary so subsequent turns prepend it as
    /// system framing. The replaced range is gone from in-memory
    /// history; the audit row in `compactions_v1` is the only
    /// surviving copy.
    pub fn apply_compaction(&mut self, summary: String, tail_start: usize) {
        let cap = tail_start.min(self.history.len());
        if cap > 0 {
            self.history.drain(..cap);
        }
        self.compacted_summary = Some(summary);
    }

    /// Appends an interaction, trimming the oldest entry if history exceeds max_history_turns.
    pub fn push(&mut self, interaction: Interaction) {
        self.history.push(interaction);
        self.last_access = Utc::now();
        if self.history.len() > self.max_history_turns {
            self.history.remove(0);
        }
    }

    /// Phase 77.6 — record that a memory was surfaced to the agent.
    pub fn mark_surfaced(&mut self, memory_id: Uuid) {
        self.already_surfaced.insert(memory_id);
    }

    /// Phase 77.6 — check whether a memory was already shown this session.
    pub fn is_surfaced(&self, memory_id: &Uuid) -> bool {
        self.already_surfaced.contains(memory_id)
    }

    /// Phase 77.6 — borrow the surfaced set for batch filtering.
    pub fn surfaced_set(&self) -> &HashSet<Uuid> {
        &self.already_surfaced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_keeps_history_bounded() {
        let mut session = Session::new("test-agent", 3);
        for i in 0..5 {
            session.push(Interaction::new(Role::User, format!("msg {i}")));
        }
        assert_eq!(session.history.len(), 3);
        assert_eq!(session.history[0].content, "msg 2");
        assert_eq!(session.history[2].content, "msg 4");
    }

    #[test]
    fn push_under_limit_does_not_trim() {
        let mut session = Session::new("agent", 5);
        session.push(Interaction::new(Role::User, "hello"));
        session.push(Interaction::new(Role::Assistant, "hi"));
        assert_eq!(session.history.len(), 2);
    }

    #[test]
    fn push_updates_last_access() {
        let mut session = Session::new("agent", 10);
        let before = session.last_access;
        std::thread::sleep(std::time::Duration::from_millis(2));
        session.push(Interaction::new(Role::User, "ping"));
        assert!(session.last_access > before);
    }

    #[test]
    fn mark_and_check_surfaced() {
        let mut session = Session::new("agent", 10);
        let id = Uuid::new_v4();
        assert!(!session.is_surfaced(&id));
        session.mark_surfaced(id);
        assert!(session.is_surfaced(&id));
    }

    #[test]
    fn surfaced_set_is_empty_initially() {
        let session = Session::new("agent", 10);
        assert!(session.surfaced_set().is_empty());
    }

    #[test]
    fn surfaced_set_tracks_multiple() {
        let mut session = Session::new("agent", 10);
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        session.mark_surfaced(a);
        session.mark_surfaced(b);
        assert_eq!(session.surfaced_set().len(), 2);
        assert!(session.is_surfaced(&a));
        assert!(session.is_surfaced(&b));
    }
}
