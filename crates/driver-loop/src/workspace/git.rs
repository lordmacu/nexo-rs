//! Thin shell-out helpers around `git`. Each helper wraps a single
//! command, sets `GIT_AUTHOR/COMMITTER` env so commits don't fail
//! when the operator's repo lacks `user.email`, and maps non-zero
//! exits + timeouts to `DriverError::Workspace`.

use std::path::Path;
use std::time::Duration;

use crate::acceptance::ShellRunner;
use crate::error::DriverError;

const GIT_AUTHOR_ENV: &str = "GIT_AUTHOR_NAME=nexo-driver \
                              GIT_AUTHOR_EMAIL=nexo-driver@localhost \
                              GIT_COMMITTER_NAME=nexo-driver \
                              GIT_COMMITTER_EMAIL=nexo-driver@localhost";

const DEFAULT_GIT_TIMEOUT: Duration = Duration::from_secs(30);

#[allow(dead_code)] // 67.x will use this; tests already exercise it.
pub(crate) async fn is_repo(shell: &ShellRunner, path: &Path) -> bool {
    let res = shell
        .run(
            "git rev-parse --is-inside-work-tree 2>/dev/null",
            path,
            DEFAULT_GIT_TIMEOUT,
        )
        .await;
    matches!(res, Ok(r) if r.exit_code == Some(0) && r.stdout.trim() == "true")
}

pub(crate) async fn worktree_add(
    shell: &ShellRunner,
    source_repo: &Path,
    branch: &str,
    target: &Path,
    base_ref: &str,
) -> Result<(), DriverError> {
    let cmd = format!(
        "git -C {src} worktree add --quiet -B {branch} {target} {base}",
        src = quote(&source_repo.display().to_string()),
        branch = quote(branch),
        target = quote(&target.display().to_string()),
        base = quote(base_ref),
    );
    let res = shell.run(&cmd, source_repo, DEFAULT_GIT_TIMEOUT).await?;
    if res.timed_out || res.exit_code != Some(0) {
        return Err(DriverError::Workspace(format!(
            "git worktree add failed (exit {:?}): {}",
            res.exit_code,
            res.stderr.trim()
        )));
    }
    Ok(())
}

pub(crate) async fn worktree_remove(
    shell: &ShellRunner,
    source_repo: &Path,
    target: &Path,
) -> Result<(), DriverError> {
    let cmd = format!(
        "git -C {src} worktree remove --force {target} 2>&1 || true",
        src = quote(&source_repo.display().to_string()),
        target = quote(&target.display().to_string()),
    );
    // Best-effort: never fail.
    let _ = shell.run(&cmd, source_repo, DEFAULT_GIT_TIMEOUT).await;
    Ok(())
}

pub(crate) async fn commit_all_with_label(
    shell: &ShellRunner,
    workspace: &Path,
    label: &str,
) -> Result<String, DriverError> {
    let cmd = format!(
        "{env} git add -A && {env} git commit -q --allow-empty -m {msg} && git rev-parse HEAD",
        env = GIT_AUTHOR_ENV,
        msg = quote(label),
    );
    let res = shell.run(&cmd, workspace, DEFAULT_GIT_TIMEOUT).await?;
    if res.timed_out || res.exit_code != Some(0) {
        return Err(DriverError::Workspace(format!(
            "git commit failed (exit {:?}): {}",
            res.exit_code,
            res.stderr.trim()
        )));
    }
    Ok(res.stdout.trim().to_string())
}

pub(crate) async fn reset_hard(
    shell: &ShellRunner,
    workspace: &Path,
    sha: &str,
) -> Result<(), DriverError> {
    let cmd = format!("git reset --hard {sha} 2>&1", sha = quote(sha));
    let res = shell.run(&cmd, workspace, DEFAULT_GIT_TIMEOUT).await?;
    if res.timed_out || res.exit_code != Some(0) {
        return Err(DriverError::Workspace(format!(
            "git reset failed (exit {:?}): {}",
            res.exit_code,
            res.stdout.trim()
        )));
    }
    Ok(())
}

pub(crate) async fn diff_stat(
    shell: &ShellRunner,
    workspace: &Path,
    since_sha: &str,
) -> Result<String, DriverError> {
    let cmd = format!("git diff --stat {sha}..HEAD", sha = quote(since_sha));
    let res = shell.run(&cmd, workspace, DEFAULT_GIT_TIMEOUT).await?;
    if res.timed_out || res.exit_code != Some(0) {
        return Ok(String::new());
    }
    Ok(res.stdout)
}

/// POSIX shell single-quoting. Embedded quotes get the standard
/// `'\''` dance.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        which::which("git").is_ok()
    }

    #[tokio::test]
    async fn quote_escapes_single_quotes() {
        assert_eq!(quote("hi"), "'hi'");
        assert_eq!(quote("it's"), "'it'\\''s'");
    }

    #[tokio::test]
    async fn is_repo_false_outside_git() {
        let dir = tempfile::tempdir().unwrap();
        let shell = ShellRunner::default();
        assert!(!is_repo(&shell, dir.path()).await);
    }

    #[tokio::test]
    async fn is_repo_true_inside_git() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let shell = ShellRunner::default();
        let res = shell
            .run("git init -q", dir.path(), DEFAULT_GIT_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(res.exit_code, Some(0));
        assert!(is_repo(&shell, dir.path()).await);
    }

    #[tokio::test]
    async fn commit_all_returns_40_hex_sha() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let shell = ShellRunner::default();
        shell
            .run("git init -q", dir.path(), DEFAULT_GIT_TIMEOUT)
            .await
            .unwrap();
        let sha = commit_all_with_label(&shell, dir.path(), "first")
            .await
            .unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
