//! Background retention-sweep task for [`super::store::EventStore`].
//!
//! Microapps usually want to drop old firehose rows on a schedule
//! so the SQLite file doesn't grow unbounded. [`spawn_sweep_loop`]
//! runs an eager pass on entry (so long-idle restarts drain
//! accumulated rows immediately) and then loops on the configured
//! interval until cancelled.
//!
//! The caller owns the [`tokio_util::sync::CancellationToken`]
//! returned via [`SweepHandle::shutdown`] — flip it during graceful
//! shutdown then `await` the [`SweepHandle::task`] to ensure the
//! loop drained before exit.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::metadata::EventMetadata;
use super::store::EventStore;

/// Default retention window — 90 days. Mirrors the legacy
/// agent-creator-microapp default.
pub const DEFAULT_RETENTION_DAYS: u64 = 90;
/// Default row cap — 100k rows. After phase-1 time-based pruning,
/// the oldest rows beyond this number are dropped.
pub const DEFAULT_MAX_ROWS: usize = 100_000;
/// Default sweep interval — 6 h. `0` switches to eager-only mode.
pub const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 6 * 3600;

/// Retention sweep parameters. Forwarded to
/// [`EventStore::sweep_retention`] on each tick.
///
/// `interval_secs == 0` → eager-only mode: the loop runs ONCE on
/// spawn and then exits cleanly. Useful when retention is wanted
/// at boot only and a separate cron handles long-running drops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepConfig {
    /// Drop rows older than `retention_days * 86_400 * 1000` ms.
    pub retention_days: u64,
    /// Cap total rows. After phase 1 the oldest excess rows drop.
    pub max_rows: usize,
    /// Seconds between ticks. `0` = eager-only.
    pub interval_secs: u64,
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            retention_days: DEFAULT_RETENTION_DAYS,
            max_rows: DEFAULT_MAX_ROWS,
            interval_secs: DEFAULT_SWEEP_INTERVAL_SECS,
        }
    }
}

/// Boot-time handle returned by [`spawn_sweep_loop`]. Flip
/// `shutdown` to cancel, then `await` `task` to drain.
pub struct SweepHandle {
    /// JoinHandle for the spawned loop. `await` after cancelling
    /// `shutdown` to guarantee in-flight DELETEs finished.
    pub task: JoinHandle<()>,
    /// Cancellation token tied to `task`. Cloned internally; flip
    /// the original to break the loop.
    pub shutdown: CancellationToken,
}

/// Spawn the background sweep task. Returns a [`SweepHandle`] the
/// caller drives during graceful shutdown.
///
/// Algorithm:
/// 1. Eager pass — call [`EventStore::sweep_retention`] once.
/// 2. If `interval_secs == 0`, exit (eager-only mode).
/// 3. Otherwise loop: every `interval_secs` seconds, sweep again
///    until `shutdown` fires.
///
/// The first interval tick is burnt — we already swept eagerly,
/// so the next sweep happens `interval_secs` later, not
/// immediately.
pub fn spawn_sweep_loop<T>(store: Arc<EventStore<T>>, cfg: SweepConfig) -> SweepHandle
where
    T: EventMetadata + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(sweep_loop(Arc::clone(&store), cfg, shutdown.clone()));
    SweepHandle { task, shutdown }
}

async fn sweep_loop<T>(store: Arc<EventStore<T>>, cfg: SweepConfig, shutdown: CancellationToken)
where
    T: EventMetadata + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    // Eager first pass.
    match store
        .sweep_retention(cfg.retention_days, cfg.max_rows)
        .await
    {
        Ok(n) if n > 0 => {
            tracing::info!(deleted = n, "events sweep (eager) ran");
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "events sweep (eager) failed");
        }
    }
    if cfg.interval_secs == 0 {
        return;
    }
    let mut tick = tokio::time::interval(Duration::from_secs(cfg.interval_secs));
    // Burn the immediate first tick so the next fire is one
    // interval out (we already swept eagerly).
    tick.tick().await;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tick.tick() => {
                match store.sweep_retention(cfg.retention_days, cfg.max_rows).await {
                    Ok(n) => tracing::info!(deleted = n, "events sweep ran"),
                    Err(e) => tracing::warn!(error = %e, "events sweep failed"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::store::{EventStore, ListFilter, DEFAULT_TABLE};
    use nexo_tool_meta::admin::agent_events::{AgentEventKind, TranscriptRole};
    use uuid::Uuid;

    /// Eager-drain: `interval_secs=0` exits after the initial
    /// sweep, so awaiting the task future deterministically
    /// asserts the eager pass ran.
    #[tokio::test]
    async fn sweep_loop_eagerly_drains_on_spawn() {
        let store = Arc::new(EventStore::open_memory(DEFAULT_TABLE).await.unwrap());
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let day_ms: u64 = 86_400 * 1000;
        for i in 0..3u64 {
            let evt = AgentEventKind::TranscriptAppended {
                agent_id: "ana".into(),
                session_id: Uuid::nil(),
                seq: i,
                role: TranscriptRole::User,
                body: "old".into(),
                sent_at_ms: now_ms - (15 + i) * day_ms,
                sender_id: None,
                source_plugin: "whatsapp".into(),
                tenant_id: None,
            };
            store.append(&evt).await.unwrap();
        }
        let fresh = AgentEventKind::TranscriptAppended {
            agent_id: "ana".into(),
            session_id: Uuid::nil(),
            seq: 3,
            role: TranscriptRole::User,
            body: "fresh".into(),
            sent_at_ms: now_ms - 1_000,
            sender_id: None,
            source_plugin: "whatsapp".into(),
            tenant_id: None,
        };
        store.append(&fresh).await.unwrap();

        let cfg = SweepConfig {
            retention_days: 10,
            max_rows: 1_000_000,
            interval_secs: 0,
        };
        let handle = spawn_sweep_loop(Arc::clone(&store), cfg);
        // interval_secs=0 ⇒ task exits after eager pass.
        handle.task.await.unwrap();

        let after = store
            .list(&ListFilter {
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(after.len(), 1, "eager sweep should drain 3 old rows");
    }
}
