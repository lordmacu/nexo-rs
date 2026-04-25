//! `PollerRunner` — owns the registry of `Poller` impls + the per-job
//! tokio tasks. One task per job, lease-acquired, schedule-driven.
//!
//! Step 8 builds the core happy path: spawn → tick → save → sleep
//! → tick. Backoff + breaker + auto-pause + hot-reload are layered on
//! in steps 9 and 10.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nexo_auth::CredentialsBundle;
use nexo_broker::AnyBroker;
use nexo_config::types::pollers::{PollerJob, PollersConfig};
use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_yaml::Value as YamlValue;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::dispatch;
use crate::error::{ErrorClass, PollerError};
use crate::poller::{PollContext, Poller, TickOutcome};
use crate::schedule::{apply_jitter, Schedule};
use crate::state::PollState;
use crate::telemetry;

/// Default config for the per-job circuit breaker. Threshold lives in
/// PollersConfig; we use the resilience crate's exponential backoff.
fn default_breaker(threshold: u32) -> CircuitBreakerConfig {
    CircuitBreakerConfig {
        failure_threshold: threshold.max(1),
        success_threshold: 1,
        initial_backoff: Duration::from_secs(30),
        max_backoff: Duration::from_secs(300),
    }
}

/// What `reload` will do. Caller can preview by calling `diff` first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReloadPlan {
    pub add: Vec<String>,
    pub replace: Vec<String>,
    pub remove: Vec<String>,
    pub keep: Vec<String>,
}

/// True when both jobs would spawn an identical task. Hash-light:
/// compare the fields the runner cares about. Module-specific knobs
/// inside `config` are compared via serde_yaml::Value equality.
fn same_shape(a: &PollerJob, b: &PollerJob) -> bool {
    a.kind == b.kind
        && a.agent == b.agent
        && a.schedule == b.schedule
        && a.config == b.config
        && a.failure_to.as_ref().map(|t| (&t.channel, &t.to))
            == b.failure_to.as_ref().map(|t| (&t.channel, &t.to))
        && a.paused_on_boot == b.paused_on_boot
}

/// One running task per job — keep its handle + a cancel token so
/// hot-reload can cancel a single job without touching the rest.
struct JobTask {
    /// Kept for hot-reload diff (paso 10) — compares the new config
    /// against the running task's current job to decide
    /// keep/replace/drop. Marked `allow(dead_code)` until step 10
    /// reads it.
    #[allow(dead_code)]
    job: Arc<PollerJob>,
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

pub struct PollerRunner {
    /// Registry of impls, keyed by `Poller::kind()`.
    pollers: Arc<DashMap<&'static str, Arc<dyn Poller>>>,
    /// Live tasks keyed by `job.id`.
    tasks: Arc<Mutex<HashMap<String, JobTask>>>,
    cfg: Arc<Mutex<PollersConfig>>,
    state: Arc<PollState>,
    broker: AnyBroker,
    credentials: Arc<CredentialsBundle>,
    /// Phase 20 — optional LLM access for the `agent_turn` built-in.
    /// Boot wires this from the same `LlmRegistry` + `LlmConfig` the
    /// agent runtimes use; pollers without LLM-driven kinds leave
    /// it `None` and pay no cost.
    llm_registry: Option<Arc<nexo_llm::LlmRegistry>>,
    llm_config: Option<Arc<nexo_config::LlmConfig>>,
    leaseholder: String,
    shutdown: CancellationToken,
}

impl PollerRunner {
    pub fn new(
        cfg: PollersConfig,
        state: Arc<PollState>,
        broker: AnyBroker,
        credentials: Arc<CredentialsBundle>,
    ) -> Self {
        let nonce = uuid_v4_short();
        let leaseholder = format!("pid-{}-{nonce}", std::process::id());
        Self {
            pollers: Arc::new(DashMap::new()),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            cfg: Arc::new(Mutex::new(cfg)),
            state,
            broker,
            credentials,
            llm_registry: None,
            llm_config: None,
            leaseholder,
            shutdown: CancellationToken::new(),
        }
    }

    /// Phase 20 — wire the LLM registry + config so the `agent_turn`
    /// built-in can build clients on demand. Optional: pollers that
    /// only do data ingestion (gmail, rss, webhook) work without it.
    pub fn with_llm(
        mut self,
        registry: Arc<nexo_llm::LlmRegistry>,
        config: Arc<nexo_config::LlmConfig>,
    ) -> Self {
        self.llm_registry = Some(registry);
        self.llm_config = Some(config);
        self
    }

    /// Register a built-in or custom `Poller`. Idempotent — re-register
    /// of the same `kind` replaces the previous impl. Tests rely on
    /// this to inject mocks.
    pub fn register(&self, poller: Arc<dyn Poller>) {
        let kind = poller.kind();
        self.pollers.insert(kind, poller);
    }

    pub fn registered_kinds(&self) -> Vec<&'static str> {
        self.pollers.iter().map(|e| *e.key()).collect()
    }

    /// Credential bundle (resolver + stores + breakers). Custom-tool
    /// handlers use this to look up an agent's Google handle without
    /// a `PollContext` (tools fire from the LLM loop, not a tick).
    pub fn credentials(&self) -> Arc<CredentialsBundle> {
        Arc::clone(&self.credentials)
    }

    /// Walk every registered Poller and collect its `custom_tools()`.
    /// Adapter in `nexo-poller-tools` consumes this and registers
    /// each spec as a `ToolHandler` per agent.
    pub fn collect_custom_tools(&self) -> Vec<crate::CustomToolSpec> {
        let mut out = Vec::new();
        for p in self.pollers.iter() {
            for spec in p.value().custom_tools() {
                out.push(spec);
            }
        }
        out
    }

    /// Snapshot every configured job + its current persisted state for
    /// the admin endpoint.
    pub async fn list_jobs(&self) -> Result<Vec<crate::admin::JobView>> {
        let cfg = self.cfg.lock().await.clone();
        let mut out = Vec::with_capacity(cfg.jobs.len());
        for j in &cfg.jobs {
            let snap = self.state.load(&j.id).await?.unwrap_or_default();
            out.push(crate::admin::JobView {
                id: j.id.clone(),
                kind: j.kind.clone(),
                agent: j.agent.clone(),
                paused: snap.paused,
                last_run_at_ms: snap.last_run_at_ms,
                next_run_at_ms: snap.next_run_at_ms,
                last_status: snap.last_status,
                last_error: snap.last_error,
                consecutive_errors: snap.consecutive_errors,
                items_seen_total: snap.items_seen_total,
                items_dispatched_total: snap.items_dispatched_total,
            });
        }
        Ok(out)
    }

    pub async fn set_paused(&self, job_id: &str, paused: bool) -> Result<()> {
        self.assert_known(job_id).await?;
        self.state.set_paused(job_id, paused, now_ms()).await
    }

    pub async fn reset_cursor(&self, job_id: &str) -> Result<()> {
        self.assert_known(job_id).await?;
        self.state.reset_cursor(job_id, now_ms()).await
    }

    async fn assert_known(&self, job_id: &str) -> Result<()> {
        let cfg = self.cfg.lock().await;
        if cfg.jobs.iter().any(|j| j.id == job_id) {
            Ok(())
        } else {
            Err(anyhow::anyhow!("unknown job '{job_id}'"))
        }
    }

    /// Boot path. Validates every configured job, persists `paused_on_boot`,
    /// then spawns a task per job. Errors here fail boot loud — the
    /// operator wants to see misconfigured jobs immediately.
    pub async fn start(&self) -> Result<()> {
        let cfg = self.cfg.lock().await.clone();
        if !cfg.enabled {
            info!("pollers: subsystem disabled (pollers.enabled=false)");
            return Ok(());
        }
        info!(jobs = cfg.jobs.len(), "pollers: starting");
        let now_ms = now_ms();
        for job in &cfg.jobs {
            self.validate_job(job)
                .with_context(|| format!("validate job '{}'", job.id))?;
            if job.paused_on_boot {
                self.state.set_paused(&job.id, true, now_ms).await.ok();
            }
        }
        for job in cfg.jobs.iter().cloned() {
            self.spawn_job(Arc::new(job), &cfg).await;
        }
        Ok(())
    }

    fn validate_job(&self, job: &PollerJob) -> Result<()> {
        let kind = self.pollers.get(job.kind.as_str()).ok_or_else(|| {
            let known: Vec<_> = self.pollers.iter().map(|e| *e.key()).collect();
            anyhow::anyhow!(
                "job '{}' uses unknown kind '{}' — registered kinds: {known:?}",
                job.id,
                job.kind
            )
        })?;
        // schedule must parse
        let _: Schedule = serde_yaml::from_value(job.schedule.clone())
            .with_context(|| format!("invalid schedule for '{}'", job.id))?;
        // module-specific validation
        let cfg_json: serde_json::Value = yaml_to_json(&job.config);
        kind.validate(&cfg_json)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(())
    }

    async fn spawn_job(&self, job: Arc<PollerJob>, cfg: &PollersConfig) {
        let cancel = self.shutdown.child_token();
        let kind = match self.pollers.get(job.kind.as_str()) {
            Some(p) => Arc::clone(&p),
            None => {
                warn!(job = %job.id, kind = %job.kind, "kind not registered, skipping");
                return;
            }
        };
        let schedule: Schedule = match serde_yaml::from_value(job.schedule.clone()) {
            Ok(s) => s,
            Err(e) => {
                warn!(job = %job.id, error = %e, "invalid schedule, skipping");
                return;
            }
        };
        let ctx_cfg: serde_json::Value = yaml_to_json(&job.config);

        let runner_ctx = TaskCtx {
            job: Arc::clone(&job),
            kind,
            schedule,
            ctx_cfg,
            state: Arc::clone(&self.state),
            broker: self.broker.clone(),
            credentials: Arc::clone(&self.credentials),
            leaseholder: self.leaseholder.clone(),
            cancel: cancel.clone(),
            cfg: cfg.clone(),
            breaker: Arc::new(CircuitBreaker::new(
                format!("poller:{}", job.id),
                default_breaker(cfg.breaker_threshold),
            )),
            llm_registry: self.llm_registry.clone(),
            llm_config: self.llm_config.clone(),
        };

        let handle = tokio::spawn(run_job_loop(runner_ctx));
        self.tasks
            .lock()
            .await
            .insert(job.id.clone(), JobTask { job, cancel, handle });
    }

    /// Cancel + join every task, in parallel. Caller awaits.
    pub async fn shutdown(&self) -> Result<()> {
        self.shutdown.cancel();
        let mut tasks = self.tasks.lock().await;
        let drain: Vec<_> = tasks.drain().collect();
        drop(tasks);
        for (id, t) in drain {
            t.cancel.cancel();
            if let Err(e) = t.handle.await {
                warn!(job = %id, error = %e, "task join failed");
            }
        }
        Ok(())
    }

    /// Diff a fresh `PollersConfig` against the running set.
    /// Returns the apply plan so callers can preview before executing.
    pub async fn diff(&self, new_cfg: &PollersConfig) -> ReloadPlan {
        let tasks = self.tasks.lock().await;
        let live: std::collections::HashSet<String> =
            tasks.keys().cloned().collect();
        let live_jobs: HashMap<String, Arc<PollerJob>> = tasks
            .iter()
            .map(|(k, v)| (k.clone(), Arc::clone(&v.job)))
            .collect();
        drop(tasks);

        let mut add = Vec::new();
        let mut replace = Vec::new();
        let mut keep = Vec::new();
        let mut remove: Vec<String> = live
            .iter()
            .cloned()
            .filter(|id| !new_cfg.jobs.iter().any(|j| &j.id == id))
            .collect();
        remove.sort();

        for j in &new_cfg.jobs {
            match live_jobs.get(&j.id) {
                None => add.push(j.id.clone()),
                Some(prev) => {
                    if same_shape(prev, j) {
                        keep.push(j.id.clone());
                    } else {
                        replace.push(j.id.clone());
                    }
                }
            }
        }
        add.sort();
        replace.sort();
        keep.sort();
        ReloadPlan {
            add,
            replace,
            remove,
            keep,
        }
    }

    /// Apply a fresh `PollersConfig` atomically: cancel removed jobs,
    /// replace shape-changed ones, leave keep set untouched.
    pub async fn reload(&self, new_cfg: PollersConfig) -> Result<ReloadPlan> {
        // Validate every new/replaced job before touching the live
        // task set — a typo should not knock siblings offline.
        for job in &new_cfg.jobs {
            self.validate_job(job)
                .with_context(|| format!("validate '{}'", job.id))?;
        }

        let plan = self.diff(&new_cfg).await;

        // Cancel + drop tasks that are gone or being replaced.
        let mut tasks = self.tasks.lock().await;
        for id in plan.remove.iter().chain(plan.replace.iter()) {
            if let Some(t) = tasks.remove(id) {
                t.cancel.cancel();
                drop(t.handle);
            }
        }
        // Swap config before spawning so the new tasks see the right
        // global knobs (jitter, breaker_threshold, …).
        *self.cfg.lock().await = new_cfg.clone();
        drop(tasks); // release before spawn_job re-locks

        let job_lookup: HashMap<&str, &PollerJob> =
            new_cfg.jobs.iter().map(|j| (j.id.as_str(), j)).collect();
        for id in plan.add.iter().chain(plan.replace.iter()) {
            if let Some(j) = job_lookup.get(id.as_str()) {
                self.spawn_job(Arc::new((*j).clone()), &new_cfg).await;
            }
        }
        Ok(plan)
    }

    /// Trigger a single tick out-of-band. Bypasses the schedule and
    /// the lease — caller assumes the job is otherwise idle. Used by
    /// `agent pollers run <id>` and admin endpoint.
    pub async fn run_once(&self, job_id: &str) -> Result<TickOutcome> {
        let cfg = self.cfg.lock().await.clone();
        let job = cfg
            .jobs
            .iter()
            .find(|j| j.id == job_id)
            .ok_or_else(|| anyhow::anyhow!("unknown job '{job_id}'"))?;
        let kind = self
            .pollers
            .get(job.kind.as_str())
            .ok_or_else(|| anyhow::anyhow!("kind '{}' not registered", job.kind))?
            .clone();
        let schedule: Schedule = serde_yaml::from_value(job.schedule.clone())?;
        let snapshot = self.state.load(job_id).await?.unwrap_or_default();
        let ctx = PollContext {
            job_id: job.id.clone(),
            agent_id: job.agent.clone(),
            kind: kind.kind(),
            credentials: Arc::clone(&self.credentials.resolver),
            stores: Some(Arc::new(self.credentials.stores.clone())),
            broker: self.broker.clone(),
            now: Utc::now(),
            cursor: snapshot.cursor.clone(),
            config: yaml_to_json(&job.config),
            interval_hint: schedule.nominal_interval(),
            cancel: self.shutdown.child_token(),
            llm_registry: self.llm_registry.clone(),
            llm_config: self.llm_config.clone(),
        };
        let started = std::time::Instant::now();
        let result = kind.tick(&ctx).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        telemetry::observe_latency(kind.kind(), &job.agent, &job.id, elapsed_ms);
        match result {
            Ok(outcome) => {
                telemetry::inc_tick(kind.kind(), &job.agent, &job.id, "ok");
                telemetry::add_items_seen(kind.kind(), &job.agent, &job.id, outcome.items_seen);
                telemetry::add_items_dispatched(
                    kind.kind(),
                    &job.agent,
                    &job.id,
                    outcome.items_dispatched,
                );
                for d in &outcome.deliver {
                    if let Err(e) =
                        dispatch::publish(&self.broker, &self.credentials.resolver, &job.agent, d)
                            .await
                    {
                        warn!(job = %job.id, error = %e, "dispatch failed");
                    }
                }
                Ok(outcome)
            }
            Err(e) => Err(anyhow::anyhow!("{e}")),
        }
    }
}

/// Per-task context: everything the spawned future needs.
struct TaskCtx {
    job: Arc<PollerJob>,
    kind: Arc<dyn Poller>,
    schedule: Schedule,
    ctx_cfg: serde_json::Value,
    state: Arc<PollState>,
    broker: AnyBroker,
    credentials: Arc<CredentialsBundle>,
    leaseholder: String,
    cancel: CancellationToken,
    cfg: PollersConfig,
    breaker: Arc<CircuitBreaker>,
    llm_registry: Option<Arc<nexo_llm::LlmRegistry>>,
    llm_config: Option<Arc<nexo_config::LlmConfig>>,
}

async fn run_job_loop(tctx: TaskCtx) {
    info!(
        job = %tctx.job.id,
        kind = %tctx.kind.kind(),
        agent = %tctx.job.agent,
        "poller: job task started"
    );

    loop {
        // Compute next firing instant. One-shot `at` schedules return
        // None when their target is in the past — honor that and exit
        // the loop.
        let now = Utc::now();
        let next = match tctx.schedule.next_run_at(now) {
            Ok(Some(t)) => t,
            Ok(None) => {
                info!(job = %tctx.job.id, "schedule produced no next run; exiting");
                return;
            }
            Err(e) => {
                warn!(job = %tctx.job.id, error = %e, "schedule eval failed; exiting");
                return;
            }
        };

        let jitter_ms = tctx
            .schedule
            .jitter_hint()
            .unwrap_or(tctx.cfg.default_jitter_ms);
        let next_with_jitter = apply_jitter(next, jitter_ms, rand_u64());
        let sleep_for = (next_with_jitter - now)
            .to_std()
            .unwrap_or(Duration::from_millis(0));

        // Wait until the next tick or until the runner shuts down /
        // the job is hot-reloaded.
        tokio::select! {
            _ = tokio::time::sleep(sleep_for) => {}
            _ = tctx.cancel.cancelled() => {
                debug!(job = %tctx.job.id, "task cancelled");
                return;
            }
        }

        // Skip ticks while paused. Paused jobs still respond to
        // shutdown / hot-reload immediately.
        if let Ok(Some(snap)) = tctx.state.load(&tctx.job.id).await {
            if snap.paused {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                    _ = tctx.cancel.cancelled() => return,
                }
            }
        }

        // Acquire the cross-process lease. Failure means another
        // worker is mid-tick; we skip and try again next slot.
        let now_ms_v = now_ms();
        let interval_secs = tctx.schedule.nominal_interval().as_secs().max(30);
        let ttl_ms =
            ((interval_secs as f32) * tctx.cfg.lease_ttl_factor.max(1.0)) as i64 * 1_000;
        let until_ms = now_ms_v + ttl_ms.max(30_000);
        match tctx
            .state
            .acquire_lease(&tctx.job.id, &tctx.leaseholder, until_ms, now_ms_v)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                debug!(job = %tctx.job.id, "lease busy, skipping tick");
                telemetry::inc_tick(tctx.kind.kind(), &tctx.job.agent, &tctx.job.id, "skipped");
                continue;
            }
            Err(e) => {
                warn!(job = %tctx.job.id, error = %e, "lease acquire failed");
                continue;
            }
        }

        // Skip the tick when the breaker is open. The breaker will
        // self-transition to half-open after its backoff window.
        if !tctx.breaker.allow() {
            telemetry::inc_tick(tctx.kind.kind(), &tctx.job.agent, &tctx.job.id, "skipped");
            telemetry::set_breaker_state(&tctx.job.id, telemetry::BreakerState::Open);
            tctx.state.release_lease(&tctx.job.id, &tctx.leaseholder).await.ok();
            continue;
        }

        // ── tick ──
        let snapshot = tctx
            .state
            .load(&tctx.job.id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let pctx = PollContext {
            job_id: tctx.job.id.clone(),
            agent_id: tctx.job.agent.clone(),
            kind: tctx.kind.kind(),
            credentials: Arc::clone(&tctx.credentials.resolver),
            stores: Some(Arc::new(tctx.credentials.stores.clone())),
            broker: tctx.broker.clone(),
            now: Utc::now(),
            cursor: snapshot.cursor.clone(),
            config: tctx.ctx_cfg.clone(),
            interval_hint: tctx.schedule.nominal_interval(),
            cancel: tctx.cancel.clone(),
            llm_registry: tctx.llm_registry.clone(),
            llm_config: tctx.llm_config.clone(),
        };
        let started = std::time::Instant::now();
        let outcome = tctx.kind.tick(&pctx).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        telemetry::observe_latency(tctx.kind.kind(), &tctx.job.agent, &tctx.job.id, elapsed_ms);

        let next_run_at_ms = (next + chrono::Duration::milliseconds(jitter_ms as i64))
            .timestamp_millis();

        match outcome {
            Ok(o) => {
                tctx.breaker.on_success();
                telemetry::set_breaker_state(&tctx.job.id, telemetry::BreakerState::Closed);
                telemetry::set_consecutive_errors(&tctx.job.id, 0);
                telemetry::inc_tick(tctx.kind.kind(), &tctx.job.agent, &tctx.job.id, "ok");
                telemetry::add_items_seen(
                    tctx.kind.kind(),
                    &tctx.job.agent,
                    &tctx.job.id,
                    o.items_seen,
                );
                telemetry::add_items_dispatched(
                    tctx.kind.kind(),
                    &tctx.job.agent,
                    &tctx.job.id,
                    o.items_dispatched,
                );
                for d in &o.deliver {
                    if let Err(e) =
                        dispatch::publish(&tctx.broker, &tctx.credentials.resolver, &tctx.job.agent, d)
                            .await
                    {
                        warn!(job = %tctx.job.id, error = %e, "dispatch failed");
                    }
                }
                let cursor_ref = o.next_cursor.as_deref();
                let _ = tctx
                    .state
                    .save_tick_ok(
                        &tctx.job.id,
                        cursor_ref,
                        o.items_seen,
                        o.items_dispatched,
                        elapsed_ms as i64,
                        next_run_at_ms,
                        now_ms(),
                    )
                    .await;
            }
            Err(e) => {
                handle_tick_error(&tctx, e, next_run_at_ms, elapsed_ms as i64).await;
            }
        }

        tctx.state.release_lease(&tctx.job.id, &tctx.leaseholder).await.ok();
    }
}

/// Handle a `PollerError` from `tick`: classify, update breaker,
/// persist state, and (when threshold hit) dispatch the failure
/// alert. Cooldown for the alert is honored from `poll_state`.
async fn handle_tick_error(
    tctx: &TaskCtx,
    err: PollerError,
    next_run_at_ms: i64,
    elapsed_ms: i64,
) {
    let class = err.classify();
    let msg = err.to_string();
    let status_label = match class {
        ErrorClass::Transient => "transient",
        ErrorClass::Permanent | ErrorClass::Config => "permanent",
    };

    tctx.breaker.on_failure();
    let breaker_state = if tctx.breaker.is_open() {
        telemetry::BreakerState::Open
    } else {
        telemetry::BreakerState::Closed
    };
    telemetry::set_breaker_state(&tctx.job.id, breaker_state);

    telemetry::inc_tick(tctx.kind.kind(), &tctx.job.agent, &tctx.job.id, status_label);

    let now_ms_v = now_ms();
    let _ = tctx
        .state
        .save_tick_err(
            &tctx.job.id,
            status_label,
            &msg,
            next_run_at_ms,
            elapsed_ms,
            now_ms_v,
            true,
        )
        .await;

    // Auto-pause on permanent failure.
    if matches!(class, ErrorClass::Permanent | ErrorClass::Config) {
        let _ = tctx.state.set_paused(&tctx.job.id, true, now_ms_v).await;
    }

    // Read the latest consecutive_errors back so the gauge matches DB.
    if let Ok(Some(snap)) = tctx.state.load(&tctx.job.id).await {
        telemetry::set_consecutive_errors(&tctx.job.id, snap.consecutive_errors);
        // Failure-destination alert when threshold crossed and cooldown elapsed.
        if let Some(target) = tctx.job.failure_to.as_ref() {
            let cooldown_ms = (tctx.cfg.failure_alert_cooldown_secs as i64) * 1_000;
            let last = snap.last_failure_alert_at_ms.unwrap_or(0);
            let cross = snap.consecutive_errors as u32 >= tctx.cfg.breaker_threshold;
            if cross && now_ms_v - last >= cooldown_ms {
                if let Err(e) = send_failure_alert(tctx, target, &msg).await {
                    warn!(job = %tctx.job.id, error = %e, "failure alert dispatch failed");
                }
                let _ = tctx.state.record_failure_alert(&tctx.job.id, now_ms_v).await;
            }
        }
    }
}

async fn send_failure_alert(
    tctx: &TaskCtx,
    target: &nexo_config::types::pollers::DeliveryTarget,
    error_text: &str,
) -> Result<()> {
    let channel: nexo_auth::Channel = match target.channel.as_str() {
        "whatsapp" => nexo_auth::handle::WHATSAPP,
        "telegram" => nexo_auth::handle::TELEGRAM,
        "google" => nexo_auth::handle::GOOGLE,
        other => anyhow::bail!("unknown failure_to.channel '{other}'"),
    };
    let payload = serde_json::json!({
        "text": format!(
            "⚠ poller `{}` (kind={}) failing: {}",
            tctx.job.id, tctx.kind.kind(), error_text
        ),
    });
    let delivery = crate::poller::OutboundDelivery {
        channel,
        recipient: target.to.clone(),
        payload,
    };
    dispatch::publish(&tctx.broker, &tctx.credentials.resolver, &tctx.job.agent, &delivery)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn rand_u64() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    SystemTime::now().hash(&mut h);
    std::process::id().hash(&mut h);
    h.finish()
}

fn uuid_v4_short() -> String {
    let v: u64 = rand_u64();
    format!("{v:016x}")
}

fn yaml_to_json(v: &YamlValue) -> serde_json::Value {
    serde_json::to_value(v)
        .ok()
        .or_else(|| serde_yaml::from_str(&serde_yaml::to_string(v).ok()?).ok())
        .unwrap_or(serde_json::Value::Null)
}

#[allow(dead_code)]
pub(crate) fn _force_unused_now_marker(_: DateTime<Utc>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poller::TickOutcome;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn empty_creds() -> Arc<CredentialsBundle> {
        Arc::new(CredentialsBundle {
            stores: nexo_auth::resolver::CredentialStores::empty(),
            resolver: Arc::new(nexo_auth::AgentCredentialResolver::empty()),
            breakers: Arc::new(nexo_auth::breaker::BreakerRegistry::default()),
            warnings: Vec::new(),
        })
    }

    struct MockPoller {
        ticks: Arc<AtomicU32>,
        next_outcome: Arc<Mutex<Result<TickOutcome, PollerError>>>,
    }

    #[async_trait]
    impl Poller for MockPoller {
        fn kind(&self) -> &'static str { "mock" }
        async fn tick(&self, _ctx: &PollContext) -> Result<TickOutcome, PollerError> {
            self.ticks.fetch_add(1, Ordering::Relaxed);
            let mut g = self.next_outcome.lock().await;
            std::mem::replace(
                &mut *g,
                Ok(TickOutcome::default()),
            )
        }
    }

    fn job(id: &str, every_secs: u64) -> PollerJob {
        PollerJob {
            id: id.into(),
            kind: "mock".into(),
            agent: "ana".into(),
            schedule: serde_yaml::from_str(&format!("every_secs: {every_secs}")).unwrap(),
            config: serde_yaml::Value::Null,
            failure_to: None,
            paused_on_boot: false,
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn registers_and_lists_kinds() {
        let cfg = PollersConfig::default();
        let state = Arc::new(PollState::open_in_memory().await.unwrap());
        let runner = PollerRunner::new(cfg, state, AnyBroker::local(), empty_creds());
        runner.register(Arc::new(MockPoller {
            ticks: Arc::new(AtomicU32::new(0)),
            next_outcome: Arc::new(Mutex::new(Ok(TickOutcome::default()))),
        }));
        assert_eq!(runner.registered_kinds(), vec!["mock"]);
    }

    #[tokio::test]
    async fn validate_rejects_unknown_kind() {
        let cfg = PollersConfig {
            jobs: vec![job("a", 60)],
            ..PollersConfig::default()
        };
        let state = Arc::new(PollState::open_in_memory().await.unwrap());
        let runner = PollerRunner::new(cfg.clone(), state, AnyBroker::local(), empty_creds());
        // No mock registered → validate fails.
        let err = runner.validate_job(&cfg.jobs[0]).unwrap_err();
        assert!(err.to_string().contains("unknown kind"));
    }

    #[tokio::test]
    async fn run_once_calls_tick_and_persists_cursor() {
        let cfg = PollersConfig {
            jobs: vec![job("a", 60)],
            ..PollersConfig::default()
        };
        let state = Arc::new(PollState::open_in_memory().await.unwrap());
        let ticks = Arc::new(AtomicU32::new(0));
        let outcome = TickOutcome {
            items_seen: 1,
            items_dispatched: 0,
            deliver: vec![],
            next_cursor: Some(b"c1".to_vec()),
            next_interval_hint: None,
        };
        let mock = Arc::new(MockPoller {
            ticks: Arc::clone(&ticks),
            next_outcome: Arc::new(Mutex::new(Ok(outcome))),
        });
        let runner = PollerRunner::new(cfg, Arc::clone(&state), AnyBroker::local(), empty_creds());
        runner.register(mock);
        let _ = runner.run_once("a").await.unwrap();
        assert_eq!(ticks.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn shutdown_joins_tasks_clean() {
        let cfg = PollersConfig::default();
        let state = Arc::new(PollState::open_in_memory().await.unwrap());
        let runner = PollerRunner::new(cfg, state, AnyBroker::local(), empty_creds());
        runner.shutdown().await.unwrap();
        // No tasks, no panic — that's the assertion.
    }
}
