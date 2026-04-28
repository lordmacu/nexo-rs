//! Phase 79.7 runtime firing — tokio task that polls the
//! [`CronStore`] and dispatches due entries.
//!
//! Without this loop the cron tools (`cron_create` etc.) ship
//! durable entries that NEVER fire. This module closes the gap.
//!
//! Architecture:
//!
//! 1. `CronRunner` owns an `Arc<dyn CronStore>` + an
//!    `Arc<dyn CronDispatcher>` (the actual side-effect
//!    invoker — production wires an LLM-call dispatcher; tests
//!    use a fake).
//! 2. On each tick:
//!    - `store.due_at(now)` returns every non-paused entry whose
//!      `next_fire_at <= now`.
//!    - For each entry, `dispatcher.fire(&entry)` runs the side
//!      effect.
//!    - Recurring entries always advance state even on dispatcher
//!      failure (avoids infinite hot-loop on a broken schedule).
//!    - One-shot entries use a retry policy:
//!      * success → `delete(...)`
//!      * failure and retries left → backoff + reschedule
//!      * failure and retries exhausted → `delete(...)`
//!      This keeps one-shot semantics bounded while avoiding silent
//!      loss on the first transient failure.
//! 3. Sleep `tick_interval` and repeat. `cancel` stops the loop
//!    cleanly.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/utils/cronTasks.ts` — the leak's
//!     scheduling tick is structurally similar (poll → fire →
//!     advance), though the leak runs in a single-process Node
//!     event loop.
//!
//! Reference (secondary):
//!   * Phase 20 `agent_turn` poller (`crates/poller/src/builtins/agent_turn.rs`)
//!     — similar dispatch shape (`LlmRegistry::build` → `chat` →
//!     optional outbound publish).
//!
//! Runtime status:
//!   * `LlmCronDispatcher` is the production path (model-routed chat,
//!     optional channel publish).
//!   * `LoggingCronDispatcher` remains as a safe fallback so cron
//!     fires stay observable under degraded boot wiring.

use crate::cron_schedule::{next_fire_after, CronEntry, CronStore};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Default tick interval. Lower = lower latency between
/// `next_fire_at` and actual fire; higher = less DB load. 5s is
/// the leak's pace and a sane default.
pub const DEFAULT_TICK_INTERVAL_SECS: u64 = 5;

/// Retry policy for one-shot cron entries when dispatch fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OneShotRetryPolicy {
    /// Number of retry attempts after the first failure. `0` keeps
    /// at-most-once semantics (drop on first failure).
    pub max_retries: u32,
    /// Delay (seconds) before attempt #1. Attempt N uses
    /// exponential backoff (`base * 2^(N-1)`) capped at
    /// `max_backoff_secs`.
    pub base_backoff_secs: u64,
    /// Upper bound for exponential delay growth.
    pub max_backoff_secs: u64,
}

impl OneShotRetryPolicy {
    pub fn retry_delay_secs(&self, attempt: u32) -> u64 {
        let base = self.base_backoff_secs.max(1);
        let pow = attempt.saturating_sub(1).min(31);
        let factor = 1u64 << pow;
        base.saturating_mul(factor)
            .min(self.max_backoff_secs.max(1))
    }
}

impl Default for OneShotRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_backoff_secs: 30,
            max_backoff_secs: 1800,
        }
    }
}

/// Side-effect invoker for a fired cron entry. Implementations
/// fan out to LLM, NATS, webhook, etc.
#[async_trait]
pub trait CronDispatcher: Send + Sync {
    async fn fire(&self, entry: &CronEntry) -> anyhow::Result<()>;
}

/// Default dispatcher — emits a `tracing::info!` per fire.
/// Production wiring layers a richer dispatcher (LLM call +
/// outbound publish) on top via [`CronRunner::with_dispatcher`].
pub struct LoggingCronDispatcher;

#[async_trait]
impl CronDispatcher for LoggingCronDispatcher {
    async fn fire(&self, entry: &CronEntry) -> anyhow::Result<()> {
        tracing::info!(
            id = %entry.id,
            binding_id = %entry.binding_id,
            cron = %entry.cron_expr,
            recurring = entry.recurring,
            channel = ?entry.channel,
            prompt_chars = entry.prompt.chars().count(),
            "[cron] fired (logging dispatcher fallback)"
        );
        Ok(())
    }
}

/// What happened to a single entry on a tick. Returned by
/// [`CronRunner::tick_once`] so tests can assert the loop's
/// per-entry behaviour without timing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FireOutcome {
    /// Recurring entry fired and advanced to a new
    /// `next_fire_at`.
    Advanced { id: String, new_next_fire_at: i64 },
    /// One-shot entry fired and was deleted.
    OneShotDeleted { id: String },
    /// One-shot dispatch failed and was rescheduled for a retry.
    OneShotRetryScheduled {
        id: String,
        retry_at: i64,
        attempt: u32,
        max_retries: u32,
        error: String,
    },
    /// One-shot dispatch failed and exhausted the retry budget; the
    /// entry was dropped.
    OneShotDroppedAfterRetries {
        id: String,
        attempts: u32,
        max_retries: u32,
        error: String,
    },
    /// Dispatcher failed; state was still advanced (or the
    /// entry deleted) so the loop never re-fires the same
    /// stuck entry.
    DispatcherFailed { id: String, error: String },
    /// Recurring entry fired but the next-fire computation
    /// failed (cron expr suddenly invalid?). Entry is left
    /// alone for the operator to inspect.
    NextFireUnknown { id: String, error: String },
}

pub struct CronRunner {
    store: Arc<dyn CronStore>,
    dispatcher: Arc<dyn CronDispatcher>,
    tick_interval: Duration,
    one_shot_retry: OneShotRetryPolicy,
}

impl CronRunner {
    pub fn new(store: Arc<dyn CronStore>, dispatcher: Arc<dyn CronDispatcher>) -> Self {
        Self {
            store,
            dispatcher,
            tick_interval: Duration::from_secs(DEFAULT_TICK_INTERVAL_SECS),
            one_shot_retry: OneShotRetryPolicy::default(),
        }
    }

    pub fn with_tick_interval(mut self, interval: Duration) -> Self {
        self.tick_interval = interval;
        self
    }

    pub fn with_one_shot_retry_policy(mut self, policy: OneShotRetryPolicy) -> Self {
        self.one_shot_retry = policy;
        self
    }

    /// Run a single tick at `now_unix` and advance state. Returns
    /// one outcome per entry that was due. Test-friendly.
    pub async fn tick_once(&self, now_unix: i64) -> Vec<FireOutcome> {
        let due = match self.store.due_at(now_unix).await {
            Ok(due) => due,
            Err(e) => {
                tracing::warn!(error = %e, "[cron] due_at query failed; skipping tick");
                return Vec::new();
            }
        };
        let mut outcomes = Vec::with_capacity(due.len());
        for entry in due {
            let id = entry.id.clone();
            let dispatch_err = match self.dispatcher.fire(&entry).await {
                Ok(()) => None,
                Err(e) => {
                    tracing::warn!(
                        id = %id,
                        binding_id = %entry.binding_id,
                        error = %e,
                        "[cron] dispatcher failed; advancing state anyway to avoid re-fire loop"
                    );
                    Some(e.to_string())
                }
            };

            if entry.recurring {
                match next_fire_after(&entry.cron_expr, now_unix) {
                    Ok(new_next) => {
                        if let Err(e) = self
                            .store
                            .advance_after_fire(&entry.id, new_next, now_unix)
                            .await
                        {
                            tracing::error!(
                                id = %id,
                                error = %e,
                                "[cron] advance_after_fire failed; entry will likely re-fire next tick"
                            );
                            outcomes.push(FireOutcome::NextFireUnknown {
                                id,
                                error: e.to_string(),
                            });
                            continue;
                        }
                        if let Some(err) = dispatch_err {
                            outcomes.push(FireOutcome::DispatcherFailed { id, error: err });
                        } else {
                            outcomes.push(FireOutcome::Advanced {
                                id,
                                new_next_fire_at: new_next,
                            });
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            id = %id,
                            error = %e,
                            "[cron] next-fire compute failed; leaving entry as-is for operator"
                        );
                        outcomes.push(FireOutcome::NextFireUnknown {
                            id,
                            error: e.to_string(),
                        });
                    }
                }
            } else {
                if let Some(err) = dispatch_err {
                    let next_attempt = entry.failure_count.saturating_add(1);
                    if entry.failure_count < self.one_shot_retry.max_retries {
                        let delay = self.one_shot_retry.retry_delay_secs(next_attempt);
                        let retry_at = now_unix.saturating_add(delay as i64);
                        match self
                            .store
                            .schedule_one_shot_retry(&entry.id, retry_at, now_unix)
                            .await
                        {
                            Ok(attempt) => {
                                tracing::warn!(
                                    id = %id,
                                    binding_id = %entry.binding_id,
                                    attempt = attempt,
                                    max_retries = self.one_shot_retry.max_retries,
                                    retry_at = retry_at,
                                    error = %err,
                                    "[cron] one-shot dispatch failed; retry scheduled"
                                );
                                outcomes.push(FireOutcome::OneShotRetryScheduled {
                                    id,
                                    retry_at,
                                    attempt,
                                    max_retries: self.one_shot_retry.max_retries,
                                    error: err,
                                });
                            }
                            Err(e) => {
                                tracing::error!(
                                    id = %id,
                                    error = %e,
                                    "[cron] one-shot retry schedule failed; entry may re-fire unexpectedly"
                                );
                                outcomes.push(FireOutcome::NextFireUnknown {
                                    id,
                                    error: e.to_string(),
                                });
                            }
                        }
                        continue;
                    }

                    if let Err(e) = self.store.delete(&entry.id).await {
                        tracing::error!(
                            id = %id,
                            error = %e,
                            "[cron] one-shot retry budget exhausted but delete failed"
                        );
                        outcomes.push(FireOutcome::NextFireUnknown {
                            id,
                            error: e.to_string(),
                        });
                        continue;
                    }

                    let attempts = entry.failure_count.saturating_add(1);
                    tracing::error!(
                        id = %id,
                        binding_id = %entry.binding_id,
                        attempts = attempts,
                        max_retries = self.one_shot_retry.max_retries,
                        error = %err,
                        "[cron] one-shot dispatch failed; retry budget exhausted, dropping entry"
                    );
                    outcomes.push(FireOutcome::OneShotDroppedAfterRetries {
                        id,
                        attempts,
                        max_retries: self.one_shot_retry.max_retries,
                        error: err,
                    });
                    continue;
                }

                if let Err(e) = self.store.delete(&entry.id).await {
                    tracing::error!(
                        id = %id,
                        error = %e,
                        "[cron] one-shot delete failed; entry may re-fire next tick"
                    );
                    outcomes.push(FireOutcome::NextFireUnknown {
                        id,
                        error: e.to_string(),
                    });
                    continue;
                }
                outcomes.push(FireOutcome::OneShotDeleted { id });
            }
        }
        outcomes
    }

    /// Run forever (until `cancel` fires). Production wiring
    /// spawns this on a tokio task.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        tracing::info!(
            tick_interval_secs = self.tick_interval.as_secs(),
            "[cron] runner started"
        );
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("[cron] runner cancelled");
                    break;
                }
                _ = tokio::time::sleep(self.tick_interval) => {
                    let now = chrono::Utc::now().timestamp();
                    let _ = self.tick_once(now).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron_schedule::{build_new_entry, SqliteCronStore};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeDispatcher {
        fires: Mutex<Vec<String>>,
        force_error: Mutex<Option<String>>,
    }

    impl FakeDispatcher {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        fn force_err(&self, msg: &str) {
            *self.force_error.lock().unwrap() = Some(msg.to_string());
        }
        fn captured(&self) -> Vec<String> {
            self.fires.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl CronDispatcher for FakeDispatcher {
        async fn fire(&self, entry: &CronEntry) -> anyhow::Result<()> {
            self.fires.lock().unwrap().push(entry.id.clone());
            if let Some(msg) = self.force_error.lock().unwrap().clone() {
                anyhow::bail!(msg);
            }
            Ok(())
        }
    }

    async fn populated_store(recurring: bool, cron: &str) -> (Arc<dyn CronStore>, String) {
        let store: Arc<dyn CronStore> = Arc::new(SqliteCronStore::open_memory().await.unwrap());
        let mut e = build_new_entry(
            &store,
            "whatsapp:default",
            cron,
            "ping",
            None,
            recurring,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        e.next_fire_at = 1_700_000_000;
        let id = e.id.clone();
        store.insert(&e).await.unwrap();
        (store, id)
    }

    #[tokio::test]
    async fn tick_advances_recurring_entry() {
        let (store, id) = populated_store(true, "*/5 * * * *").await;
        let dispatcher = FakeDispatcher::new();
        let runner = CronRunner::new(store.clone(), dispatcher.clone());
        let outcomes = runner.tick_once(1_700_000_500).await;
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            FireOutcome::Advanced {
                id: out_id,
                new_next_fire_at,
            } => {
                assert_eq!(out_id, &id);
                assert!(*new_next_fire_at > 1_700_000_500);
            }
            other => panic!("expected Advanced, got {other:?}"),
        }
        // Stored entry now has the new next_fire_at + last_fired_at.
        let updated = store.get(&id).await.unwrap();
        assert!(updated.next_fire_at > 1_700_000_500);
        assert_eq!(updated.last_fired_at, Some(1_700_000_500));
        // Dispatcher was invoked.
        assert_eq!(dispatcher.captured(), vec![id]);
    }

    #[tokio::test]
    async fn tick_deletes_one_shot_after_fire() {
        let (store, id) = populated_store(false, "*/5 * * * *").await;
        let dispatcher = FakeDispatcher::new();
        let runner = CronRunner::new(store.clone(), dispatcher.clone());
        let outcomes = runner.tick_once(1_700_000_500).await;
        assert_eq!(outcomes.len(), 1);
        assert!(
            matches!(&outcomes[0], FireOutcome::OneShotDeleted { id: out_id } if out_id == &id)
        );
        // Entry gone.
        assert!(store.get(&id).await.is_err());
        assert_eq!(dispatcher.captured().len(), 1);
    }

    #[tokio::test]
    async fn tick_skips_paused_entries() {
        let (store, id) = populated_store(true, "*/5 * * * *").await;
        store.set_paused(&id, true).await.unwrap();
        let dispatcher = FakeDispatcher::new();
        let runner = CronRunner::new(store.clone(), dispatcher.clone());
        let outcomes = runner.tick_once(1_700_000_500).await;
        assert!(outcomes.is_empty(), "paused entries must not fire");
        assert!(dispatcher.captured().is_empty());
    }

    #[tokio::test]
    async fn tick_skips_future_entries() {
        let (store, _id) = populated_store(true, "0 9 * * *").await; // daily 9am UTC
                                                                     // Force next_fire_at way ahead.
        let entries = store.list_by_binding("whatsapp:default").await.unwrap();
        let id = entries[0].id.clone();
        store
            .advance_after_fire(&id, 1_700_999_999, 0)
            .await
            .unwrap();
        let dispatcher = FakeDispatcher::new();
        let runner = CronRunner::new(store.clone(), dispatcher.clone());
        let outcomes = runner.tick_once(1_700_000_500).await;
        assert!(outcomes.is_empty(), "future entries must not fire");
    }

    #[tokio::test]
    async fn dispatcher_failure_advances_state_anyway() {
        let (store, id) = populated_store(true, "*/5 * * * *").await;
        let dispatcher = FakeDispatcher::new();
        dispatcher.force_err("simulated");
        let runner = CronRunner::new(store.clone(), dispatcher.clone());
        let outcomes = runner.tick_once(1_700_000_500).await;
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            &outcomes[0],
            FireOutcome::DispatcherFailed { id: out_id, error } if out_id == &id && error.contains("simulated")
        ));
        // CRITICAL: state was still advanced — without this the loop
        // would re-fire the same broken entry forever.
        let updated = store.get(&id).await.unwrap();
        assert!(updated.next_fire_at > 1_700_000_500);
    }

    #[tokio::test]
    async fn dispatcher_failure_on_one_shot_schedules_retry() {
        let (store, id) = populated_store(false, "*/5 * * * *").await;
        let dispatcher = FakeDispatcher::new();
        dispatcher.force_err("boom");
        let runner = CronRunner::new(store.clone(), dispatcher.clone());
        let outcomes = runner.tick_once(1_700_000_500).await;
        assert!(matches!(
            &outcomes[0],
            FireOutcome::OneShotRetryScheduled { id: out_id, attempt: 1, .. } if out_id == &id
        ));
        // One-shot is retained and moved forward for retry.
        let updated = store.get(&id).await.unwrap();
        assert_eq!(updated.failure_count, 1);
        assert!(updated.next_fire_at > 1_700_000_500);
    }

    #[tokio::test]
    async fn one_shot_drops_after_retry_budget_exhausted() {
        let (store, id) = populated_store(false, "*/5 * * * *").await;
        let dispatcher = FakeDispatcher::new();
        dispatcher.force_err("boom");
        let runner = CronRunner::new(store.clone(), dispatcher.clone()).with_one_shot_retry_policy(
            OneShotRetryPolicy {
                max_retries: 1,
                base_backoff_secs: 10,
                max_backoff_secs: 60,
            },
        );

        // First failure schedules retry.
        let first = runner.tick_once(1_700_000_500).await;
        assert!(matches!(
            &first[0],
            FireOutcome::OneShotRetryScheduled { id: out_id, attempt: 1, .. } if out_id == &id
        ));
        let scheduled = store.get(&id).await.unwrap();
        assert_eq!(scheduled.failure_count, 1);

        // Second failure exhausts budget and drops entry.
        let second = runner.tick_once(scheduled.next_fire_at + 1).await;
        assert!(matches!(
            &second[0],
            FireOutcome::OneShotDroppedAfterRetries {
                id: out_id,
                attempts: 2,
                max_retries: 1,
                ..
            } if out_id == &id
        ));
        assert!(store.get(&id).await.is_err(), "entry should be deleted");
    }

    #[tokio::test]
    async fn many_due_entries_all_fire_in_one_tick() {
        let store: Arc<dyn CronStore> = Arc::new(SqliteCronStore::open_memory().await.unwrap());
        for i in 0..5 {
            let mut e = build_new_entry(
                &store,
                "whatsapp:default",
                "*/5 * * * *",
                &format!("ping-{i}"),
                None,
                true,
                None,
                None,
                None,
            )
            .await
            .unwrap();
            e.next_fire_at = 1_700_000_000;
            store.insert(&e).await.unwrap();
        }
        let dispatcher = FakeDispatcher::new();
        let runner = CronRunner::new(store.clone(), dispatcher.clone());
        let outcomes = runner.tick_once(1_700_000_500).await;
        assert_eq!(outcomes.len(), 5);
        assert_eq!(dispatcher.captured().len(), 5);
        // All entries advanced.
        let listed = store.list_by_binding("whatsapp:default").await.unwrap();
        assert!(listed.iter().all(|e| e.next_fire_at > 1_700_000_500));
    }

    #[tokio::test]
    async fn run_loop_terminates_on_cancel() {
        let store: Arc<dyn CronStore> = Arc::new(SqliteCronStore::open_memory().await.unwrap());
        let dispatcher = FakeDispatcher::new();
        let runner = Arc::new(
            CronRunner::new(store, dispatcher.clone())
                .with_tick_interval(Duration::from_millis(20)),
        );
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let handle = tokio::spawn(async move {
            runner.run(cancel2).await;
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        cancel.cancel();
        // Should terminate quickly after cancel.
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("runner did not terminate after cancel")
            .expect("runner task panicked");
    }

    #[tokio::test]
    async fn logging_dispatcher_returns_ok() {
        let entry = CronEntry {
            id: "x".into(),
            binding_id: "wp:def".into(),
            cron_expr: "*/5 * * * *".into(),
            prompt: "ping".into(),
            channel: None,
            model_provider: None,
            model_name: None,
            recurring: true,
            created_at: 0,
            next_fire_at: 0,
            last_fired_at: None,
            failure_count: 0,
            paused: false,
            recipient: None,
        };
        assert!(LoggingCronDispatcher.fire(&entry).await.is_ok());
    }
}
