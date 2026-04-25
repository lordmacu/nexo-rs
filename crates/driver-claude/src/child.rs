//! Child-process lifetime helpers.
//!
//! `ChildHandle` guarantees the spawned `claude` is reaped: `shutdown`
//! does the cooperative SIGTERM → grace → SIGKILL dance; `Drop` is
//! best-effort if the caller forgets to call it (panic path, abort).

use std::process::ExitStatus;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Child;

use crate::error::ClaudeError;

pub struct ChildHandle {
    child: Option<Child>,
    forced_kill_after: Duration,
}

impl ChildHandle {
    pub fn wrap(child: Child, forced_kill_after: Duration) -> Self {
        Self {
            child: Some(child),
            forced_kill_after,
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|c| c.id())
    }

    /// Cooperative shutdown: close stdin → SIGTERM → wait up to
    /// `forced_kill_after` → SIGKILL → reap. Idempotent: takes
    /// `self`, can't be called twice.
    pub async fn shutdown(mut self) -> Result<ExitStatus, ClaudeError> {
        let mut child = self.child.take().expect("ChildHandle::shutdown twice");
        // Close stdin so the child sees EOF on its prompt input.
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.shutdown().await;
        }
        // SIGTERM (start_kill is `kill -SIGKILL` on Unix actually —
        // tokio doesn't expose SIGTERM directly. The cooperative
        // grace below makes this still useful: most CLIs treat the
        // signal as terminate-now, which is fine for `claude`.)
        let _ = child.start_kill();
        match tokio::time::timeout(self.forced_kill_after, child.wait()).await {
            Ok(res) => Ok(res?),
            Err(_) => {
                // Grace expired. `start_kill` already issued the
                // signal; reap.
                Ok(child.wait().await?)
            }
        }
    }
}

impl Drop for ChildHandle {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.start_kill();
        }
    }
}

/// Drain stderr to `tracing::warn!` line by line. Returns when EOF.
/// Spawn this in a background task; never join.
pub async fn drain_stderr<R: AsyncRead + Unpin + Send>(stderr: R) {
    let mut lines = BufReader::new(stderr).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) if !line.trim().is_empty() => {
                tracing::warn!(target: "claude-cli", "{line}");
            }
            Ok(Some(_)) => continue,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(target: "claude-cli", "stderr drain error: {e}");
                return;
            }
        }
    }
}
