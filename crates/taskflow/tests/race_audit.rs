//! Regression tests for the 2026-04-24 taskflow audit:
//!
//! * `(flow_id, run_id)` is unique — a duplicate insert by two
//!   concurrent step observations fails at the DB layer instead of
//!   silently creating two rows with non-deterministic lookup.
//! * `update_and_append` is atomic — revision + event commit together.

use std::sync::Arc;

use chrono::Utc;
use nexo_taskflow::{
    manager::{CreateManagedInput, FlowManager},
    store::{FlowStore, SqliteFlowStore},
    types::{FlowStep, FlowStepStatus, StepRuntime},
};
use serde_json::json;
use uuid::Uuid;

async fn store() -> Arc<SqliteFlowStore> {
    Arc::new(SqliteFlowStore::open(":memory:").await.unwrap())
}

#[tokio::test]
async fn duplicate_flow_step_run_id_rejected_at_db_layer() {
    let s = store().await;
    let m = FlowManager::new(s.clone());

    // Create a flow first (steps reference it via FK).
    let flow = m
        .create_managed(CreateManagedInput {
            controller_id: "kate".into(),
            goal: "demo".into(),
            owner_session_key: "key".into(),
            requester_origin: "test".into(),
            current_step: "start".into(),
            state_json: json!({}),
        })
        .await
        .unwrap();

    // Insert a step with run_id="run_1".
    let step1 = FlowStep {
        id: Uuid::new_v4(),
        flow_id: flow.id,
        runtime: StepRuntime::Mirrored,
        child_session_key: None,
        run_id: "run_1".into(),
        task: "a".into(),
        status: FlowStepStatus::Running,
        result_json: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    s.insert_step(&step1).await.unwrap();

    // Second insert with the SAME (flow_id, run_id) must fail — the
    // unique index is the safety net behind TOCTOU races in the
    // manager's `record_step_observation` codepath.
    let step2 = FlowStep {
        id: Uuid::new_v4(), // fresh id
        flow_id: flow.id,
        runtime: StepRuntime::Mirrored,
        child_session_key: None,
        run_id: "run_1".into(),
        task: "b".into(),
        status: FlowStepStatus::Succeeded,
        result_json: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let err = s.insert_step(&step2).await.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("UNIQUE") || msg.contains("unique") || msg.contains("constraint"),
        "expected unique-constraint violation, got: {msg}"
    );
}

#[tokio::test]
async fn update_and_append_is_atomic_on_revision_match() {
    let s = store().await;
    let m = FlowManager::new(s.clone());
    let flow = m
        .create_managed(CreateManagedInput {
            controller_id: "kate".into(),
            goal: "demo".into(),
            owner_session_key: "key".into(),
            requester_origin: "test".into(),
            current_step: "start".into(),
            state_json: json!({}),
        })
        .await
        .unwrap();

    // Happy path: start_running bumps revision AND records event.
    let ran = m.start_running(flow.id).await.unwrap();
    assert_eq!(ran.revision, flow.revision + 1);
    let events = s.list_events(flow.id, 10).await.unwrap();
    assert!(events.iter().any(|e| e.kind == "started"));
}

#[tokio::test]
async fn prune_terminal_flows_drops_only_old_terminal_rows() {
    let s = store().await;
    let m = FlowManager::new(s.clone());

    let old = m
        .create_managed(CreateManagedInput {
            controller_id: "kate".into(),
            goal: "old".into(),
            owner_session_key: "k".into(),
            requester_origin: "t".into(),
            current_step: "start".into(),
            state_json: json!({}),
        })
        .await
        .unwrap();
    m.start_running(old.id).await.unwrap();
    m.finish(old.id, None).await.unwrap();

    let active = m
        .create_managed(CreateManagedInput {
            controller_id: "kate".into(),
            goal: "active".into(),
            owner_session_key: "k".into(),
            requester_origin: "t".into(),
            current_step: "start".into(),
            state_json: json!({}),
        })
        .await
        .unwrap();
    m.start_running(active.id).await.unwrap(); // stays Running

    // retain_days=0 → everything terminal-older-than-now gets culled.
    // Sleep a beat so `updated_at < now()` is true for the finished row.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let dropped = s.prune_terminal_flows(0).await.unwrap();
    assert_eq!(dropped, 1, "only the finished flow should be pruned");
    assert!(s.get(old.id).await.unwrap().is_none());
    assert!(s.get(active.id).await.unwrap().is_some());
}

#[tokio::test]
async fn list_events_negative_limit_guarded() {
    let s = store().await;
    let m = FlowManager::new(s.clone());
    let f = m
        .create_managed(CreateManagedInput {
            controller_id: "kate".into(),
            goal: "n".into(),
            owner_session_key: "k".into(),
            requester_origin: "t".into(),
            current_step: "start".into(),
            state_json: json!({}),
        })
        .await
        .unwrap();
    m.start_running(f.id).await.unwrap();
    // Negative limit must NOT be treated as unbounded by SQLite —
    // defensive clamp to zero → empty result.
    let events = s.list_events(f.id, -1).await.unwrap();
    assert_eq!(events.len(), 0);
}

#[tokio::test]
async fn update_and_append_rollback_on_stale_revision() {
    let s = store().await;
    let m = FlowManager::new(s.clone());
    let flow = m
        .create_managed(CreateManagedInput {
            controller_id: "kate".into(),
            goal: "demo".into(),
            owner_session_key: "key".into(),
            requester_origin: "test".into(),
            current_step: "start".into(),
            state_json: json!({}),
        })
        .await
        .unwrap();

    // Forge a stale flow handle with wrong revision.
    let mut stale = flow.clone();
    stale.revision = 999;
    stale.status = nexo_taskflow::types::FlowStatus::Running;

    let before_events = s.list_events(flow.id, 10).await.unwrap().len();
    let err = s
        .update_and_append(&stale, "manual_event", json!({}))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        nexo_taskflow::types::FlowError::RevisionMismatch { .. }
    ));
    // Atomicity: the event must NOT have been inserted even though the
    // stale revision check was done mid-transaction.
    let after_events = s.list_events(flow.id, 10).await.unwrap().len();
    assert_eq!(before_events, after_events);
}
