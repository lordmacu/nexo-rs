//! Phase 67.B.3 — applying an `AttemptResult` updates the live
//! snapshot reachable through `snapshot()` / `handle()` without
//! the reader holding any lock.

use std::sync::Arc;

use chrono::Utc;
use nexo_agent_registry::{
    AgentHandle, AgentRegistry, AgentRunStatus, AgentSnapshot, MemoryAgentRegistryStore,
};
use nexo_driver_types::{AttemptOutcome, AttemptResult, BudgetUsage, GoalId};
use uuid::Uuid;

fn fresh_handle(phase: &str) -> AgentHandle {
    AgentHandle {
        goal_id: GoalId(Uuid::new_v4()),
        phase_id: phase.into(),
        status: AgentRunStatus::Running,
        origin: None,
        dispatcher: None,
        started_at: Utc::now(),
        finished_at: None,
        snapshot: AgentSnapshot::default(),
    }
}

#[tokio::test]
async fn apply_attempt_advances_turn_index() {
    let reg = AgentRegistry::new(Arc::new(MemoryAgentRegistryStore::default()), 5);
    let h = fresh_handle("67.10");
    let id = h.goal_id;
    reg.admit(h, true).await.unwrap();
    reg.set_max_turns(id, 40);

    let mut diff = serde_json::Map::new();
    diff.insert(
        "worktree.diff_stat".into(),
        serde_json::Value::String("crates/foo: +120/-30".into()),
    );

    let ev = AttemptResult {
        goal_id: id,
        turn_index: 7,
        outcome: AttemptOutcome::Continue {
            reason: "tool result pending".into(),
        },
        decisions_recorded: vec![],
        usage_after: BudgetUsage {
            turns: 7,
            ..Default::default()
        },
        acceptance: None,
        final_text: Some("turn 7 progress".into()),
        harness_extras: diff,
    };
    reg.apply_attempt(&ev).await.unwrap();

    let snap = reg.snapshot(id).expect("snapshot");
    assert_eq!(snap.turn_index, 7);
    assert_eq!(snap.max_turns, 40);
    assert_eq!(snap.usage.turns, 7);
    assert_eq!(snap.last_diff_stat.as_deref(), Some("crates/foo: +120/-30"));
    assert_eq!(snap.last_progress_text.as_deref(), Some("turn 7 progress"));

    let summary = reg.list().await.unwrap();
    let row = summary.iter().find(|r| r.goal_id == id).unwrap();
    assert_eq!(row.turn, "7/40");
}

#[tokio::test]
async fn out_of_order_attempt_is_ignored() {
    let reg = AgentRegistry::new(Arc::new(MemoryAgentRegistryStore::default()), 5);
    let h = fresh_handle("67.10");
    let id = h.goal_id;
    reg.admit(h, true).await.unwrap();

    let later = AttemptResult {
        goal_id: id,
        turn_index: 5,
        outcome: AttemptOutcome::Done,
        decisions_recorded: vec![],
        usage_after: BudgetUsage {
            turns: 5,
            ..Default::default()
        },
        acceptance: None,
        final_text: None,
        harness_extras: Default::default(),
    };
    reg.apply_attempt(&later).await.unwrap();

    let earlier = AttemptResult {
        turn_index: 2,
        ..later.clone()
    };
    reg.apply_attempt(&earlier).await.unwrap();

    let snap = reg.snapshot(id).unwrap();
    assert_eq!(snap.turn_index, 5, "earlier event must not roll back");
}

#[tokio::test]
async fn pause_resume_round_trip() {
    let reg = AgentRegistry::new(Arc::new(MemoryAgentRegistryStore::default()), 5);
    let h = fresh_handle("67.10");
    let id = h.goal_id;
    reg.admit(h, true).await.unwrap();
    reg.set_status(id, AgentRunStatus::Paused).await.unwrap();
    let h = reg.handle(id).unwrap();
    assert_eq!(h.status, AgentRunStatus::Paused);
    reg.set_status(id, AgentRunStatus::Running).await.unwrap();
    assert_eq!(reg.handle(id).unwrap().status, AgentRunStatus::Running);
}
