//! End-to-end git-worktree tests. Skipped when `git` isn't on PATH
//! so minimal CI images still pass.

#![cfg(unix)]

use std::path::Path;
use std::time::Duration;

use nexo_driver_loop::{GitWorktreeMode, ShellRunner, WorkspaceManager};
use nexo_driver_types::{AcceptanceCriterion, BudgetGuards, Goal, GoalId};
use uuid::Uuid;

fn git_available() -> bool {
    which::which("git").is_ok()
}

async fn sh(cmd: &str, cwd: &Path) {
    let r = ShellRunner::default()
        .run(cmd, cwd, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(r.exit_code, Some(0), "{cmd} failed: {:?}", r);
}

async fn make_source_repo(dir: &Path) {
    sh(
        "git init -q && git config user.email t@t && git config user.name t \
         && echo seed > seed.txt && git add -A && git commit -qm baseline",
        dir,
    )
    .await;
}

fn goal_with_id(id: uuid::Uuid) -> Goal {
    Goal {
        id: GoalId(id),
        description: "test".into(),
        acceptance: vec![AcceptanceCriterion::shell("true")],
        budget: BudgetGuards {
            max_turns: 1,
            max_wall_time: Duration::from_secs(30),
            max_tokens: 100,
            max_consecutive_denies: 1,
            max_consecutive_errors: 5,
            max_consecutive_413: 2,
        },
        workspace: None,
        metadata: serde_json::Map::new(),
    }
}

#[tokio::test]
async fn ensure_creates_worktree_with_branch() {
    if !git_available() {
        return;
    }
    let source = tempfile::tempdir().unwrap();
    make_source_repo(source.path()).await;
    let workspaces = tempfile::tempdir().unwrap();
    let mgr = WorkspaceManager::new(workspaces.path()).with_git(GitWorktreeMode::SourceRepo {
        path: source.path().to_path_buf(),
        base_ref: "HEAD".into(),
    });
    let g = goal_with_id(Uuid::new_v4());
    let path = mgr.ensure(&g).await.unwrap();
    assert!(path.is_dir());
    // Worktree directories have a `.git` FILE (not dir).
    let dotgit = path.join(".git");
    assert!(dotgit.exists());
    let meta = std::fs::metadata(&dotgit).unwrap();
    assert!(meta.is_file(), ".git should be a file in a worktree");
    let _ = mgr.cleanup(&path).await;
}

#[tokio::test]
async fn checkpoint_returns_40_hex_sha() {
    if !git_available() {
        return;
    }
    let source = tempfile::tempdir().unwrap();
    make_source_repo(source.path()).await;
    let workspaces = tempfile::tempdir().unwrap();
    let mgr = WorkspaceManager::new(workspaces.path()).with_git(GitWorktreeMode::SourceRepo {
        path: source.path().to_path_buf(),
        base_ref: "HEAD".into(),
    });
    let g = goal_with_id(Uuid::new_v4());
    let path = mgr.ensure(&g).await.unwrap();
    let sha = mgr.checkpoint(&path, "turn-0-pre").await.unwrap();
    assert_eq!(sha.len(), 40, "expected 40-hex sha, got {sha:?}");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    let _ = mgr.cleanup(&path).await;
}

#[tokio::test]
async fn checkpoint_then_rollback_reverts_changes() {
    if !git_available() {
        return;
    }
    let source = tempfile::tempdir().unwrap();
    make_source_repo(source.path()).await;
    let workspaces = tempfile::tempdir().unwrap();
    let mgr = WorkspaceManager::new(workspaces.path()).with_git(GitWorktreeMode::SourceRepo {
        path: source.path().to_path_buf(),
        base_ref: "HEAD".into(),
    });
    let g = goal_with_id(Uuid::new_v4());
    let path = mgr.ensure(&g).await.unwrap();

    let cp = mgr.checkpoint(&path, "before").await.unwrap();

    // Add a brand-new file post-checkpoint.
    tokio::fs::write(path.join("new.txt"), "claude wrote me\n")
        .await
        .unwrap();
    sh("git add -A && git commit -qm \"claude turn\"", &path).await;
    assert!(path.join("new.txt").exists());

    mgr.rollback(&path, &cp).await.unwrap();
    assert!(
        !path.join("new.txt").exists(),
        "rollback failed to remove new.txt"
    );

    let _ = mgr.cleanup(&path).await;
}

#[tokio::test]
async fn cleanup_removes_worktree_registration() {
    if !git_available() {
        return;
    }
    let source = tempfile::tempdir().unwrap();
    make_source_repo(source.path()).await;
    let workspaces = tempfile::tempdir().unwrap();
    let mgr = WorkspaceManager::new(workspaces.path()).with_git(GitWorktreeMode::SourceRepo {
        path: source.path().to_path_buf(),
        base_ref: "HEAD".into(),
    });
    let g = goal_with_id(Uuid::new_v4());
    let path = mgr.ensure(&g).await.unwrap();
    mgr.cleanup(&path).await.unwrap();

    let r = ShellRunner::default()
        .run("git worktree list", source.path(), Duration::from_secs(10))
        .await
        .unwrap();
    assert!(
        !r.stdout.contains(path.to_str().unwrap()),
        "worktree list still shows it:\n{}",
        r.stdout
    );
}
