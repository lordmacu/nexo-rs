//! End-to-end recall semantics for `SqliteVecDecisionMemory`.

use std::sync::Arc;

use chrono::Utc;
use nexo_driver_loop::memory::mock::MockEmbedder;
use nexo_driver_loop::{DecisionMemory, Namespace, NoopDecisionMemory, SqliteVecDecisionMemory};
use nexo_driver_permission::PermissionRequest;
use nexo_driver_types::{Decision, DecisionChoice, DecisionId, GoalId};
use serde_json::json;
use uuid::Uuid;

fn dec(goal: GoalId, tool: &str, input: serde_json::Value) -> Decision {
    Decision {
        id: DecisionId::new(),
        goal_id: goal,
        turn_index: 0,
        tool: tool.into(),
        input,
        choice: DecisionChoice::Allow,
        rationale: "ok".into(),
        decided_at: Utc::now(),
    }
}

fn req(tool: &str, input: serde_json::Value) -> PermissionRequest {
    PermissionRequest {
        goal_id: GoalId::new(),
        tool_use_id: "tu".into(),
        tool_name: tool.into(),
        input,
        metadata: serde_json::Map::new(),
    }
}

#[tokio::test]
async fn record_then_recall_returns_at_least_one() {
    let mem = SqliteVecDecisionMemory::open_memory(Arc::new(MockEmbedder::new()))
        .await
        .unwrap();
    let g = GoalId::new();
    mem.record(&dec(g, "Edit", json!({"file": "src/lib.rs"})))
        .await
        .unwrap();
    let hits = mem
        .recall(&req("Edit", json!({"file": "src/lib.rs"})), 5)
        .await;
    assert!(!hits.is_empty(), "expected at least one recall hit");
}

#[tokio::test]
async fn per_goal_namespace_filters_other_goals() {
    let g1 = GoalId(Uuid::new_v4());
    let g2 = GoalId(Uuid::new_v4());
    let global = SqliteVecDecisionMemory::open_memory(Arc::new(MockEmbedder::new()))
        .await
        .unwrap();
    global
        .record(&dec(g1, "Edit", json!({"file": "a.rs"})))
        .await
        .unwrap();
    global
        .record(&dec(g2, "Edit", json!({"file": "a.rs"})))
        .await
        .unwrap();

    // Re-open with PerGoal(g1) — recall must only return g1's record.
    // Same DB pool? we can't re-namespace an existing instance, so
    // exercise via with_namespace builder.
    let g1_view = SqliteVecDecisionMemory::open_memory(Arc::new(MockEmbedder::new()))
        .await
        .unwrap()
        .with_namespace(Namespace::PerGoal(g1));
    // The new in-memory pool is separate; replay records for g1.
    g1_view
        .record(&dec(g1, "Edit", json!({"file": "a.rs"})))
        .await
        .unwrap();
    g1_view
        .record(&dec(g2, "Edit", json!({"file": "a.rs"})))
        .await
        .unwrap();
    let hits = g1_view
        .recall(&req("Edit", json!({"file": "a.rs"})), 5)
        .await;
    assert!(!hits.is_empty());
    for h in &hits {
        assert_eq!(h.goal_id, g1, "goal_id leaked: {h:?}");
    }
}

#[tokio::test]
async fn noop_decision_memory_is_passthrough() {
    let n = NoopDecisionMemory;
    n.record(&dec(GoalId::new(), "Edit", json!({})))
        .await
        .unwrap();
    let hits = n.recall(&req("Edit", json!({})), 5).await;
    assert!(hits.is_empty());
}
