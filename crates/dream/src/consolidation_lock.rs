//! Lock file whose mtime IS `lastConsolidatedAt`. Body is the holder's
//! PID. Phase 80.1.
//!
//! Verbatim port of
//! `claude-code-leak/src/services/autoDream/consolidationLock.ts:1-140`.
//!
//! # Design
//!
//! - The lock file path is `<memory_dir>/.consolidate-lock`,
//!   canonicalized at construction so a later symlink swap can't
//!   redirect the lock target.
//! - `mtime` of the file IS the `lastConsolidatedAt` timestamp. One
//!   `stat` per turn — cheaper than reading a separate state file.
//! - Body is the holder's PID. Stale if PID dead OR
//!   `now - mtime >= holder_stale` (default 1h).
//! - `try_acquire`: write our PID, re-read; if PID matches our own →
//!   acquired (returns prior mtime for rollback); else lost the race
//!   (returns `Ok(None)`).
//! - `rollback`: rewind mtime to prior. `prior == 0` → unlink.
//!   Idempotent — kill + fail double-call safe.
//! - **NO heartbeat** — leak doesn't have one. Operator with
//!   sub-1h-but-close-to-1h forks should raise `holder_stale`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nix::sys::signal;
use nix::unistd::Pid;
use tokio::fs;

use crate::error::AutoDreamError;

const LOCK_FILE: &str = ".consolidate-lock";

/// Lock file rooted at `<memory_dir>/.consolidate-lock`. Single
/// instance per `memory_dir` (one fork at a time per binding).
pub struct ConsolidationLock {
    path: PathBuf,
    holder_stale: Duration,
}

impl ConsolidationLock {
    /// Construct, canonicalizing `memory_dir` for symlink defense.
    /// Memory dir MUST exist; caller `mkdir -p` first.
    pub fn new(
        memory_dir: &Path,
        holder_stale: Duration,
    ) -> Result<Self, AutoDreamError> {
        if !memory_dir.exists() {
            return Err(AutoDreamError::Config(format!(
                "memory_dir does not exist: {}",
                memory_dir.display()
            )));
        }
        let canonical = memory_dir.canonicalize()?;
        Ok(Self {
            path: canonical.join(LOCK_FILE),
            holder_stale,
        })
    }

    /// `mtime` of the lock file = `lastConsolidatedAt`. 0 if absent.
    /// Per-turn cost: one stat. Leak `:25-32`.
    pub async fn read_last_consolidated_at(&self) -> i64 {
        match fs::metadata(&self.path).await {
            Ok(meta) => meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            Err(_) => 0,
        }
    }

    /// Read the holder's PID + lock mtime. Returns `(0, 0)` if absent
    /// or unreadable. Used for `RunOutcome::LockBlocked` operator
    /// surface.
    pub async fn read_holder_info(&self) -> (i32, i64) {
        let mtime_ms = self.read_last_consolidated_at().await;
        let pid = match fs::read_to_string(&self.path).await {
            Ok(s) => s.trim().parse::<i32>().unwrap_or(0),
            Err(_) => 0,
        };
        (pid, mtime_ms)
    }

    /// Acquire the lock. Returns `Ok(Some(prior_mtime))` on success
    /// (caller passes prior to `rollback` on failure). Returns
    /// `Ok(None)` when blocked by a live holder OR when we lost the
    /// race against another acquirer.
    ///
    /// Mirror leak `:46-84`:
    /// 1. stat + read body in parallel.
    /// 2. If mtime exists AND `now - mtime < holder_stale` AND PID is
    ///    live → bail `Ok(None)`.
    /// 3. Else: write our PID; re-read; if PID matches → won; else lost.
    pub async fn try_acquire(&self) -> Result<Option<i64>, AutoDreamError> {
        let prior_mtime = self.read_last_consolidated_at().await;
        let prior_pid = match fs::read_to_string(&self.path).await {
            Ok(s) => s.trim().parse::<i32>().ok(),
            Err(_) => None,
        };

        if prior_mtime > 0 {
            let now_ms = now_unix_ms();
            let age = now_ms.saturating_sub(prior_mtime);
            if age < self.holder_stale.as_millis() as i64 {
                if let Some(pid) = prior_pid {
                    if is_pid_running(pid) {
                        // Live holder, recent mtime — block.
                        return Ok(None);
                    }
                }
            }
            // Either dead PID or stale by `holder_stale` — reclaim.
        }

        // Write our PID.
        let our_pid = std::process::id();
        // mkdir parent if needed (memory_dir was canonicalized; should exist
        // — defensive against test envs that nuke and recreate).
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&self.path, our_pid.to_string()).await?;

        // Re-read to detect race (loser).
        let verify = match fs::read_to_string(&self.path).await {
            Ok(s) => s.trim().parse::<i32>().ok(),
            Err(_) => None,
        };
        match verify {
            Some(p) if p as u32 == our_pid => Ok(Some(prior_mtime)),
            _ => Ok(None),
        }
    }

    /// Rewind mtime to pre-acquire. Idempotent.
    /// `prior == 0` → unlink (mirror leak `:91-99`).
    /// Otherwise → set utimes to `prior_mtime / 1000` seconds (mirror
    /// leak `:101-107`).
    pub async fn rollback(&self, prior_mtime: i64) {
        if prior_mtime == 0 {
            // Try to unlink. Errors logged but not bubbled.
            if let Err(e) = fs::remove_file(&self.path).await {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        target: "auto_dream.lock_rollback",
                        path = %self.path.display(),
                        error = %e,
                        "rollback unlink failed"
                    );
                }
            }
            return;
        }
        // Set utimes to prior_mtime. Use std::fs (sync) since tokio
        // doesn't expose utimes directly without an extra crate.
        let secs = prior_mtime / 1000;
        let nanos = ((prior_mtime % 1000) * 1_000_000) as u32;
        let timestamp = SystemTime::UNIX_EPOCH
            + Duration::new(secs as u64, nanos);
        let path = self.path.clone();
        let result = tokio::task::spawn_blocking(move || {
            // Clear PID body before utimes — leak `:103`.
            std::fs::write(&path, b"")?;
            filetime::set_file_mtime(
                &path,
                filetime::FileTime::from_system_time(timestamp),
            )?;
            Ok::<(), std::io::Error>(())
        })
        .await;
        if let Err(e) = result {
            tracing::warn!(
                target: "auto_dream.lock_rollback",
                error = %e,
                "rollback join failed"
            );
        } else if let Ok(Err(e)) = result {
            tracing::warn!(
                target: "auto_dream.lock_rollback",
                error = %e,
                "rollback utimes failed — next trigger delayed to min_hours"
            );
        }
    }

    /// Stamp from manual `/dream`. Best-effort. Mirror leak `:130-138`.
    /// Optimistic: fires at prompt-build time, no completion hook.
    pub async fn record_consolidation(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent).await;
        }
        let pid = std::process::id();
        if let Err(e) = fs::write(&self.path, pid.to_string()).await {
            tracing::warn!(
                target: "auto_dream.record_consolidation",
                error = %e,
                "record write failed"
            );
        }
    }

    /// Path accessor for tests.
    #[doc(hidden)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// True if a process with `pid` is currently running. Uses `kill(0)`
/// on Unix — sends no signal but returns Ok if process exists. Mirror
/// leak `nix::sys::signal::kill(pid, None)` semantics (TS uses
/// `process.kill(pid, 0)`).
pub fn is_pid_running(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    signal::kill(Pid::from_raw(pid), None).is_ok()
}

/// Phase 80.1.e — sync probe for the scoring sweep coordination.
/// Reads the lock file, parses the PID body, and checks live-ness.
/// **Fail-open**: any I/O / parse error → `false` (no live holder
/// detected). Real liveness checks log nothing — sweep proceeds
/// silently if the probe can't read the file.
impl nexo_driver_types::ConsolidationLockProbe for ConsolidationLock {
    fn is_live_holder(&self) -> bool {
        match std::fs::read_to_string(&self.path) {
            Ok(body) => match body.trim().parse::<i32>() {
                Ok(0) => false, // rollback marker
                Ok(pid) if pid > 0 => is_pid_running(pid),
                _ => false,
            },
            Err(_) => false, // file absent or unreadable → fail-open
        }
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// List session IDs from a transcript dir whose mtime > `since_ms`,
/// excluding the current session. Mirror leak `:118-124`.
///
/// Sessions are identified by directory entries whose name parses as
/// a UUID. Non-UUID files (e.g., `agent-*.jsonl` per leak `:117`) are
/// skipped.
pub async fn list_sessions_touched_since(
    transcript_dir: &Path,
    since_ms: i64,
    exclude_current: &str,
) -> Result<Vec<String>, AutoDreamError> {
    if !transcript_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut rd = fs::read_dir(transcript_dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Skip non-UUID names.
        let stem = name.strip_suffix(".jsonl").unwrap_or(&name);
        if uuid::Uuid::parse_str(stem).is_err() {
            continue;
        }
        if stem == exclude_current {
            continue;
        }
        let meta = entry.metadata().await?;
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if mtime_ms > since_ms {
            out.push(stem.to_string());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    fn mk_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn read_last_returns_zero_when_absent() {
        let tmp = mk_dir();
        let lock =
            ConsolidationLock::new(tmp.path(), Duration::from_secs(3600)).unwrap();
        assert_eq!(lock.read_last_consolidated_at().await, 0);
    }

    #[tokio::test]
    async fn try_acquire_succeeds_first_time() {
        let tmp = mk_dir();
        let lock =
            ConsolidationLock::new(tmp.path(), Duration::from_secs(3600)).unwrap();
        let prior = lock.try_acquire().await.unwrap();
        assert_eq!(prior, Some(0)); // no prior
        let mtime = lock.read_last_consolidated_at().await;
        assert!(mtime > 0);
    }

    #[tokio::test]
    async fn try_acquire_blocked_by_live_pid() {
        let tmp = mk_dir();
        let lock =
            ConsolidationLock::new(tmp.path(), Duration::from_secs(3600)).unwrap();
        // Write our own PID — we know it's alive (we're running).
        // Sandbox-safe: kill(1, None) may be blocked in CI sandboxes,
        // so we use our own pid.
        let our_pid = std::process::id().to_string();
        fs::write(lock.path(), &our_pid).await.unwrap();
        // Within holder_stale window with a live PID, must block.
        let result = lock.try_acquire().await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn try_acquire_reclaims_dead_pid() {
        let tmp = mk_dir();
        let lock =
            ConsolidationLock::new(tmp.path(), Duration::from_secs(3600)).unwrap();
        // Write a PID we know is dead — pick something huge.
        let dead_pid = "999999";
        fs::write(lock.path(), dead_pid).await.unwrap();
        // Set mtime to recent so age check is triggered.
        let result = lock.try_acquire().await.unwrap();
        assert!(result.is_some()); // reclaimed
    }

    #[tokio::test]
    async fn try_acquire_reclaims_after_holder_stale() {
        let tmp = mk_dir();
        let lock =
            ConsolidationLock::new(tmp.path(), Duration::from_millis(50)).unwrap();
        // Write live PID + back-date mtime to 100ms ago.
        fs::write(lock.path(), "1").await.unwrap();
        // Sleep > holder_stale.
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Should reclaim despite live PID.
        let result = lock.try_acquire().await.unwrap();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn rollback_unlinks_when_prior_zero() {
        let tmp = mk_dir();
        let lock =
            ConsolidationLock::new(tmp.path(), Duration::from_secs(3600)).unwrap();
        let _ = lock.try_acquire().await.unwrap();
        assert!(lock.path().exists());
        lock.rollback(0).await;
        assert!(!lock.path().exists(), "rollback(0) must unlink");
    }

    #[tokio::test]
    async fn rollback_resets_mtime_when_prior_nonzero() {
        let tmp = mk_dir();
        let lock =
            ConsolidationLock::new(tmp.path(), Duration::from_secs(3600)).unwrap();
        let _ = lock.try_acquire().await.unwrap();
        let prior = 1_000_000_000_000_i64; // year 2001
        lock.rollback(prior).await;
        let after = lock.read_last_consolidated_at().await;
        // Allow ±1s drift due to filesystem mtime granularity.
        assert!(
            (after - prior).abs() < 1000,
            "expected ~{prior}, got {after}"
        );
        // PID body cleared.
        let body = fs::read_to_string(lock.path()).await.unwrap();
        assert_eq!(body, "");
    }

    #[tokio::test]
    async fn rollback_idempotent() {
        let tmp = mk_dir();
        let lock =
            ConsolidationLock::new(tmp.path(), Duration::from_secs(3600)).unwrap();
        // Path doesn't exist — rollback(0) should be no-op (no panic).
        lock.rollback(0).await;
        // Acquire and rollback twice.
        let _ = lock.try_acquire().await.unwrap();
        lock.rollback(123_456).await;
        lock.rollback(123_456).await; // double-rollback safe
    }

    #[tokio::test]
    async fn record_consolidation_writes_pid() {
        let tmp = mk_dir();
        let lock =
            ConsolidationLock::new(tmp.path(), Duration::from_secs(3600)).unwrap();
        lock.record_consolidation().await;
        let body = fs::read_to_string(lock.path()).await.unwrap();
        assert_eq!(body.trim(), std::process::id().to_string());
    }

    #[tokio::test]
    async fn read_holder_info_returns_pid_and_mtime() {
        let tmp = mk_dir();
        let lock =
            ConsolidationLock::new(tmp.path(), Duration::from_secs(3600)).unwrap();
        let _ = lock.try_acquire().await.unwrap();
        let (pid, mtime) = lock.read_holder_info().await;
        assert_eq!(pid as u32, std::process::id());
        assert!(mtime > 0);
    }

    #[test]
    fn is_pid_running_negative_pid_false() {
        assert!(!is_pid_running(-1));
        assert!(!is_pid_running(0));
    }

    #[test]
    fn is_pid_running_self_true() {
        // Our own PID is always alive while we're running. Sandbox-safe
        // (kill(1, ...) may be blocked in CI sandboxes).
        let our_pid = std::process::id() as i32;
        assert!(is_pid_running(our_pid));
    }

    #[tokio::test]
    async fn list_sessions_returns_empty_when_dir_missing() {
        let result = list_sessions_touched_since(
            Path::new("/nonexistent/transcripts"),
            0,
            "current",
        )
        .await
        .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn list_sessions_filters_by_mtime_and_excludes_current() {
        let tmp = mk_dir();
        let dir = tmp.path();

        let s1 = "11111111-1111-1111-1111-111111111111";
        let s2 = "22222222-2222-2222-2222-222222222222";
        let current = "33333333-3333-3333-3333-333333333333";

        fs::write(dir.join(format!("{s1}.jsonl")), b"x").await.unwrap();
        fs::write(dir.join(format!("{s2}.jsonl")), b"y").await.unwrap();
        fs::write(dir.join(format!("{current}.jsonl")), b"z").await.unwrap();
        // Non-UUID file should be skipped.
        fs::write(dir.join("agent-foo.jsonl"), b"a").await.unwrap();

        // since=0 → all qualify (other than current).
        let mut got = list_sessions_touched_since(dir, 0, current).await.unwrap();
        got.sort();
        assert_eq!(got, vec![s1.to_string(), s2.to_string()]);
    }

    #[tokio::test]
    async fn list_sessions_skips_non_uuid_names() {
        let tmp = mk_dir();
        let dir = tmp.path();
        fs::write(dir.join("not-a-uuid.jsonl"), b"x").await.unwrap();
        fs::write(dir.join("agent-foo.jsonl"), b"x").await.unwrap();
        let got = list_sessions_touched_since(dir, 0, "any").await.unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn new_rejects_missing_dir() {
        let result = ConsolidationLock::new(
            Path::new("/__should__not__exist__nx"),
            Duration::from_secs(3600),
        );
        assert!(result.is_err());
    }

    // ── Phase 80.1.e — ConsolidationLockProbe impl ──

    use nexo_driver_types::ConsolidationLockProbe;

    #[test]
    fn probe_returns_false_when_lock_absent() {
        let tmp = mk_dir();
        let lock = ConsolidationLock::new(tmp.path(), Duration::from_secs(3600))
            .unwrap();
        // No lock file written yet.
        assert!(!lock.is_live_holder());
    }

    #[tokio::test]
    async fn probe_returns_false_for_pid_zero() {
        let tmp = mk_dir();
        let lock = ConsolidationLock::new(tmp.path(), Duration::from_secs(3600))
            .unwrap();
        std::fs::write(tmp.path().canonicalize().unwrap().join(LOCK_FILE), b"0")
            .unwrap();
        assert!(!lock.is_live_holder());
    }

    #[test]
    fn probe_returns_true_for_live_pid() {
        let tmp = mk_dir();
        let lock = ConsolidationLock::new(tmp.path(), Duration::from_secs(3600))
            .unwrap();
        // std::process::id() is always alive — no PID 1 sandbox surprises.
        let our_pid = std::process::id();
        std::fs::write(
            tmp.path().canonicalize().unwrap().join(LOCK_FILE),
            our_pid.to_string(),
        )
        .unwrap();
        assert!(lock.is_live_holder());
    }

    #[test]
    fn probe_returns_false_for_dead_pid() {
        let tmp = mk_dir();
        let lock = ConsolidationLock::new(tmp.path(), Duration::from_secs(3600))
            .unwrap();
        // Find a clearly-dead PID by walking backwards from the typical
        // PID-max boundary; on Linux 32k is the historical default. Use
        // 999_999 — outside almost any kernel's pid_max.
        std::fs::write(
            tmp.path().canonicalize().unwrap().join(LOCK_FILE),
            b"999999",
        )
        .unwrap();
        assert!(!lock.is_live_holder());
    }

    #[test]
    fn probe_returns_false_for_garbage_body() {
        let tmp = mk_dir();
        let lock = ConsolidationLock::new(tmp.path(), Duration::from_secs(3600))
            .unwrap();
        std::fs::write(
            tmp.path().canonicalize().unwrap().join(LOCK_FILE),
            b"not-a-pid",
        )
        .unwrap();
        assert!(!lock.is_live_holder());
    }
}
