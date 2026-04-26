//! Phase 67.B.4 — boot-time reattach. After a daemon restart the
//! in-memory `AgentRegistry` is empty; this module reads the
//! persistent store, decides what to do with each row, and seeds the
//! registry so `list_agents` / `agent_status` keep working
//! immediately.
//!
//! Reattach policy in this step is conservative:
//!
//! - `Running` rows that the operator wants to reattach (the
//!   default) are surfaced to the caller as
//!   [`ReattachOutcome::Resume`] alongside their `AgentHandle`. The
//!   actual subprocess respawn lives in Phase 67.C.1; this crate
//!   does not own that decision.
//! - `Running` rows when reattach is disabled are flipped to
//!   `LostOnRestart` and persisted, so the next chat query still
//!   sees them.
//! - `Queued` rows go back into the queue. They never started; we
//!   keep them admitted so the caller can drive promotion.
//! - `Paused` rows stay paused — the operator's intent survives.
//! - Terminal rows (`Done` / `Failed` / `Cancelled` / `LostOnRestart`)
//!   are loaded into the in-memory map only if their `finished_at`
//!   is recent enough to be useful in `list_agents`. The caller
//!   passes the cutoff.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::registry::{AdmitOutcome, AgentRegistry};
use crate::store::AgentRegistryStore;
use crate::types::{AgentHandle, AgentRunStatus, RegistryError};

/// What to do with each row found in the store.
#[derive(Clone, Debug)]
pub enum ReattachOutcome {
    /// In-memory state restored; the caller should resume the
    /// underlying subprocess.
    Resume(AgentHandle),
    /// Goal was Running before the crash but reattach is off in
    /// config → marked LostOnRestart and persisted.
    MarkedLost(AgentHandle),
    /// Queued goal was re-admitted; the caller should kick the
    /// scheduler so it can promote when capacity frees up.
    Requeued(AgentHandle),
    /// Paused or terminal — kept in memory but no caller action.
    Recorded(AgentHandle),
    /// Old terminal row skipped (older than `keep_terminal_for`).
    Skipped {
        goal_id: nexo_driver_types::GoalId,
        status: AgentRunStatus,
    },
}

#[derive(Clone, Debug)]
pub struct ReattachOptions {
    /// `true` → Running rows return [`ReattachOutcome::Resume`].
    /// `false` → flip them to `LostOnRestart`.
    pub resume_running: bool,
    /// Drop terminal rows whose `finished_at` is older than
    /// `now - keep_terminal_for`. Skipped rows are removed from the
    /// store too so the file does not grow unbounded.
    pub keep_terminal_for: Duration,
}

impl Default for ReattachOptions {
    fn default() -> Self {
        Self {
            resume_running: true,
            keep_terminal_for: Duration::from_secs(60 * 60 * 24 * 7), // 7d
        }
    }
}

/// Walks the store, applies the policy, and seeds the registry.
/// Returns one outcome per row in store order.
pub async fn reattach(
    registry: &AgentRegistry,
    store: Arc<dyn AgentRegistryStore>,
    opts: ReattachOptions,
) -> Result<Vec<ReattachOutcome>, RegistryError> {
    let cutoff: DateTime<Utc> = Utc::now()
        - chrono::Duration::from_std(opts.keep_terminal_for)
            .unwrap_or_else(|_| chrono::Duration::days(7));

    let rows = store.list().await?;
    let mut out = Vec::with_capacity(rows.len());

    for handle in rows {
        match handle.status {
            AgentRunStatus::Running => {
                if opts.resume_running {
                    let mut h = handle.clone();
                    // Re-admit as Running. enqueue=true would push it
                    // to the queue if cap is full; reattach should
                    // honour cap so an operator who shrunk the cap
                    // doesn't oversubscribe on restart.
                    let admit = registry.admit(h.clone(), true).await?;
                    if matches!(admit, AdmitOutcome::Queued { .. }) {
                        // Cap was tighter than before; requeue.
                        out.push(ReattachOutcome::Requeued(h));
                    } else {
                        // admit() resets status to Running; reflect that.
                        h.status = AgentRunStatus::Running;
                        out.push(ReattachOutcome::Resume(h));
                    }
                } else {
                    let mut lost = handle.clone();
                    lost.status = AgentRunStatus::LostOnRestart;
                    lost.finished_at = Some(Utc::now());
                    store.upsert(&lost).await?;
                    // Mirror to memory so list_agents sees it.
                    registry.admit(lost.clone(), true).await?;
                    registry
                        .set_status(lost.goal_id, AgentRunStatus::LostOnRestart)
                        .await?;
                    out.push(ReattachOutcome::MarkedLost(lost));
                }
            }
            AgentRunStatus::Queued => {
                let h = handle.clone();
                let _ = registry.admit(h.clone(), true).await?;
                // admit() flips status to Running when there is
                // capacity; force it back to Queued so the
                // restored queue intent matches what was on disk.
                registry
                    .set_status(h.goal_id, AgentRunStatus::Queued)
                    .await?;
                out.push(ReattachOutcome::Requeued(h));
            }
            AgentRunStatus::Paused => {
                registry.admit(handle.clone(), true).await?;
                registry
                    .set_status(handle.goal_id, AgentRunStatus::Paused)
                    .await?;
                out.push(ReattachOutcome::Recorded(handle));
            }
            AgentRunStatus::Done
            | AgentRunStatus::Failed
            | AgentRunStatus::Cancelled
            | AgentRunStatus::LostOnRestart => {
                let too_old = handle.finished_at.map(|t| t < cutoff).unwrap_or(false);
                if too_old {
                    store.remove(handle.goal_id).await?;
                    out.push(ReattachOutcome::Skipped {
                        goal_id: handle.goal_id,
                        status: handle.status,
                    });
                } else {
                    // Terminal rows are kept in memory so list_agents
                    // can show them; admit() flips status to Running,
                    // so we restore the real status afterwards.
                    registry.admit(handle.clone(), true).await?;
                    registry.set_status(handle.goal_id, handle.status).await?;
                    out.push(ReattachOutcome::Recorded(handle));
                }
            }
        }
    }

    Ok(out)
}
