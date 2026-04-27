//! Per-account IMAP IDLE worker (Phase 48.3).
//!
//! Each `AccountWorker` owns one `ImapConnection` and drives the
//! Connecting → Idle/Polling → ... state machine. `InboundManager`
//! spawns one worker per declared account and owns the cancellation
//! token + join handles.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use nexo_auth::email::EmailCredentialStore;
use nexo_auth::google::GoogleCredentialStore;
use nexo_broker::{AnyBroker, BrokerHandle, Event};
use nexo_config::types::plugins::{EmailAccountConfig, EmailPluginConfig};
use nexo_resilience::CircuitBreaker;
use rand::Rng;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::cursor::{CursorStore, UidCursor};
use crate::events::InboundEvent;
use crate::health::{AccountHealth, WorkerState};
use crate::imap_conn::{ImapConnection, IdleOutcome};
use crate::plugin::inbound_topic_for;

const SOURCE: &str = "plugin.email";
const INITIAL_BACKOFF_MS: u64 = 1_000;
const MAX_BACKOFF_MS: u64 = 60_000;

/// Map of `instance -> AccountHealth`. Cloned `Arc` is what
/// `EmailPlugin::health()` reads (Phase 48.10).
pub type HealthMap = Arc<DashMap<String, Arc<RwLock<AccountHealth>>>>;

pub struct InboundManager {
    workers: Vec<JoinHandle<()>>,
    cancel: CancellationToken,
    health: HealthMap,
}

impl InboundManager {
    /// Spawn one worker per account. Returns immediately; workers run
    /// in the background until `stop()` is called.
    pub fn start(
        cfg: &EmailPluginConfig,
        creds: Arc<EmailCredentialStore>,
        google: Arc<GoogleCredentialStore>,
        cursor: Arc<CursorStore>,
        broker: AnyBroker,
    ) -> Self {
        let cancel = CancellationToken::new();
        let health: HealthMap = Arc::new(DashMap::new());
        let mut workers = Vec::with_capacity(cfg.accounts.len());

        for account_cfg in &cfg.accounts {
            let h = Arc::new(RwLock::new(AccountHealth::default()));
            health.insert(account_cfg.instance.clone(), h.clone());

            let worker = AccountWorker {
                account_cfg: account_cfg.clone(),
                creds: creds.clone(),
                google: google.clone(),
                cursor: cursor.clone(),
                broker: broker.clone(),
                health: h,
                cancel: cancel.child_token(),
                idle_reissue: Duration::from_secs(cfg.idle_reissue_minutes * 60),
                poll_fallback: Duration::from_secs(cfg.poll_fallback_seconds),
                inbox_folder: account_cfg.folders.inbox.clone(),
            };
            workers.push(tokio::spawn(worker.run()));
        }

        Self {
            workers,
            cancel,
            health,
        }
    }

    /// Cancel every worker and join them with a 5s upper bound. Workers
    /// that don't drain in time are dropped — IMAP servers tolerate the
    /// abort, the daemon must shut down promptly.
    pub async fn stop(self) {
        self.cancel.cancel();
        let join = futures::future::join_all(self.workers);
        let _ = tokio::time::timeout(Duration::from_secs(5), join).await;
    }

    pub fn health_map(&self) -> HealthMap {
        self.health.clone()
    }
}

struct AccountWorker {
    account_cfg: EmailAccountConfig,
    creds: Arc<EmailCredentialStore>,
    google: Arc<GoogleCredentialStore>,
    cursor: Arc<CursorStore>,
    broker: AnyBroker,
    health: Arc<RwLock<AccountHealth>>,
    cancel: CancellationToken,
    idle_reissue: Duration,
    poll_fallback: Duration,
    inbox_folder: String,
}

impl AccountWorker {
    async fn run(self) {
        let instance = self.account_cfg.instance.clone();
        info!(target: "plugin.email", instance = %instance, "worker spawned");

        let breaker = Arc::new(CircuitBreaker::new(
            format!("email/{instance}"),
            nexo_resilience::CircuitBreakerConfig::default(),
        ));

        let mut attempt: u32 = 0;
        loop {
            if self.cancel.is_cancelled() {
                info!(target: "plugin.email", instance = %instance, "worker cancelled");
                break;
            }

            if !breaker.allow() {
                self.set_state(WorkerState::Down).await;
                let wait = jittered_backoff(attempt);
                if !sleep_or_cancel(wait, &self.cancel).await {
                    break;
                }
                continue;
            }

            self.set_state(WorkerState::Connecting).await;
            match self.connect_select_and_loop(&breaker).await {
                Ok(()) => {
                    // run() returned cleanly (cancel) — exit
                    break;
                }
                Err(e) => {
                    breaker.on_failure();
                    attempt = attempt.saturating_add(1);
                    self.note_failure(format!("{e:#}")).await;
                    warn!(
                        target: "plugin.email",
                        instance = %instance,
                        attempt = attempt,
                        error = %e,
                        "worker failure — backing off"
                    );
                    let wait = jittered_backoff(attempt);
                    if !sleep_or_cancel(wait, &self.cancel).await {
                        break;
                    }
                }
            }
        }
    }

    async fn connect_select_and_loop(&self, breaker: &CircuitBreaker) -> Result<()> {
        let creds = self
            .creds
            .account(&self.account_cfg.instance)
            .ok_or_else(|| anyhow!("no credentials for instance '{}'", self.account_cfg.instance))?
            .clone();

        let mut conn = ImapConnection::connect(
            &self.account_cfg.imap,
            &creds,
            self.google.clone(),
        )
        .await?;
        breaker.on_success();
        self.note_connect_ok().await;

        let mb = conn.select(&self.inbox_folder).await?;
        let cursor_now = self
            .cursor
            .reset_if_validity_changed(&self.account_cfg.instance, mb.uid_validity)
            .await?;
        let mut last_uid = cursor_now.last_uid;

        // Drain anything the server already has past last_uid before
        // entering IDLE — covers the boot case where new mail landed
        // while the daemon was down.
        last_uid = self.drain_pending(&mut conn, last_uid).await?;

        let supports_idle = conn.capabilities.idle;
        if !supports_idle {
            info!(
                target: "plugin.email",
                instance = %self.account_cfg.instance,
                "server lacks IDLE; entering polling mode"
            );
            self.set_state(WorkerState::Polling).await;
            self.poll_loop(conn, last_uid).await
        } else {
            self.set_state(WorkerState::Idle).await;
            self.idle_loop(conn, last_uid).await
        }
    }

    async fn idle_loop(&self, mut conn: ImapConnection, mut last_uid: u32) -> Result<()> {
        loop {
            if self.cancel.is_cancelled() {
                let _ = conn.logout().await;
                return Ok(());
            }
            let (returned, outcome) = conn
                .idle_wait(self.idle_reissue, self.cancel.clone())
                .await?;
            conn = returned;
            self.note_idle_alive().await;
            match outcome {
                IdleOutcome::NewMessages => {
                    last_uid = self.drain_pending(&mut conn, last_uid).await?;
                }
                IdleOutcome::Timeout => {
                    debug!(
                        target: "plugin.email",
                        instance = %self.account_cfg.instance,
                        "IDLE reissue cycle"
                    );
                }
                IdleOutcome::Cancelled => {
                    let _ = conn.logout().await;
                    return Ok(());
                }
            }
        }
    }

    async fn poll_loop(&self, mut conn: ImapConnection, mut last_uid: u32) -> Result<()> {
        let mut ticker = tokio::time::interval(self.poll_fallback);
        ticker.tick().await; // first tick is immediate; consume so the
                             // next tick respects the interval.
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    last_uid = self.drain_pending(&mut conn, last_uid).await?;
                    self.note_poll().await;
                }
                _ = self.cancel.cancelled() => {
                    let _ = conn.logout().await;
                    return Ok(());
                }
            }
        }
    }

    /// Fetch every UID > `last_uid`, publish, persist cursor. Returns
    /// the new `last_uid`. Cursor is updated **after** publish so a
    /// crash mid-batch reprocesses the message rather than losing it
    /// (at-least-once semantics).
    async fn drain_pending(&self, conn: &mut ImapConnection, last_uid: u32) -> Result<u32> {
        let uids = conn.search_since(last_uid).await?;
        let mut highest = last_uid;
        for uid in uids {
            let msg = conn.fetch_uid(uid).await?;
            let event = InboundEvent {
                account_id: self.account_cfg.address.clone(),
                instance: self.account_cfg.instance.clone(),
                uid: msg.uid,
                internal_date: msg.internal_date,
                raw_bytes: msg.raw_bytes,
                meta: None,
                attachments: vec![],
            };
            let topic = inbound_topic_for(&self.account_cfg.instance);
            let payload = serde_json::to_value(&event)?;
            self.broker
                .publish(&topic, Event::new(topic.clone(), SOURCE, payload))
                .await?;
            highest = uid;
            self.note_message().await;
            // Persist cursor each message; cheap (sqlite WAL) and
            // tightens the at-least-once window on crash.
            let cursor = UidCursor {
                uid_validity: 0, // re-set below
                last_uid: highest,
                updated_at: chrono::Utc::now().timestamp(),
            };
            // Keep the existing uid_validity by reading the current
            // row before overwriting; cheap read amortised by sqlite
            // page cache.
            if let Some(existing) = self.cursor.get(&self.account_cfg.instance).await? {
                let merged = UidCursor {
                    uid_validity: existing.uid_validity,
                    ..cursor
                };
                self.cursor
                    .set(&self.account_cfg.instance, &merged)
                    .await?;
            }
        }
        Ok(highest)
    }

    async fn set_state(&self, state: WorkerState) {
        self.health.write().await.state = state;
    }

    async fn note_idle_alive(&self) {
        let mut h = self.health.write().await;
        h.last_idle_alive_ts = chrono::Utc::now().timestamp();
        h.state = WorkerState::Idle;
    }

    async fn note_poll(&self) {
        let mut h = self.health.write().await;
        h.last_poll_ts = chrono::Utc::now().timestamp();
    }

    async fn note_connect_ok(&self) {
        let mut h = self.health.write().await;
        h.last_connect_ok_ts = chrono::Utc::now().timestamp();
        h.consecutive_failures = 0;
        h.last_error = None;
    }

    async fn note_failure(&self, err: String) {
        let mut h = self.health.write().await;
        h.consecutive_failures = h.consecutive_failures.saturating_add(1);
        h.last_error = Some(err);
        h.state = WorkerState::Connecting;
    }

    async fn note_message(&self) {
        let mut h = self.health.write().await;
        h.messages_seen_total = h.messages_seen_total.saturating_add(1);
    }
}

/// Exponential backoff with ±20% jitter. Cap 60s. Attempt 1 → ~1s,
/// attempt 6 → ~32s, attempt 7+ → 60s±jitter.
fn jittered_backoff(attempt: u32) -> Duration {
    let base = INITIAL_BACKOFF_MS
        .saturating_mul(1u64 << attempt.min(6))
        .min(MAX_BACKOFF_MS);
    let jitter: f64 = rand::thread_rng().gen_range(0.8..1.2);
    Duration::from_millis(((base as f64) * jitter) as u64)
}

/// Sleep `dur` or wake on cancel. Returns `false` if cancelled (caller
/// should exit), `true` if the sleep elapsed normally.
async fn sleep_or_cancel(dur: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(dur) => true,
        _ = cancel.cancelled() => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially_then_caps() {
        // Drop jitter for the property: minimum is 0.8x base.
        let d1 = jittered_backoff(0);
        let d6 = jittered_backoff(6);
        let d10 = jittered_backoff(10);
        assert!(d1 < Duration::from_secs(2));
        assert!(d6 >= Duration::from_secs(20));
        // Cap holds even at high attempt count.
        assert!(d10 <= Duration::from_secs(80));
    }

    #[tokio::test]
    async fn sleep_or_cancel_returns_false_on_cancel() {
        let token = CancellationToken::new();
        token.cancel();
        let kept = sleep_or_cancel(Duration::from_secs(60), &token).await;
        assert!(!kept);
    }
}
