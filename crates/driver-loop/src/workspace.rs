//! Resolve / mkdir / cleanup of a goal's workspace dir.

use std::path::{Path, PathBuf};

use nexo_driver_types::Goal;

use crate::error::DriverError;

pub struct WorkspaceManager {
    root: PathBuf,
}

impl WorkspaceManager {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve the goal's workspace path, mkdir it, and verify it
    /// stays inside `root` after canonicalisation. Returns the
    /// absolute path the harness will `cwd` into.
    pub async fn ensure(&self, goal: &Goal) -> Result<PathBuf, DriverError> {
        // 1. mkdir root unconditionally so canonicalize() works.
        tokio::fs::create_dir_all(&self.root).await?;
        let canonical_root = tokio::fs::canonicalize(&self.root).await?;

        // 2. Pick the desired path.
        let candidate = match &goal.workspace {
            Some(p) => PathBuf::from(p),
            None => canonical_root.join(goal.id.0.to_string()),
        };

        // 3. mkdir the candidate so canonicalize() works on a fresh path.
        tokio::fs::create_dir_all(&candidate).await?;
        let canonical = tokio::fs::canonicalize(&candidate).await?;

        // 4. Path-traversal guard.
        if !canonical.starts_with(&canonical_root) {
            return Err(DriverError::WorkspaceTraversal {
                path: canonical.display().to_string(),
            });
        }
        Ok(canonical)
    }

    /// Best-effort recursive remove. Returns `Ok(())` on `NotFound`.
    pub async fn cleanup(&self, path: &Path) -> Result<(), DriverError> {
        match tokio::fs::remove_dir_all(path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DriverError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_driver_types::{AcceptanceCriterion, BudgetGuards, Goal, GoalId};
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
            },
            workspace,
            metadata: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn ensure_creates_default_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = WorkspaceManager::new(dir.path());
        let g = goal(None);
        let path = mgr.ensure(&g).await.unwrap();
        assert!(path.is_dir());
        assert!(path.starts_with(dir.path().canonicalize().unwrap()));
    }

    #[tokio::test]
    async fn ensure_rejects_path_traversal() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mgr = WorkspaceManager::new(root.path());
        let g = goal(Some(outside.path().display().to_string()));
        let err = mgr.ensure(&g).await.unwrap_err();
        assert!(matches!(err, DriverError::WorkspaceTraversal { .. }));
    }

    #[tokio::test]
    async fn cleanup_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = WorkspaceManager::new(dir.path());
        let nonexistent = dir.path().join("does/not/exist");
        // First call: NotFound — must succeed.
        mgr.cleanup(&nonexistent).await.unwrap();
        // Make a real subdir then nuke it twice.
        let sub = dir.path().join("sub");
        tokio::fs::create_dir_all(&sub).await.unwrap();
        mgr.cleanup(&sub).await.unwrap();
        mgr.cleanup(&sub).await.unwrap();
    }
}
