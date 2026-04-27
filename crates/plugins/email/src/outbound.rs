//! SMTP outbound dispatcher (Phase 48.4).
//!
//! One `OutboundWorker` per account. Lifecycle:
//!
//! 1. **Enqueue**: subscribe to `plugin.outbound.email.<instance>`,
//!    deserialize each `OutboundCommand`, generate a stable
//!    `Message-ID`, build text/plain MIME, append to the JSONL queue.
//! 2. **Drain tick** (every 1s): peek the queue, for each ready job
//!    take a `DashMap` "in-flight" guard, call SMTP, classify the
//!    outcome, update queue + ack.
//!
//! Retries: `2s, 4s, 8s, 16s, 30s` (cap) with ±20% jitter. Five
//! transient attempts → DLQ. Any 5xx → DLQ immediately.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use dashmap::DashMap;
use nexo_auth::email::EmailCredentialStore;
use nexo_auth::google::GoogleCredentialStore;
use nexo_broker::{AnyBroker, BrokerHandle, Event};
use nexo_config::types::plugins::{EmailAccountConfig, EmailPluginConfig};
use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig};
use rand::Rng;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::events::{AckStatus, OutboundAck, OutboundCommand};
use crate::health::AccountHealth;
use crate::inbound::HealthMap;
use crate::mime_build::{build_mime, generate_message_id, BuildContext};
use crate::outbound_queue::{OutboundJob, OutboundQueue, SmtpEnvelope};
use crate::plugin::outbound_topic_for;
use crate::smtp_conn::{SmtpClient, SmtpSendOutcome};

const SOURCE: &str = "plugin.email";
const DRAIN_INTERVAL_MS: u64 = 1_000;
const MAX_ATTEMPTS: u32 = 5;
const TRANSIENT_BACKOFF_BASE_MS: u64 = 2_000;
const TRANSIENT_BACKOFF_MAX_MS: u64 = 30_000;

/// Per-instance state shared between the spawned worker and the
/// `DispatcherHandle` impl that 48.7 tools call into.
pub struct InstanceState {
    queue: Arc<OutboundQueue>,
    address: String,
}

/// Cheap, `Arc`-able core that the outbound dispatcher shares with
/// the email tools. Holds only the per-instance map needed for
/// `enqueue_for_instance` — keeps the dispatcher's lifetime
/// (workers + cancel token) separate from the tool surface, so the
/// tool registry can outlive a `stop()` call without dangling.
pub struct DispatcherCore {
    instances: Arc<DashMap<String, Arc<InstanceState>>>,
}

impl DispatcherCore {
    pub fn instance_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.instances.iter().map(|e| e.key().clone()).collect();
        v.sort();
        v
    }

    pub async fn enqueue_for_instance(
        &self,
        instance: &str,
        cmd: OutboundCommand,
    ) -> Result<String> {
        // Audit #3 follow-up — refuse a command with no recipients.
        // Without this guard, lettre's `Envelope::new(Some(from),
        // vec![])` succeeds, the SMTP server rejects with "no
        // recipients", we classify the response as Transient and
        // retry MAX_ATTEMPTS times before DLQ — a tight retry loop
        // for a malformed payload that can never succeed. Catch it
        // at enqueue so the caller (tool, broker subscriber) gets a
        // clear error instead of phantom queue activity.
        if cmd.to.is_empty() && cmd.cc.is_empty() && cmd.bcc.is_empty() {
            anyhow::bail!(
                "outbound command for instance '{instance}' has no recipients (to / cc / bcc all empty); refusing to enqueue"
            );
        }
        let state = self
            .instances
            .get(instance)
            .ok_or_else(|| anyhow!("unknown email instance: {instance}"))?
            .clone();
        let from = state.address.clone();
        let message_id = generate_message_id(&from);
        let raw = build_mime(
            BuildContext {
                message_id: &message_id,
                from: &from,
                now: Utc::now(),
            },
            &cmd,
        )
        .await
        .with_context(|| format!("build outbound MIME for {message_id}"))?;
        let now = Utc::now().timestamp();
        let job = OutboundJob {
            message_id: message_id.clone(),
            instance: instance.to_string(),
            envelope: SmtpEnvelope {
                from,
                to: cmd.to,
                cc: cmd.cc,
                bcc: cmd.bcc,
            },
            raw_mime: raw,
            attempts: 0,
            next_attempt_at: now,
            last_error: None,
            created_at: now,
            done: false,
        };
        state.queue.enqueue(&job).await?;
        Ok(message_id)
    }
}

#[async_trait::async_trait]
impl crate::tool::DispatcherHandle for DispatcherCore {
    async fn enqueue_for_instance(
        &self,
        instance: &str,
        cmd: OutboundCommand,
    ) -> Result<String> {
        DispatcherCore::enqueue_for_instance(self, instance, cmd).await
    }

    fn instance_ids(&self) -> Vec<String> {
        DispatcherCore::instance_ids(self)
    }
}

/// Per-instance outbound worker handle. Cancel kills just one
/// account's drain loop; the parent dispatcher token is the union.
struct OutboundSlot {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

pub struct OutboundDispatcher {
    workers: std::collections::HashMap<String, OutboundSlot>,
    cancel: CancellationToken,
    core: Arc<DispatcherCore>,
    /// Shared scratch the per-account spawner reuses on
    /// `add_account`. Captured at `start` time.
    creds: Arc<EmailCredentialStore>,
    google: Arc<GoogleCredentialStore>,
    broker: AnyBroker,
    data_dir: std::path::PathBuf,
    health: HealthMap,
    max_dlq_lines: usize,
}

impl OutboundDispatcher {
    /// Spawn one worker per account. The shared `health` map is
    /// updated in-place so `EmailPlugin::health_map()` sees inbound +
    /// outbound counters merged into one `AccountHealth` row.
    pub async fn start(
        cfg: &EmailPluginConfig,
        creds: Arc<EmailCredentialStore>,
        google: Arc<GoogleCredentialStore>,
        broker: AnyBroker,
        data_dir: &Path,
        health: HealthMap,
    ) -> Result<Self> {
        let cancel = CancellationToken::new();
        let instances: Arc<DashMap<String, Arc<InstanceState>>> = Arc::new(DashMap::new());

        let mut dispatcher = Self {
            workers: std::collections::HashMap::new(),
            cancel: cancel.clone(),
            core: Arc::new(DispatcherCore {
                instances: instances.clone(),
            }),
            creds: creds.clone(),
            google: google.clone(),
            broker: broker.clone(),
            data_dir: data_dir.to_path_buf(),
            health: health.clone(),
            max_dlq_lines: cfg.max_dlq_lines,
        };
        for account_cfg in &cfg.accounts {
            dispatcher.add_account(account_cfg).await?;
        }
        Ok(dispatcher)
    }

    /// Phase 48 follow-up #5 — spawn one outbound worker for an
    /// account that arrived via a config reload. Idempotent on
    /// instance id; opens the queue file fresh each time.
    pub async fn add_account(&mut self, account_cfg: &EmailAccountConfig) -> Result<bool> {
        if self.workers.contains_key(&account_cfg.instance) {
            return Ok(false);
        }
        let queue_dir = self.data_dir.join("email").join("outbound");
        let queue = Arc::new(
            OutboundQueue::open(&queue_dir, &account_cfg.instance)
                .await
                .with_context(|| {
                    format!(
                        "email/outbound: open queue for instance '{}'",
                        account_cfg.instance
                    )
                })?,
        );
        self.core.instances.insert(
            account_cfg.instance.clone(),
            Arc::new(InstanceState {
                queue: queue.clone(),
                address: account_cfg.address.clone(),
            }),
        );
        let h = self
            .health
            .entry(account_cfg.instance.clone())
            .or_insert_with(|| Arc::new(RwLock::new(AccountHealth::default())))
            .clone();
        let breaker = Arc::new(CircuitBreaker::new(
            format!("email/{}/smtp", account_cfg.instance),
            CircuitBreakerConfig::default(),
        ));
        let per_instance_cancel = self.cancel.child_token();
        let worker = OutboundWorker {
            account_cfg: account_cfg.clone(),
            creds: self.creds.clone(),
            google: self.google.clone(),
            queue,
            breaker,
            broker: self.broker.clone(),
            health: h,
            cancel: per_instance_cancel.clone(),
            in_flight: Arc::new(DashMap::new()),
            max_dlq_lines: self.max_dlq_lines,
        };
        self.workers.insert(
            account_cfg.instance.clone(),
            OutboundSlot {
                handle: tokio::spawn(worker.run()),
                cancel: per_instance_cancel,
            },
        );
        Ok(true)
    }

    /// Phase 48 follow-up #5 — cancel one account's outbound
    /// worker without touching siblings. Removes the queue
    /// reference from `DispatcherCore.instances` so subsequent
    /// `enqueue_for_instance` calls fail with "unknown email
    /// instance" — operators removing an account expect that.
    pub async fn remove_account(&mut self, instance: &str) -> bool {
        let Some(slot) = self.workers.remove(instance) else {
            return false;
        };
        slot.cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), slot.handle).await;
        self.core.instances.remove(instance);
        true
    }

    /// Cheap shared handle the tool surface holds. Outlives `stop()`
    /// so a tool registry that retained a reference doesn't dangle.
    pub fn core(&self) -> Arc<DispatcherCore> {
        self.core.clone()
    }

    pub fn instance_ids(&self) -> Vec<String> {
        self.core.instance_ids()
    }

    pub async fn enqueue_for_instance(
        &self,
        instance: &str,
        cmd: OutboundCommand,
    ) -> Result<String> {
        self.core.enqueue_for_instance(instance, cmd).await
    }

    pub async fn stop(self) {
        self.cancel.cancel();
        let handles: Vec<_> = self.workers.into_values().map(|s| s.handle).collect();
        let join = futures::future::join_all(handles);
        let _ = tokio::time::timeout(Duration::from_secs(5), join).await;
    }
}

#[async_trait::async_trait]
impl crate::tool::DispatcherHandle for OutboundDispatcher {
    async fn enqueue_for_instance(
        &self,
        instance: &str,
        cmd: OutboundCommand,
    ) -> Result<String> {
        OutboundDispatcher::enqueue_for_instance(self, instance, cmd).await
    }

    fn instance_ids(&self) -> Vec<String> {
        OutboundDispatcher::instance_ids(self)
    }
}

struct OutboundWorker {
    account_cfg: EmailAccountConfig,
    creds: Arc<EmailCredentialStore>,
    google: Arc<GoogleCredentialStore>,
    queue: Arc<OutboundQueue>,
    breaker: Arc<CircuitBreaker>,
    broker: AnyBroker,
    health: Arc<RwLock<AccountHealth>>,
    cancel: CancellationToken,
    in_flight: Arc<DashMap<String, ()>>,
    /// Audit follow-up I — DLQ size cap. `0` disables; positive
    /// values trigger a trim during the idle compaction tick.
    max_dlq_lines: usize,
}

impl OutboundWorker {
    async fn run(self) {
        let instance = self.account_cfg.instance.clone();
        info!(target: "plugin.email", instance = %instance, "outbound worker spawned");

        let topic = outbound_topic_for(&instance);
        let mut sub = match self.broker.subscribe(&topic).await {
            Ok(s) => Some(s),
            Err(e) => {
                warn!(
                    target: "plugin.email",
                    instance = %instance,
                    error = %e,
                    "broker subscribe failed; running drain-only (queued sends only)"
                );
                None
            }
        };

        let mut ticker = tokio::time::interval(Duration::from_millis(DRAIN_INTERVAL_MS));
        ticker.tick().await; // consume immediate first tick

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    info!(target: "plugin.email", instance = %instance, "outbound worker cancelled");
                    return;
                }
                _ = ticker.tick() => {
                    if let Err(e) = self.drain_tick().await {
                        warn!(
                            target: "plugin.email",
                            instance = %instance,
                            error = %e,
                            "drain tick failed"
                        );
                    }
                    self.refresh_depth_health().await;
                }
                maybe_event = next_subscription(sub.as_mut()) => {
                    let Some(event) = maybe_event else {
                        // Subscription closed — keep ticking the queue
                        // (drain-only mode).
                        sub = None;
                        continue;
                    };
                    match serde_json::from_value::<OutboundCommand>(event.payload.clone()) {
                        Ok(cmd) => {
                            if let Err(e) = self.enqueue_command(cmd).await {
                                warn!(
                                    target: "plugin.email",
                                    instance = %instance,
                                    error = %e,
                                    "failed to enqueue outbound command"
                                );
                            }
                        }
                        Err(e) => warn!(
                            target: "plugin.email",
                            instance = %instance,
                            error = %e,
                            "outbound payload not parseable as OutboundCommand"
                        ),
                    }
                }
            }
        }
    }

    /// Generate a Message-ID, build MIME, persist a fresh job. Caller
    /// receives the message_id so a tool wrapping `email_send` (Phase
    /// 48.7) can return it to the agent.
    pub async fn enqueue_command(&self, cmd: OutboundCommand) -> Result<String> {
        let from = self.account_cfg.address.clone();
        let message_id = generate_message_id(&from);
        // Phase 48.5 — `build_mime` reads attachment files at this
        // point. Missing files surface here (not at drain time) so
        // the dispatcher can fail fast and ack `Failed` instead of
        // leaving a doomed job on the queue.
        let raw = build_mime(
            BuildContext {
                message_id: &message_id,
                from: &from,
                now: Utc::now(),
            },
            &cmd,
        )
        .await
        .with_context(|| format!("build outbound MIME for {message_id}"))?;
        let now = Utc::now().timestamp();
        let job = OutboundJob {
            message_id: message_id.clone(),
            instance: self.account_cfg.instance.clone(),
            envelope: SmtpEnvelope {
                from,
                to: cmd.to,
                cc: cmd.cc,
                bcc: cmd.bcc,
            },
            raw_mime: raw,
            attempts: 0,
            next_attempt_at: now,
            last_error: None,
            created_at: now,
            done: false,
        };
        self.queue.enqueue(&job).await?;
        Ok(message_id)
    }

    async fn drain_tick(&self) -> Result<()> {
        if !self.breaker.allow() {
            return Ok(());
        }
        let now = Utc::now().timestamp();
        let pending = self.queue.list_pending().await?;
        if pending.is_empty() {
            // Cheap compaction check while idle.
            let _ = self.queue.compact_if_needed().await;
            // Audit follow-up I — DLQ trim. Idle ticks are the
            // right place: trimming under load risks renaming the
            // file out from under a fresh `move_to_dlq` append.
            if let Ok(n) = self.queue.trim_dlq(self.max_dlq_lines).await {
                if n > 0 {
                    tracing::info!(
                        target: "plugin.email",
                        instance = %self.account_cfg.instance,
                        dropped = n,
                        cap = self.max_dlq_lines,
                        "DLQ trimmed (oldest entries dropped)"
                    );
                }
            }
            return Ok(());
        }

        for job in pending {
            if self.cancel.is_cancelled() {
                return Ok(());
            }
            if job.next_attempt_at > now {
                continue;
            }
            // Single-flight guard: if a previous tick is still
            // mid-send for this message_id, skip this round.
            if self.in_flight.insert(job.message_id.clone(), ()).is_some() {
                continue;
            }
            let outcome = self.send_one(&job).await;
            self.in_flight.remove(&job.message_id);
            self.handle_outcome(job, outcome).await?;
        }
        Ok(())
    }

    async fn send_one(&self, job: &OutboundJob) -> Result<SmtpSendOutcome> {
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
        let client =
            SmtpClient::build(&self.account_cfg.smtp, &creds, self.google.clone()).await?;
        let outcome = client.send_raw(&job.envelope, &job.raw_mime).await?;
        Ok(outcome)
    }

    async fn handle_outcome(&self, job: OutboundJob, outcome: Result<SmtpSendOutcome>) -> Result<()> {
        let (status, reason): (AckStatus, Option<String>) = match outcome {
            Ok(SmtpSendOutcome::Sent) => {
                self.breaker.on_success();
                self.queue.mark_done(&job.message_id).await?;
                {
                    let mut h = self.health.write().await;
                    h.outbound_sent_total = h.outbound_sent_total.saturating_add(1);
                }
                crate::metrics::inc_outbound_sent(&self.account_cfg.instance);
                (AckStatus::Sent, None)
            }
            Ok(SmtpSendOutcome::Permanent { code, message }) => {
                self.breaker.on_success(); // server reply, not a network failure
                self.queue.move_to_dlq(&job).await?;
                {
                    let mut h = self.health.write().await;
                    h.outbound_failed_total = h.outbound_failed_total.saturating_add(1);
                }
                crate::metrics::inc_outbound_failed(&self.account_cfg.instance);
                (AckStatus::Failed, Some(format!("smtp {code}: {message}")))
            }
            Ok(SmtpSendOutcome::Transient { code, message }) => {
                self.breaker.on_success();
                let next_attempts = job.attempts.saturating_add(1);
                if next_attempts >= MAX_ATTEMPTS {
                    self.queue.move_to_dlq(&job).await?;
                    let mut h = self.health.write().await;
                    h.outbound_failed_total = h.outbound_failed_total.saturating_add(1);
                    crate::metrics::inc_outbound_failed(&self.account_cfg.instance);
                    (
                        AckStatus::Failed,
                        Some(format!(
                            "smtp {code} after {next_attempts} attempts: {message}"
                        )),
                    )
                } else {
                    let mut updated = job.clone();
                    updated.attempts = next_attempts;
                    let backoff_ms = transient_backoff_ms(next_attempts);
                    updated.next_attempt_at =
                        Utc::now().timestamp() + (backoff_ms / 1000) as i64;
                    updated.last_error = Some(format!("smtp {code}: {message}"));
                    self.queue.update(&updated).await?;
                    (
                        AckStatus::Retrying,
                        Some(format!("smtp {code} (attempt {next_attempts}): {message}")),
                    )
                }
            }
            Err(e) => {
                // Network / TLS / build error — counts against the CB.
                self.breaker.on_failure();
                let next_attempts = job.attempts.saturating_add(1);
                if next_attempts >= MAX_ATTEMPTS {
                    self.queue.move_to_dlq(&job).await?;
                    let mut h = self.health.write().await;
                    h.outbound_failed_total = h.outbound_failed_total.saturating_add(1);
                    crate::metrics::inc_outbound_failed(&self.account_cfg.instance);
                    (AckStatus::Failed, Some(format!("network: {e:#}")))
                } else {
                    let mut updated = job.clone();
                    updated.attempts = next_attempts;
                    let backoff_ms = transient_backoff_ms(next_attempts);
                    updated.next_attempt_at =
                        Utc::now().timestamp() + (backoff_ms / 1000) as i64;
                    updated.last_error = Some(format!("network: {e:#}"));
                    self.queue.update(&updated).await?;
                    (AckStatus::Retrying, Some(format!("network: {e:#}")))
                }
            }
        };

        let ack = OutboundAck {
            message_id: job.message_id.clone(),
            status,
            reason,
        };
        let topic = format!("{}.ack", outbound_topic_for(&self.account_cfg.instance));
        let payload = serde_json::to_value(&ack)?;
        if let Err(e) = self
            .broker
            .publish(&topic, Event::new(topic.clone(), SOURCE, payload))
            .await
        {
            debug!(
                target: "plugin.email",
                instance = %self.account_cfg.instance,
                error = %e,
                "ack publish failed (broker disconnected?)"
            );
        }
        Ok(())
    }

    async fn refresh_depth_health(&self) {
        let pending = self.queue.pending_count().await.unwrap_or(0);
        let dlq = self.queue.dlq_count().await.unwrap_or(0);
        let mut h = self.health.write().await;
        h.outbound_queue_depth = pending;
        h.outbound_dlq_depth = dlq;
    }
}

/// `2s, 4s, 8s, 16s, 32s capped at 30s`, ±20% jitter. `attempts` is
/// the count we're scheduling for (i.e. 1 after first failure).
fn transient_backoff_ms(attempts: u32) -> u64 {
    let exp = TRANSIENT_BACKOFF_BASE_MS
        .saturating_mul(1u64 << attempts.saturating_sub(1).min(5))
        .min(TRANSIENT_BACKOFF_MAX_MS);
    let jitter: f64 = rand::thread_rng().gen_range(0.8..1.2);
    ((exp as f64) * jitter) as u64
}

/// Drive a `Subscription` if present. Returns `None` if the
/// subscription is gone (caller should switch to drain-only mode).
async fn next_subscription(
    sub: Option<&mut nexo_broker::Subscription>,
) -> Option<Event> {
    match sub {
        Some(s) => s.next().await,
        None => {
            // Park forever so `tokio::select!` doesn't busy-loop on
            // this branch. The other arms (cancel, ticker) drive the
            // loop.
            std::future::pending::<()>().await;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_backoff_grows_then_caps() {
        let one = transient_backoff_ms(1);
        let two = transient_backoff_ms(2);
        let big = transient_backoff_ms(10);
        assert!(one >= 1_600 && one <= 2_400, "got {one}");
        assert!(two >= 3_200 && two <= 4_800, "got {two}");
        assert!(big <= 36_000, "cap exceeded: {big}");
    }
}
