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
use crate::imap_conn::{IdleOutcome, ImapConnection};
use crate::plugin::inbound_topic_for;

const SOURCE: &str = "plugin.email";
const INITIAL_BACKOFF_MS: u64 = 1_000;
const MAX_BACKOFF_MS: u64 = 60_000;

/// Map of `instance -> AccountHealth`. Cloned `Arc` is what
/// `EmailPlugin::health()` reads (Phase 48.10).
pub type HealthMap = Arc<DashMap<String, Arc<RwLock<AccountHealth>>>>;

/// Per-instance worker handle. Cancel kills just this account's
/// IDLE loop; the parent plugin-level token is the union.
struct WorkerSlot {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

pub struct InboundManager {
    workers: std::collections::HashMap<String, WorkerSlot>,
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
        bounce_store: Option<Arc<crate::bounce_store::BounceStore>>,
        attachment_store: Option<Arc<crate::attachment_store::AttachmentStore>>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let health: HealthMap = Arc::new(DashMap::new());
        let mut mgr = Self {
            workers: std::collections::HashMap::new(),
            cancel: cancel.clone(),
            health,
        };
        for account_cfg in &cfg.accounts {
            mgr.add_account(
                cfg,
                account_cfg,
                creds.clone(),
                google.clone(),
                cursor.clone(),
                broker.clone(),
                bounce_store.clone(),
                attachment_store.clone(),
            );
        }
        mgr
    }

    /// Cancel every worker and join them with a 5s upper bound. Workers
    /// that don't drain in time are dropped — IMAP servers tolerate the
    /// abort, the daemon must shut down promptly.
    pub async fn stop(self) {
        self.cancel.cancel();
        let handles: Vec<_> = self.workers.into_values().map(|w| w.handle).collect();
        let join = futures::future::join_all(handles);
        let _ = tokio::time::timeout(Duration::from_secs(5), join).await;
    }

    /// Phase 48 follow-up #5 — cancel one account's worker without
    /// touching siblings. Returns `true` when a worker was found
    /// and cancelled. The handle is awaited up to 5 s; a worker
    /// stuck in IDLE that doesn't unwind in time is dropped (the
    /// IMAP server tolerates the abort, same as `stop`).
    pub async fn remove_account(&mut self, instance: &str) -> bool {
        let Some(slot) = self.workers.remove(instance) else {
            return false;
        };
        slot.cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), slot.handle).await;
        self.health.remove(instance);
        true
    }

    pub fn health_map(&self) -> HealthMap {
        self.health.clone()
    }

    /// Phase 48 follow-up #5 — spawn one worker for an account that
    /// arrived via a config reload. Idempotent on the wire side: if
    /// the worker for `instance` is already alive (health entry
    /// present), the call is a noop. Caller is responsible for
    /// computing the diff via `compute_account_diff`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_account(
        &mut self,
        cfg: &EmailPluginConfig,
        account_cfg: &EmailAccountConfig,
        creds: Arc<EmailCredentialStore>,
        google: Arc<GoogleCredentialStore>,
        cursor: Arc<CursorStore>,
        broker: AnyBroker,
        bounce_store: Option<Arc<crate::bounce_store::BounceStore>>,
        attachment_store: Option<Arc<crate::attachment_store::AttachmentStore>>,
    ) -> bool {
        if self.workers.contains_key(&account_cfg.instance) {
            return false;
        }
        let h = Arc::new(RwLock::new(AccountHealth::default()));
        self.health.insert(account_cfg.instance.clone(), h.clone());
        let per_instance_cancel = self.cancel.child_token();
        let worker = AccountWorker {
            account_cfg: account_cfg.clone(),
            creds,
            google,
            cursor,
            broker,
            health: h,
            cancel: per_instance_cancel.clone(),
            idle_reissue: Duration::from_secs(cfg.idle_reissue_minutes * 60),
            poll_fallback: Duration::from_secs(cfg.poll_fallback_seconds),
            inbox_folder: account_cfg.folders.inbox.clone(),
            max_body_bytes: cfg.max_body_bytes,
            max_attachment_bytes: cfg.max_attachment_bytes,
            attachments_dir: std::path::PathBuf::from(&cfg.attachments_dir),
            loop_prevention: cfg.loop_prevention.clone(),
            bounce_store,
            attachment_store,
        };
        self.workers.insert(
            account_cfg.instance.clone(),
            WorkerSlot {
                handle: tokio::spawn(worker.run()),
                cancel: per_instance_cancel,
            },
        );
        true
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
    /// Phase 48.5 — MIME enrichment knobs cloned from the plugin
    /// config so the worker doesn't need to reach back into the
    /// shared cfg (and so hot-reload can swap workers cleanly).
    max_body_bytes: usize,
    max_attachment_bytes: usize,
    attachments_dir: std::path::PathBuf,
    /// Phase 48.8 — Loop-prevention toggles cloned from the plugin
    /// config so the worker doesn't reach back into shared state on
    /// the hot path.
    loop_prevention: nexo_config::types::plugins::LoopPreventionCfg,
    /// Phase 48 follow-up #4 — persistent bounce history. `None`
    /// when the operator hasn't opted in; populated by
    /// `EmailPlugin::start` when the SQLite file is reachable.
    bounce_store: Option<std::sync::Arc<crate::bounce_store::BounceStore>>,
    /// Phase 48 follow-up #10 — attachment ref-counter for GC.
    attachment_store: Option<std::sync::Arc<crate::attachment_store::AttachmentStore>>,
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
            .ok_or_else(|| {
                anyhow!(
                    "no credentials for instance '{}'",
                    self.account_cfg.instance
                )
            })?
            .clone();

        let mut conn =
            ImapConnection::connect(&self.account_cfg.imap, &creds, self.google.clone()).await?;
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
            // Phase 48.5 — best-effort MIME parse. Failures degrade
            // to a raw-only publish so a malformed message never
            // wedges the worker.
            let parse_cfg = crate::mime_parse::ParseConfig {
                max_body_bytes: self.max_body_bytes,
                max_attachment_bytes: self.max_attachment_bytes,
                attachments_dir: self.attachments_dir.clone(),
                fallback_internal_date: msg.internal_date,
            };
            let (meta, attachments, thread_root_id) =
                match crate::mime_parse::parse_eml(&msg.raw_bytes, &parse_cfg).await {
                    Ok(p) => {
                        // Phase 48.6 — derive a stable thread root so
                        // downstream tools can group by hilo without
                        // re-parsing meta.
                        let root = crate::threading::resolve_thread_root(
                            &p.meta,
                            msg.uid,
                            &self.account_cfg.address,
                        );
                        // Phase 48 follow-up #10 — bump ref-count for
                        // every attachment so the GC sweep knows the
                        // file is still live. Best-effort: storage
                        // failures don't block delivery.
                        if let Some(store) = &self.attachment_store {
                            for att in &p.attachments {
                                if let Err(e) = store.record(&att.sha256).await {
                                    warn!(
                                        target: "plugin.email",
                                        sha256 = %att.sha256,
                                        error = %e,
                                        "attachment_store record failed (continuing)"
                                    );
                                }
                            }
                        }
                        (Some(p.meta), p.attachments, Some(root))
                    }
                    Err(e) => {
                        crate::metrics::inc_parse_error(&self.account_cfg.instance);
                        warn!(
                            target: "plugin.email",
                            instance = %self.account_cfg.instance,
                            uid = msg.uid,
                            error = %e,
                            "email.parse.malformed — publishing raw-only"
                        );
                        (None, Vec::new(), None)
                    }
                };
            // Phase 48.8 — DSN check first. A delivery report should
            // emit a `BounceEvent` (operational signal) and NOT
            // surface as conversational `InboundEvent`.
            let mut suppressed = false;
            if let Some(meta_ref) = meta.as_ref() {
                if let Some(parsed) =
                    crate::dsn::parse_bounce(meta_ref, &msg.raw_bytes, &self.account_cfg.address)
                {
                    let bounce = crate::dsn::BounceEvent {
                        account_id: self.account_cfg.address.clone(),
                        instance: self.account_cfg.instance.clone(),
                        original_message_id: parsed.original_message_id,
                        recipient: parsed.recipient,
                        status_code: parsed.status_code,
                        action: parsed.action,
                        reason: parsed.reason,
                        classification: parsed.classification,
                    };
                    // Persist to the bounce store before publishing so
                    // a downstream `email_send` retry that races the
                    // broker hop still sees the new row.
                    if let Some(store) = &self.bounce_store {
                        if let Err(e) = store.record(&bounce).await {
                            warn!(
                                target: "plugin.email",
                                instance = %self.account_cfg.instance,
                                error = %e,
                                "bounce_store record failed (continuing)"
                            );
                        }
                    }
                    let topic = format!("email.bounce.{}", self.account_cfg.instance);
                    if let Ok(payload) = serde_json::to_value(&bounce) {
                        if let Err(e) = self
                            .broker
                            .publish(&topic, Event::new(topic.clone(), SOURCE, payload))
                            .await
                        {
                            warn!(
                                target: "plugin.email",
                                instance = %self.account_cfg.instance,
                                error = %e,
                                "bounce publish failed (continuing — cursor advances)"
                            );
                        }
                    }
                    info!(
                        target: "plugin.email",
                        instance = %self.account_cfg.instance,
                        uid = msg.uid,
                        reason = "dsn_inbound",
                        "email.loop_skip"
                    );
                    crate::metrics::inc_loop_skipped("dsn_inbound");
                    crate::metrics::inc_bounce(&self.account_cfg.instance, bounce.classification);
                    suppressed = true;
                } else if let Some(reason) = crate::loop_prevent::should_skip(
                    meta_ref,
                    &self.account_cfg.address,
                    &self.loop_prevention,
                ) {
                    info!(
                        target: "plugin.email",
                        instance = %self.account_cfg.instance,
                        uid = msg.uid,
                        reason = %reason.metric_label(),
                        "email.loop_skip"
                    );
                    crate::metrics::inc_loop_skipped(reason.metric_label());
                    suppressed = true;
                }
            }

            if !suppressed {
                let event = InboundEvent {
                    account_id: self.account_cfg.address.clone(),
                    instance: self.account_cfg.instance.clone(),
                    uid: msg.uid,
                    internal_date: msg.internal_date,
                    raw_bytes: msg.raw_bytes,
                    meta,
                    attachments,
                    thread_root_id,
                };
                let topic = inbound_topic_for(&self.account_cfg.instance);
                let payload = serde_json::to_value(&event)?;
                self.broker
                    .publish(&topic, Event::new(topic.clone(), SOURCE, payload))
                    .await?;
                self.note_message().await;
                crate::metrics::inc_messages_fetched(&self.account_cfg.instance);
            }
            highest = uid;
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
                self.cursor.set(&self.account_cfg.instance, &merged).await?;
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
