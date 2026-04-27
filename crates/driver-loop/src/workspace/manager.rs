//! `WorkspaceManager` — resolve, mkdir, optionally checkout into a
//! git worktree, and provide checkpoint / rollback / diff_stat
//! helpers used by the driver loop.

use std::path::{Path, PathBuf};

use nexo_driver_types::Goal;
use regex::Regex;

use crate::acceptance::ShellRunner;
use crate::error::DriverError;
use crate::workspace::git;

#[derive(Clone, Debug)]
pub enum GitWorktreeMode {
    Disabled,
    SourceRepo { path: PathBuf, base_ref: String },
}

pub struct WorkspaceManager {
    root: PathBuf,
    git: GitWorktreeMode,
    shell: ShellRunner,
}

impl WorkspaceManager {
    /// Sentinel returned by `checkpoint` when git mode is `Disabled`.
    pub const NO_GIT_SENTINEL: &'static str = "<no-git>";

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            git: GitWorktreeMode::Disabled,
            shell: ShellRunner::default(),
        }
    }

    pub fn with_git(mut self, mode: GitWorktreeMode) -> Self {
        self.git = mode;
        self
    }

    pub fn with_shell(mut self, shell: ShellRunner) -> Self {
        self.shell = shell;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git_mode(&self) -> &GitWorktreeMode {
        &self.git
    }

    /// Resolve the goal's workspace path, mkdir/worktree-add it, and
    /// verify it stays inside `root`. Returns the absolute path the
    /// harness will `cwd` into.
    ///
    /// In `SourceRepo` mode, `goal.workspace` (operator-supplied) is
    /// IGNORED — we always use `<root>/<goal_id>` so the worktree
    /// branch name (`nexo-driver/<goal_id>`) lines up with the path.
    pub async fn ensure(&self, goal: &Goal) -> Result<PathBuf, DriverError> {
        tokio::fs::create_dir_all(&self.root).await?;
        let canonical_root = tokio::fs::canonicalize(&self.root).await?;

        // Phase 76 — per-goal source repo override. When
        // `program_phase_dispatch` detects the active tracker is a
        // standalone git repo (typical after `init_project`), it
        // stamps `goal.metadata["worktree.source_repo"]` with that
        // path. The override takes precedence over `self.git` so
        // the per-goal worktree clones the right repo even when
        // the daemon was booted against a different one.
        let override_source: Option<PathBuf> = goal
            .metadata
            .get("worktree.source_repo")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.join(".git").exists());

        let resolved = if let Some(repo) = override_source {
            // Use the operator-provided source repo with the same
            // base_ref the boot config picked, falling back to a
            // sane default when the boot mode is `Disabled`.
            let base_ref = match &self.git {
                GitWorktreeMode::SourceRepo { base_ref, .. } => base_ref.clone(),
                GitWorktreeMode::Disabled => "HEAD".to_string(),
            };
            GitWorktreeMode::SourceRepo {
                path: repo,
                base_ref,
            }
        } else {
            self.git.clone()
        };

        match &resolved {
            GitWorktreeMode::Disabled => {
                let candidate = match &goal.workspace {
                    Some(p) => PathBuf::from(p),
                    None => canonical_root.join(goal.id.0.to_string()),
                };
                tokio::fs::create_dir_all(&candidate).await?;
                let canonical = tokio::fs::canonicalize(&candidate).await?;
                if !canonical.starts_with(&canonical_root) {
                    return Err(DriverError::WorkspaceTraversal {
                        path: canonical.display().to_string(),
                    });
                }
                Ok(canonical)
            }
            GitWorktreeMode::SourceRepo { path, base_ref } => {
                let target = canonical_root.join(goal.id.0.to_string());
                let branch = format!("nexo-driver/{}", goal.id.0);
                let parent = target.parent().unwrap_or(&canonical_root);
                tokio::fs::create_dir_all(parent).await?;
                // If target already exists and is a worktree, the
                // `worktree add -B` form moves the branch pointer
                // back to base_ref while keeping the worktree.
                git::worktree_add(&self.shell, path, &branch, &target, base_ref).await?;
                let canonical = tokio::fs::canonicalize(&target).await?;
                if !canonical.starts_with(&canonical_root) {
                    return Err(DriverError::WorkspaceTraversal {
                        path: canonical.display().to_string(),
                    });
                }
                Ok(canonical)
            }
        }
    }

    /// Best-effort recursive remove. In `SourceRepo` mode, also
    /// unregisters the worktree.
    pub async fn cleanup(&self, path: &Path) -> Result<(), DriverError> {
        if let GitWorktreeMode::SourceRepo {
            path: source_repo, ..
        } = &self.git
        {
            let _ = git::worktree_remove(&self.shell, source_repo, path).await;
        }
        match tokio::fs::remove_dir_all(path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DriverError::Io(e)),
        }
    }

    pub async fn checkpoint(&self, workspace: &Path, label: &str) -> Result<String, DriverError> {
        match &self.git {
            GitWorktreeMode::Disabled => Ok(Self::NO_GIT_SENTINEL.to_string()),
            GitWorktreeMode::SourceRepo { .. } => {
                git::commit_all_with_label(&self.shell, workspace, label).await
            }
        }
    }

    pub async fn rollback(&self, workspace: &Path, sha: &str) -> Result<(), DriverError> {
        match &self.git {
            GitWorktreeMode::Disabled => Ok(()),
            GitWorktreeMode::SourceRepo { .. } => {
                if !is_valid_sha(sha) {
                    return Err(DriverError::Workspace(format!(
                        "rollback: sha {sha:?} is not 7..=40 hex chars"
                    )));
                }
                git::reset_hard(&self.shell, workspace, sha).await
            }
        }
    }

    pub async fn diff_stat(
        &self,
        workspace: &Path,
        since_sha: &str,
    ) -> Result<String, DriverError> {
        match &self.git {
            GitWorktreeMode::Disabled => Ok(String::new()),
            GitWorktreeMode::SourceRepo { .. } => {
                if since_sha == Self::NO_GIT_SENTINEL || !is_valid_sha(since_sha) {
                    return Ok(String::new());
                }
                let raw = git::diff_stat(&self.shell, workspace, since_sha).await?;
                Ok(truncate_to(&raw, 1024))
            }
        }
    }
}

fn is_valid_sha(s: &str) -> bool {
    static_re().is_match(s)
}

fn static_re() -> &'static Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[0-9a-fA-F]{7,40}$").unwrap())
}

fn truncate_to(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut end = limit;
    while end < s.len() && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str("\n... (truncated)");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_driver_types::{AcceptanceCriterion, BudgetGuards, GoalId};
    use std::time::Duration;
    use uuid::Uuid;

    fn goal(workspace: Option<String>) -> Goal {
        Goal {
            id: GoalId(Uuid::new_v4()),
            description: "test".into(),
            acceptance: vec![AcceptanceCriterion::shell("true")],
            budget: BudgetGuards {
                max_turns: 1,
                max_wall_time: Duration::from_secs(60),
                max_tokens: 1_000,
                max_consecutive_denies: 1,
                max_consecutive_errors: 5,
            },
            workspace,
            metadata: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn ensure_disabled_creates_default_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = WorkspaceManager::new(dir.path());
        let g = goal(None);
        let path = mgr.ensure(&g).await.unwrap();
        assert!(path.is_dir());
        assert!(path.starts_with(dir.path().canonicalize().unwrap()));
    }

    #[tokio::test]
    async fn ensure_disabled_rejects_path_traversal() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mgr = WorkspaceManager::new(root.path());
        let g = goal(Some(outside.path().display().to_string()));
        let err = mgr.ensure(&g).await.unwrap_err();
        assert!(matches!(err, DriverError::WorkspaceTraversal { .. }));
    }

    #[tokio::test]
    async fn cleanup_disabled_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = WorkspaceManager::new(dir.path());
        let nonexistent = dir.path().join("does/not/exist");
        mgr.cleanup(&nonexistent).await.unwrap();
        let sub = dir.path().join("sub");
        tokio::fs::create_dir_all(&sub).await.unwrap();
        mgr.cleanup(&sub).await.unwrap();
        mgr.cleanup(&sub).await.unwrap();
    }

    #[tokio::test]
    async fn disabled_checkpoint_returns_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = WorkspaceManager::new(dir.path());
        let sha = mgr.checkpoint(dir.path(), "x").await.unwrap();
        assert_eq!(sha, WorkspaceManager::NO_GIT_SENTINEL);
    }

    #[tokio::test]
    async fn disabled_rollback_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = WorkspaceManager::new(dir.path());
        // Even with a bogus sha; Disabled mode short-circuits.
        mgr.rollback(dir.path(), "deadbeef").await.unwrap();
    }

    #[tokio::test]
    async fn invalid_sha_in_sourcerepo_mode_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = WorkspaceManager::new(dir.path()).with_git(GitWorktreeMode::SourceRepo {
            path: dir.path().to_path_buf(),
            base_ref: "HEAD".into(),
        });
        let err = mgr.rollback(dir.path(), "zzz").await.unwrap_err();
        assert!(matches!(err, DriverError::Workspace(_)));
    }

    #[tokio::test]
    async fn truncate_to_appends_marker() {
        let s: String = "x".repeat(2000);
        let out = truncate_to(&s, 100);
        assert!(out.starts_with(&"x".repeat(100)));
        assert!(out.contains("(truncated)"));
    }
}
