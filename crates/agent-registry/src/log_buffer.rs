//! Phase 67.B.4 — per-goal ring buffer of recent driver events.
//!
//! Keeps the last `capacity` events for each `GoalId` so the
//! `agent_logs_tail` tool can answer without re-streaming NATS.
//! Strings are pre-rendered to keep the buffer cheap (no decoding
//! on read) and bounded (subject + summary, not full payloads).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::SystemTime;

use dashmap::DashMap;
use nexo_driver_types::GoalId;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogLine {
    pub at: SystemTime,
    pub subject: String,
    pub summary: String,
}

#[derive(Clone)]
pub struct LogBuffer {
    capacity: usize,
    inner: Arc<DashMap<GoalId, Mutex<VecDeque<LogLine>>>>,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            capacity: cap,
            inner: Arc::new(DashMap::new()),
        }
    }

    pub fn push(&self, goal_id: GoalId, subject: impl Into<String>, summary: impl Into<String>) {
        let line = LogLine {
            at: SystemTime::now(),
            subject: subject.into(),
            summary: summary.into(),
        };
        let entry = self
            .inner
            .entry(goal_id)
            .or_insert_with(|| Mutex::new(VecDeque::with_capacity(self.capacity)));
        let mut q = entry.value().lock();
        if q.len() == self.capacity {
            q.pop_front();
        }
        q.push_back(line);
    }

    pub fn tail(&self, goal_id: GoalId, n: usize) -> Vec<LogLine> {
        let Some(entry) = self.inner.get(&goal_id) else {
            return Vec::new();
        };
        let q = entry.value().lock();
        let take = n.min(q.len());
        q.iter().rev().take(take).rev().cloned().collect()
    }

    pub fn drop_goal(&self, goal_id: GoalId) {
        self.inner.remove(&goal_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn ring_drops_oldest_when_full() {
        let buf = LogBuffer::new(3);
        let g = GoalId(Uuid::new_v4());
        for i in 0..5 {
            buf.push(g, "agent.driver.attempt.completed", format!("turn {i}"));
        }
        let tail = buf.tail(g, 10);
        assert_eq!(tail.len(), 3);
        assert!(tail[0].summary.contains("turn 2"));
        assert!(tail[2].summary.contains("turn 4"));
    }

    #[test]
    fn missing_goal_returns_empty() {
        let buf = LogBuffer::new(3);
        let g = GoalId(Uuid::new_v4());
        assert!(buf.tail(g, 10).is_empty());
    }

    #[test]
    fn drop_goal_clears_entries() {
        let buf = LogBuffer::new(3);
        let g = GoalId(Uuid::new_v4());
        buf.push(g, "x", "y");
        assert_eq!(buf.tail(g, 10).len(), 1);
        buf.drop_goal(g);
        assert!(buf.tail(g, 10).is_empty());
    }

    #[test]
    fn capacity_zero_is_clamped_to_one() {
        let buf = LogBuffer::new(0);
        let g = GoalId(Uuid::new_v4());
        buf.push(g, "x", "1");
        buf.push(g, "x", "2");
        assert_eq!(buf.tail(g, 10).len(), 1);
    }
}
