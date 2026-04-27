//! Phase 79.1 — verify the plan-mode JSON survives a simulated daemon
//! restart: write the row → close the SQLite pool → reopen the same
//! file → reattach → expect the same `plan_mode` JSON to come back on
//! the resumed AgentHandle.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use nexo_agent_registry::{
    reattach, AgentHandle, AgentRegistry, AgentRegistryStore, AgentRunStatus, AgentSnapshot,
    ReattachOptions, ReattachOutcome, SqliteAgentRegistryStore,
};
use nexo_driver_types::GoalId;
use uuid::Uuid;

fn tmp_db_path() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "nexo-plan-mode-reattach-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

const PLAN_ON_JSON: &str = r#"{"state":"on","entered_at":1700000000,"reason":{"kind":"model_requested","reason":"explore auth flow"},"prior_mode":"default"}"#;

fn running_with_plan_mode(plan_mode: Option<String>) -> AgentHandle {
    AgentHandle {
        goal_id: GoalId(Uuid::new_v4()),
        phase_id: "79.1.test".into(),
        status: AgentRunStatus::Running,
        origin: None,
        dispatcher: None,
        started_at: Utc::now(),
        finished_at: None,
        snapshot: AgentSnapshot::default(),
        plan_mode,
    }
}

#[tokio::test]
async fn plan_mode_on_round_trips_across_restart() {
    let path = tmp_db_path();
    let path_str = path.to_string_lossy().into_owned();

    // ── Daemon "instance #1" — write the row, then drop the store
    //    so the SQLite pool closes (mimics a clean shutdown).
    let goal_id = {
        let store = Arc::new(SqliteAgentRegistryStore::open(&path_str).await.unwrap());
        let handle = running_with_plan_mode(Some(PLAN_ON_JSON.to_string()));
        let id = handle.goal_id;
        store.upsert(&handle).await.unwrap();
        // Drop is implicit at end of scope — just being explicit.
        drop(store);
        id
    };

    // ── Daemon "instance #2" — reopen the same file, run reattach,
    //    confirm the row comes back as Resume with plan_mode preserved.
    let store = Arc::new(SqliteAgentRegistryStore::open(&path_str).await.unwrap());
    let registry = AgentRegistry::new(store.clone(), 8);
    let outcomes = reattach(
        &registry,
        store.clone(),
        ReattachOptions {
            resume_running: true,
            keep_terminal_for: Duration::from_secs(60),
        },
    )
    .await
    .unwrap();

    assert_eq!(outcomes.len(), 1, "expected exactly one row");
    match &outcomes[0] {
        ReattachOutcome::Resume(handle) => {
            assert_eq!(handle.goal_id, goal_id);
            assert_eq!(
                handle.plan_mode.as_deref(),
                Some(PLAN_ON_JSON),
                "plan_mode JSON did not survive the simulated restart"
            );
        }
        other => panic!("expected Resume, got {other:?}"),
    }

    // Cleanup.
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn plan_mode_off_stays_none_across_restart() {
    let path = tmp_db_path();
    let path_str = path.to_string_lossy().into_owned();

    let goal_id = {
        let store = Arc::new(SqliteAgentRegistryStore::open(&path_str).await.unwrap());
        let handle = running_with_plan_mode(None);
        let id = handle.goal_id;
        store.upsert(&handle).await.unwrap();
        drop(store);
        id
    };

    let store = Arc::new(SqliteAgentRegistryStore::open(&path_str).await.unwrap());
    let registry = AgentRegistry::new(store.clone(), 8);
    let outcomes = reattach(
        &registry,
        store.clone(),
        ReattachOptions {
            resume_running: true,
            keep_terminal_for: Duration::from_secs(60),
        },
    )
    .await
    .unwrap();

    match &outcomes[0] {
        ReattachOutcome::Resume(handle) => {
            assert_eq!(handle.goal_id, goal_id);
            assert!(
                handle.plan_mode.is_none(),
                "plan_mode should stay None when never set"
            );
        }
        other => panic!("expected Resume, got {other:?}"),
    }

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn plan_mode_marked_lost_when_resume_disabled_keeps_field() {
    let path = tmp_db_path();
    let path_str = path.to_string_lossy().into_owned();

    {
        let store = Arc::new(SqliteAgentRegistryStore::open(&path_str).await.unwrap());
        store
            .upsert(&running_with_plan_mode(Some(PLAN_ON_JSON.to_string())))
            .await
            .unwrap();
    }

    let store = Arc::new(SqliteAgentRegistryStore::open(&path_str).await.unwrap());
    let registry = AgentRegistry::new(store.clone(), 8);
    let outcomes = reattach(
        &registry,
        store.clone(),
        ReattachOptions {
            resume_running: false, // operator disabled reattach
            keep_terminal_for: Duration::from_secs(60),
        },
    )
    .await
    .unwrap();

    match &outcomes[0] {
        ReattachOutcome::MarkedLost(handle) => {
            // Even when reattach flips the status to LostOnRestart,
            // the plan_mode field is preserved in the persisted row
            // so a follow-up "what happened" query can show that the
            // goal was mid-plan-mode at the time of the crash.
            assert_eq!(handle.plan_mode.as_deref(), Some(PLAN_ON_JSON));
            assert_eq!(handle.status, AgentRunStatus::LostOnRestart);
        }
        other => panic!("expected MarkedLost, got {other:?}"),
    }

    let _ = std::fs::remove_file(&path);
}
