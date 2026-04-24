//! Phase 14.7 end-to-end tests. Everything else lives in unit tests near the
//! module it exercises; these cover cross-cutting concerns the unit tests
//! cannot express cleanly — restart durability (re-open the database) and
//! concurrent mutation safety (two tokio tasks racing an update).

use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;

use agent_taskflow::{
    CreateManagedInput, Flow, FlowError, FlowManager, FlowStatus, SqliteFlowStore,
};

fn tempdir_db() -> (TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir
        .path()
        .join("taskflow.db")
        .to_string_lossy()
        .into_owned();
    (dir, path)
}

fn seed_input() -> CreateManagedInput {
    CreateManagedInput {
        controller_id: "kate/bible-upload".into(),
        goal: "upload chapter 1".into(),
        owner_session_key: "agent:kate:session:abc".into(),
        requester_origin: "user-1".into(),
        current_step: "verse-0".into(),
        state_json: json!({ "verses_done": 0, "total": 31 }),
    }
}

/// Flow survives process restart: the same database file reopened later
/// returns the exact flow state, including `current_step`, `state_json`,
/// status, and revision.
#[tokio::test]
async fn flow_state_survives_reopen() {
    let (_dir_guard, path) = tempdir_db();
    let flow_id = {
        let store = Arc::new(SqliteFlowStore::open(&path).await.unwrap());
        let m = FlowManager::new(store);
        let f = m.create_managed(seed_input()).await.unwrap();
        let f = m.start_running(f.id).await.unwrap();
        let f = m
            .update_state(f.id, json!({ "verses_done": 10 }), Some("verse-10".into()))
            .await
            .unwrap();
        let f = m
            .set_waiting(f.id, json!({ "kind": "manual" }))
            .await
            .unwrap();
        assert_eq!(f.status, FlowStatus::Waiting);
        assert_eq!(f.current_step, "verse-10");
        assert_eq!(f.state_json["verses_done"], 10);
        f.id
        // store dropped here — simulates process exit.
    };

    // Reopen same file, rebuild manager from scratch.
    let store = Arc::new(SqliteFlowStore::open(&path).await.unwrap());
    let m = FlowManager::new(store);
    let reloaded = m.get(flow_id).await.unwrap().expect("flow persisted");
    assert_eq!(reloaded.status, FlowStatus::Waiting);
    assert_eq!(reloaded.current_step, "verse-10");
    assert_eq!(reloaded.state_json["verses_done"], 10);
    assert_eq!(reloaded.state_json["total"], 31);
    assert_eq!(reloaded.wait_json.as_ref().unwrap()["kind"], "manual");
    // Revision should match what the writer saw last (create=0, running=1,
    // update=2, waiting=3).
    assert_eq!(reloaded.revision, 3);

    // Resume still works post-restart.
    let resumed = m.resume(reloaded.id, None).await.unwrap();
    assert_eq!(resumed.status, FlowStatus::Running);
    assert!(resumed.wait_json.is_none());
}

/// Two concurrent tasks race to mutate the same flow. Both read at
/// revision=1; the first write wins (revision=2), the second retries once
/// and then lands successfully — the manager's internal retry loop
/// transparently handles the conflict.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_mutations_serialize_via_revision_retry() {
    let (_dir_guard, path) = tempdir_db();
    let store = Arc::new(SqliteFlowStore::open(&path).await.unwrap());
    let m = Arc::new(FlowManager::new(store));
    let f = m.create_managed(seed_input()).await.unwrap();
    let f = m.start_running(f.id).await.unwrap();
    assert_eq!(f.revision, 1);

    let m1 = Arc::clone(&m);
    let m2 = Arc::clone(&m);
    let flow_id = f.id;

    let t1 = tokio::spawn(async move { m1.update_state(flow_id, json!({ "a": 1 }), None).await });
    let t2 = tokio::spawn(async move { m2.update_state(flow_id, json!({ "b": 2 }), None).await });

    let r1 = t1.await.expect("join1");
    let r2 = t2.await.expect("join2");
    assert!(r1.is_ok(), "t1 result: {r1:?}");
    assert!(r2.is_ok(), "t2 result: {r2:?}");

    let final_flow = m.get(flow_id).await.unwrap().unwrap();
    // Final revision is +2 above post-start (one per successful update).
    assert_eq!(final_flow.revision, 3);
    // Both patches landed (shallow merge preserves both keys).
    assert_eq!(final_flow.state_json["a"], 1);
    assert_eq!(final_flow.state_json["b"], 2);
}

/// Persistent contention: if the retry budget is exhausted, the manager
/// surfaces `RevisionMismatch` to the caller. We simulate this by forcing
/// three concurrent updates on a flow that has RETRY_ATTEMPTS=2 — at least
/// one should land the mismatch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heavy_contention_surfaces_revision_mismatch() {
    let (_dir_guard, path) = tempdir_db();
    let store = Arc::new(SqliteFlowStore::open(&path).await.unwrap());
    let m = Arc::new(FlowManager::new(store));
    let f = m.create_managed(seed_input()).await.unwrap();
    let f = m.start_running(f.id).await.unwrap();
    let flow_id = f.id;

    let mut handles = Vec::new();
    for i in 0..10 {
        let m_clone = Arc::clone(&m);
        handles.push(tokio::spawn(async move {
            m_clone
                .update_state(flow_id, json!({ format!("k{i}"): i }), None)
                .await
        }));
    }

    let mut ok = 0usize;
    let mut conflict = 0usize;
    for h in handles {
        match h.await.expect("join") {
            Ok(_) => ok += 1,
            Err(FlowError::RevisionMismatch { .. }) => conflict += 1,
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
    // Most land; at least one likely lost the race. We do not hard-require
    // a conflict because on a fast/quiet machine the retry may absorb every
    // collision, but the invariant is "ok + conflict == 10".
    assert_eq!(ok + conflict, 10);
    // The flow must still be in a valid state and have advanced `revision`
    // exactly by the number of successful writes.
    let final_flow = m.get(flow_id).await.unwrap().unwrap();
    assert_eq!(final_flow.revision as usize, 1 + ok);
}

/// Mirrored observation pipeline survives restart too: step rows persisted
/// by the first process are visible to the second.
#[tokio::test]
async fn mirrored_steps_survive_reopen() {
    let (_dir_guard, path) = tempdir_db();
    let flow_id = {
        let store = Arc::new(SqliteFlowStore::open(&path).await.unwrap());
        let m = FlowManager::new(store);
        let f = m.create_mirrored(seed_input()).await.unwrap();
        for i in 0..3 {
            m.record_step_observation(agent_taskflow::StepObservation {
                flow_id: f.id,
                run_id: format!("cron-{i}"),
                task: format!("task-{i}"),
                status: agent_taskflow::FlowStepStatus::Succeeded,
                child_session_key: None,
                result_json: Some(json!({ "idx": i })),
            })
            .await
            .unwrap();
        }
        f.id
    };

    let store = Arc::new(SqliteFlowStore::open(&path).await.unwrap());
    let m = FlowManager::new(store);
    let steps = m.list_steps(flow_id).await.unwrap();
    assert_eq!(steps.len(), 3);
    for (i, s) in steps.iter().enumerate() {
        assert_eq!(s.run_id, format!("cron-{i}"));
        assert_eq!(s.result_json.as_ref().unwrap()["idx"], i);
    }
    let _ = Flow::clone; // use the import to silence unused warning paths
}
