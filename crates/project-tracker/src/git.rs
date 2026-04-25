//! `git log --grep=<phase_id>` lookup behind a circuit breaker.
//!
//! The tracker tools quote shipped commits per phase; the lookup
//! shells out to `git`, which is fast in the happy path but can hang
//! on a corrupted repo or a slow filesystem. We wrap it in a
//! resilience::CircuitBreaker so a stuck `git` doesn't take down
//! every `project_status` query, and we cap the call with an explicit
//! tokio timeout so a hung process can't outlive its turn.

use std::path::Path;
use std::time::Duration;

use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig, CircuitError};
use serde::Serialize;
use tokio::process::Command;

/// One commit row.
#[derive(Clone, Debug, Serialize)]
pub struct CommitRow {
    pub sha: String,
    pub subject: String,
    pub date: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git unavailable (circuit open: {0})")]
    CircuitOpen(String),
    #[error("git timeout after {0:?}")]
    Timeout(Duration),
    #[error("git exited {code:?}: {stderr}")]
    NonZero { code: Option<i32>, stderr: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Reads `git log` for a phase id, applying timeout + breaker. The
/// `phase_id` flows into `--grep="Phase <id>"` directly, so callers
/// must not pass untrusted input — but the tracker only ever feeds
/// known ids parsed out of `PHASES.md`.
pub struct GitLogReader {
    breaker: CircuitBreaker,
    timeout: Duration,
    max_commits: usize,
}

impl Default for GitLogReader {
    fn default() -> Self {
        Self::new(Duration::from_secs(5), 5)
    }
}

impl GitLogReader {
    pub fn new(timeout: Duration, max_commits: usize) -> Self {
        let cfg = CircuitBreakerConfig {
            failure_threshold: 3,
            success_threshold: 1,
            initial_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(60),
        };
        Self {
            breaker: CircuitBreaker::new("project-tracker.git_log", cfg),
            timeout,
            max_commits,
        }
    }

    pub async fn for_phase(&self, repo: &Path, phase_id: &str) -> Result<Vec<CommitRow>, GitError> {
        let timeout = self.timeout;
        let max = self.max_commits;
        let repo = repo.to_path_buf();
        let phase_id = phase_id.to_string();

        let result = self
            .breaker
            .call(|| async move { run_git(&repo, &phase_id, max, timeout).await })
            .await;

        match result {
            Ok(rows) => Ok(rows),
            Err(CircuitError::Open(name)) => Err(GitError::CircuitOpen(name)),
            Err(CircuitError::Inner(e)) => Err(e),
        }
    }
}

async fn run_git(
    repo: &Path,
    phase_id: &str,
    max: usize,
    timeout: Duration,
) -> Result<Vec<CommitRow>, GitError> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .arg("log")
        .arg(format!("--grep=Phase {phase_id}"))
        .arg(format!("--max-count={max}"))
        .arg("--pretty=format:%h%x09%cI%x09%s");

    let fut = cmd.output();
    let out = tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| GitError::Timeout(timeout))??;

    if !out.status.success() {
        return Err(GitError::NonZero {
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = stdout
        .lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\t');
            let sha = parts.next()?.to_string();
            let date = parts.next()?.to_string();
            let subject = parts.next()?.to_string();
            Some(CommitRow { sha, date, subject })
        })
        .collect();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repo guaranteed to have a commit referencing "Phase 67.9".
    fn workspace_root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[tokio::test]
    async fn finds_phase_67_9_commit() {
        let r = GitLogReader::default();
        let rows = match r.for_phase(&workspace_root(), "67.9").await {
            Ok(v) => v,
            Err(GitError::Io(_)) | Err(GitError::CircuitOpen(_)) => {
                // git binary missing or sandboxed — skip without
                // failing the test.
                return;
            }
            Err(e) => panic!("unexpected error: {e}"),
        };
        assert!(
            rows.iter().any(|r| r.subject.contains("67.9")),
            "expected at least one row mentioning 67.9, got {:?}",
            rows
        );
    }

    #[tokio::test]
    async fn nonexistent_phase_returns_empty() {
        let r = GitLogReader::default();
        let rows = match r.for_phase(&workspace_root(), "9999.999").await {
            Ok(v) => v,
            Err(GitError::Io(_)) | Err(GitError::CircuitOpen(_)) => return,
            Err(e) => panic!("unexpected error: {e}"),
        };
        assert!(rows.is_empty());
    }
}
