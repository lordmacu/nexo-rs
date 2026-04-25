//! `TurnHandle` — owns the spawned child + event stream + cancel
//! token + per-turn deadline.

use std::process::ExitStatus;
use std::time::Duration;

use nexo_driver_types::CancellationToken;
use tokio::io::BufReader;
use tokio::process::ChildStdout;
use tokio::time::Instant;

use crate::child::{drain_stderr, ChildHandle};
use crate::command::ClaudeCommand;
use crate::error::ClaudeError;
use crate::event::ClaudeEvent;
use crate::stream::EventStream;

pub struct TurnHandle {
    child: ChildHandle,
    events: EventStream<BufReader<ChildStdout>>,
    cancel: CancellationToken,
    deadline: Instant,
}

impl TurnHandle {
    /// Pull the next event. Errors:
    /// - `Cancelled` — the caller's cancel token fired.
    /// - `Timeout` — the per-turn wall-clock budget elapsed.
    /// - `ParseLine` — a malformed JSONL line.
    /// - `Io` — stdout pipe failure.
    pub async fn next_event(&mut self) -> Result<Option<ClaudeEvent>, ClaudeError> {
        // Fast path: cancel was already signalled.
        if self.cancel.is_cancelled() {
            return Err(ClaudeError::Cancelled);
        }
        let now = Instant::now();
        if now >= self.deadline {
            return Err(ClaudeError::Timeout);
        }
        tokio::select! {
            _ = self.cancel.cancelled() => Err(ClaudeError::Cancelled),
            _ = tokio::time::sleep_until(self.deadline) => Err(ClaudeError::Timeout),
            res = self.events.next() => res,
        }
    }

    /// Drain remaining events into a `Vec`. Stops on first error or
    /// `None`. Convenience for tests / one-shot callers; production
    /// code iterates `next_event` for incremental control.
    pub async fn drain(&mut self) -> Result<Vec<ClaudeEvent>, ClaudeError> {
        let mut out = Vec::new();
        while let Some(ev) = self.next_event().await? {
            out.push(ev);
        }
        Ok(out)
    }

    pub async fn shutdown(self) -> Result<ExitStatus, ClaudeError> {
        self.child.shutdown().await
    }
}

/// Spawn one turn of `claude` from a built command. The returned
/// handle owns the child + its stdout stream + cancellation hooks.
pub async fn spawn_turn(
    cmd: ClaudeCommand,
    cancel: &CancellationToken,
    turn_timeout: Duration,
    forced_kill_after: Duration,
) -> Result<TurnHandle, ClaudeError> {
    let mut command = cmd.into_command();
    let mut child = command
        .spawn()
        .map_err(|e| ClaudeError::Spawn(e.to_string()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or(ClaudeError::MissingPipe("stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or(ClaudeError::MissingPipe("stderr"))?;

    // Stderr drains to tracing in the background. We never join —
    // the task ends when stderr EOFs.
    tokio::spawn(drain_stderr(stderr));

    Ok(TurnHandle {
        child: ChildHandle::wrap(child, forced_kill_after),
        events: EventStream::new(BufReader::new(stdout)),
        cancel: cancel.clone(),
        deadline: Instant::now() + turn_timeout,
    })
}
