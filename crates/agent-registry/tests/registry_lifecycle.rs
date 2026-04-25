//! Phase 67.B.2 — store-level lifecycle test. The registry façade
//! that wraps these calls lands in 67.B.3; this test pins the
//! contract of the persistence layer in isolation so future cap /
//! queue changes don't accidentally rewrite the store semantics.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use nexo_agent_registry::{
    AgentHandle, AgentRegistryStore, AgentRunStatus, AgentSnapshot, MemoryAgentRegistryStore,
    SqliteAgentRegistryStore,
};
use nexo_driver_types::GoalId;
use uuid::Uuid;

fn handle(phase: &str, status: AgentRunStatus) -> AgentHandle {
    AgentHandle {
        goal_id: GoalId(Uuid::new_v4()),
        phase_id: phase.into(),
        status,
        origin: None,
        dispatcher: None,
        started_at: Utc::now(),
        finished_at: None,
        snapshot: AgentSnapshot::default(),
    }
}

fn tmp_db_path() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "nexo-agent-registry-{}-{}.db",
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
async fn memory_store_round_trip() {
    let store = MemoryAgentRegistryStore::default();
    let h = handle("67.10", AgentRunStatus::Running);
    let id = h.goal_id;
    store.upsert(&h).await.unwrap();
    let read = store.get(id).await.unwrap().unwrap();
    assert_eq!(read.phase_id, "67.10");
    assert_eq!(read.status, AgentRunStatus::Running);

    let list = store.list().await.unwrap();
    assert_eq!(list.len(), 1);
    let by_status = store.list_by_status(AgentRunStatus::Running).await.unwrap();
    assert_eq!(by_status.len(), 1);

    store.remove(id).await.unwrap();
    assert!(store.get(id).await.unwrap().is_none());
}

#[tokio::test]
async fn sqlite_store_persists_across_pool_close() {
    let path = tmp_db_path();
    let path_str = path.to_string_lossy().into_owned();

    // Open, write, drop the store (and its pool), reopen — row must
    // still be there.
    {
        let store = SqliteAgentRegistryStore::open(&path_str).await.unwrap();
        let h = handle("67.10", AgentRunStatus::Running);
        store.upsert(&h).await.unwrap();
    }
    let store2 = SqliteAgentRegistryStore::open(&path_str).await.unwrap();
    let list = store2.list().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].phase_id, "67.10");

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn list_by_status_filters_correctly() {
    let store = SqliteAgentRegistryStore::open_memory().await.unwrap();
    store
        .upsert(&handle("67.10", AgentRunStatus::Running))
        .await
        .unwrap();
    store
        .upsert(&handle("67.11", AgentRunStatus::Queued))
        .await
        .unwrap();
    let mut done = handle("67.12", AgentRunStatus::Done);
    done.finished_at = Some(Utc::now());
    store.upsert(&done).await.unwrap();

    let running = store.list_by_status(AgentRunStatus::Running).await.unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].phase_id, "67.10");
    let queued = store.list_by_status(AgentRunStatus::Queued).await.unwrap();
    assert_eq!(queued.len(), 1);
    let dn = store.list_by_status(AgentRunStatus::Done).await.unwrap();
    assert_eq!(dn.len(), 1);
}

#[tokio::test]
async fn evict_only_drops_terminal_old_rows() {
    let store = SqliteAgentRegistryStore::open_memory().await.unwrap();

    // Active row — must NOT be evicted.
    store
        .upsert(&handle("67.10", AgentRunStatus::Running))
        .await
        .unwrap();

    // Terminal but recent — must NOT be evicted.
    let mut recent = handle("67.11", AgentRunStatus::Done);
    recent.finished_at = Some(Utc::now());
    store.upsert(&recent).await.unwrap();

    // Terminal and old — must be evicted.
    let mut old = handle("67.12", AgentRunStatus::Failed);
    old.finished_at = Some(Utc::now() - ChronoDuration::days(30));
    store.upsert(&old).await.unwrap();

    let cutoff = Utc::now() - ChronoDuration::days(7);
    let n = store.evict_terminal_older_than(cutoff).await.unwrap();
    assert_eq!(n, 1);

    let remaining = store.list().await.unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().all(|h| h.phase_id != "67.12"));
}

#[tokio::test]
async fn upsert_overwrites_status_and_finished_at() {
    let store = SqliteAgentRegistryStore::open_memory().await.unwrap();
    let mut h = handle("67.10", AgentRunStatus::Running);
    let id = h.goal_id;
    store.upsert(&h).await.unwrap();

    h.status = AgentRunStatus::Done;
    h.finished_at = Some(Utc::now());
    store.upsert(&h).await.unwrap();

    let read = store.get(id).await.unwrap().unwrap();
    assert_eq!(read.status, AgentRunStatus::Done);
    assert!(read.finished_at.is_some());
    // elapsed() consults finished_at when present.
    assert!(read.elapsed() < Duration::from_secs(60));
}
