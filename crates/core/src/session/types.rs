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
        }
    }

    /// Appends an interaction, trimming the oldest entry if history exceeds max_history_turns.
    pub fn push(&mut self, interaction: Interaction) {
        self.history.push(interaction);
        self.last_access = Utc::now();
        if self.history.len() > self.max_history_turns {
            self.history.remove(0);
        }
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
}
