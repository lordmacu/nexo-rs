//! Phase 67.B.4 — when the daemon restarts, reattach() seeds the
//! in-memory registry from the persistent store and applies the
//! configured policy. Running goals come back as Resume; queued
//! goals as Requeued; old terminal rows are evicted.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use nexo_agent_registry::{
    reattach, AgentHandle, AgentRegistry, AgentRegistryStore, AgentRunStatus, AgentSnapshot,
    ReattachOptions, ReattachOutcome, SqliteAgentRegistryStore,
};
use nexo_driver_types::GoalId;
use uuid::Uuid;

fn handle(
    phase: &str,
    status: AgentRunStatus,
    finished_at: Option<chrono::DateTime<Utc>>,
) -> AgentHandle {
    AgentHandle {
        goal_id: GoalId(Uuid::new_v4()),
        phase_id: phase.into(),
        status,
        origin: None,
        dispatcher: None,
        started_at: Utc::now(),
        finished_at,
        snapshot: AgentSnapshot::default(),
        plan_mode: None,
    }
}

fn tmp_db_path() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "nexo-reattach-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

#[tokio::test]
async fn running_with_resume_on_returns_resume_outcome() {
    let path = tmp_db_path();
    let path_str = path.to_string_lossy().into_owned();
    let store = Arc::new(SqliteAgentRegistryStore::open(&path_str).await.unwrap());
    store
        .upsert(&handle("67.10", AgentRunStatus::Running, None))
        .await
        .unwrap();
    store
        .upsert(&handle("67.11", AgentRunStatus::Queued, None))
        .await
        .unwrap();

    // Drop everything; new daemon boot.
    drop(store);
    let store = Arc::new(SqliteAgentRegistryStore::open(&path_str).await.unwrap());
    let reg = AgentRegistry::new(store.clone(), 4);

    let outcomes = reattach(&reg, store.clone(), ReattachOptions::default())
        .await
        .unwrap();
    let resume_count = outcomes
        .iter()
        .filter(|o| matches!(o, ReattachOutcome::Resume(_)))
        .count();
    let requeue_count = outcomes
        .iter()
        .filter(|o| matches!(o, ReattachOutcome::Requeued(_)))
        .count();
    assert_eq!(resume_count, 1);
    assert_eq!(requeue_count, 1);

    // Registry now lists both goals.
    let rows = reg.list().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r.status == AgentRunStatus::Running));
    assert!(rows.iter().any(|r| r.status == AgentRunStatus::Queued));

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn running_with_resume_off_marks_lost() {
    let store = Arc::new(SqliteAgentRegistryStore::open_memory().await.unwrap());
    store
        .upsert(&handle("67.10", AgentRunStatus::Running, None))
        .await
        .unwrap();
    let reg = AgentRegistry::new(store.clone(), 4);

    let opts = ReattachOptions {
        resume_running: false,
        keep_terminal_for: Duration::from_secs(60 * 60 * 24),
    };
    let outcomes = reattach(&reg, store.clone(), opts).await.unwrap();
    assert!(matches!(outcomes[0], ReattachOutcome::MarkedLost(_)));
    let rows = reg.list().await.unwrap();
    assert_eq!(rows[0].status, AgentRunStatus::LostOnRestart);
}

#[tokio::test]
async fn old_terminal_rows_get_skipped_and_evicted() {
    let store = Arc::new(SqliteAgentRegistryStore::open_memory().await.unwrap());
    let recent = handle(
        "recent",
        AgentRunStatus::Done,
        Some(Utc::now() - ChronoDuration::minutes(1)),
    );
    let old = handle(
        "old",
        AgentRunStatus::Failed,
        Some(Utc::now() - ChronoDuration::days(60)),
    );
    let recent_id = recent.goal_id;
    let old_id = old.goal_id;
    store.upsert(&recent).await.unwrap();
    store.upsert(&old).await.unwrap();

    let reg = AgentRegistry::new(store.clone(), 4);
    let opts = ReattachOptions {
        resume_running: true,
        keep_terminal_for: Duration::from_secs(60 * 60 * 24 * 7), // 7d
    };
    let outcomes = reattach(&reg, store.clone(), opts).await.unwrap();

    let skipped: Vec<_> = outcomes
        .iter()
        .filter_map(|o| match o {
            ReattachOutcome::Skipped { goal_id, .. } => Some(*goal_id),
            _ => None,
        })
        .collect();
    assert!(skipped.contains(&old_id));
    assert!(!skipped.contains(&recent_id));

    // Store should no longer have the old row.
    assert!(store.get(old_id).await.unwrap().is_none());
    assert!(store.get(recent_id).await.unwrap().is_some());
}
