//! Disabled mode preserves the 67.4 surface bit-for-bit.

use std::time::Duration;

use nexo_driver_loop::WorkspaceManager;
use nexo_driver_types::{AcceptanceCriterion, BudgetGuards, Goal, GoalId};
use uuid::Uuid;

fn goal() -> Goal {
    Goal {
        id: GoalId(Uuid::new_v4()),
        description: "test".into(),
        acceptance: vec![AcceptanceCriterion::shell("true")],
        budget: BudgetGuards {
            max_turns: 1,
            max_wall_time: Duration::from_secs(30),
            max_tokens: 100,
            max_consecutive_denies: 1,
            max_consecutive_errors: 5,
        },
        workspace: None,
        metadata: serde_json::Map::new(),
    }
}

#[tokio::test]
async fn ensure_works_without_git() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = WorkspaceManager::new(dir.path());
    let g = goal();
    let path = mgr.ensure(&g).await.unwrap();
    assert!(path.is_dir());
    assert!(path.starts_with(dir.path().canonicalize().unwrap()));
}

#[tokio::test]
async fn checkpoint_returns_sentinel_in_disabled_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = WorkspaceManager::new(dir.path());
    let sha = mgr.checkpoint(dir.path(), "label").await.unwrap();
    assert_eq!(sha, WorkspaceManager::NO_GIT_SENTINEL);
}

#[tokio::test]
async fn rollback_is_noop_in_disabled_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = WorkspaceManager::new(dir.path());
    // Disabled short-circuits even with bogus sha.
    mgr.rollback(dir.path(), "deadbeef").await.unwrap();
}
