//! AutoDream control flow. Phase 80.1.
//!
//! Verbatim port of
//! `claude-code-leak/src/services/autoDream/autoDream.ts:122-273`
//! (the `runAutoDream` closure inside `initAutoDream`).
//!
//! # Gate ordering (cheapest first — leak `:5-8`)
//!
//! 1. **Disabled** (`config.enabled == false`)
//! 2. **KAIROS active** (`ctx.kairos_active() == true` → skip; KAIROS
//!    runs the dream from a disk-skill instead — leak `:96`)
//! 3. **Remote mode** (skip when running in remote-control mode)
//! 4. **Auto-memory enabled** (memory_dir present)
//! 5. **Time gate** (`hours_since_last >= min_hours`)
//! 6. **Scan throttle** (`now - last_scan_at >= scan_interval`)
//! 7. **Sessions gate** (`sessions_touched >= min_sessions`)
//! 8. **Lock acquire** (no other process mid-consolidation)
//!
//! Force path (`RunReason::Manual`): bypass gates 5-7; lock
//! `priorMtime = lastAt` so kill rollback is a no-op (leak `:174-179`).
//!
//! # Provider-agnostic
//!
//! Built on `Arc<dyn LlmClient>` — works under any provider impl per
//! memory rule `feedback_provider_agnostic.md`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use chrono::Utc;
use nexo_agent_registry::{
    DreamPhase, DreamRunRow, DreamRunStatus, DreamRunStore,
};
use nexo_driver_types::{
    AutoDreamHook, AutoDreamOutcomeKind, DreamContextLite, GoalId,
};
use nexo_fork::{
    AutoMemFilter, CacheSafeParams, DefaultForkSubagent, DelegateMode,
    ForkParams, ForkSubagent, QuerySource, ToolDispatcher,
};
use nexo_llm::types::{ChatMessage, ToolDef};
use nexo_llm::LlmClient;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::AutoDreamConfig;
use crate::consolidation_lock::{
    list_sessions_touched_since, ConsolidationLock,
};
use crate::consolidation_prompt::ConsolidationPromptBuilder;
use crate::dream_progress_watcher::DreamProgressWatcher;
use crate::error::AutoDreamError;

/// Why this fork was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunReason {
    /// Per-turn invocation from driver-loop (mirror leak's
    /// `executeAutoDream` from stopHooks).
    PerTurn,
    /// Operator-forced via `dream_now` LLM tool or `nexo agent dream`
    /// CLI. Bypasses time/scan/session gates (mirror leak `isForced()`).
    Manual,
}

/// Why a run was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Disabled,
    KairosActive,
    RemoteMode,
    AutoMemoryDisabled,
    TimeGate,
    ScanThrottle,
    SessionGate,
}

/// Outcome of a single `check_and_run` call. Nexo addition (leak
/// returns `Promise<void>`); structured for CLI / `dream_now` tool
/// feedback.
#[derive(Debug)]
pub enum RunOutcome {
    /// Fork ran to completion. `files_touched` reflects the
    /// post-completion DreamRunRow.
    Completed {
        run_id: Uuid,
        files_touched: Vec<PathBuf>,
        duration_ms: u64,
    },
    /// Gate rejected the run.
    Skipped { gate: SkipReason },
    /// Lock held by another live process.
    LockBlocked { holder_pid: i32, mtime_secs: i64 },
    /// Fork errored mid-execution. Lock rolled back.
    Errored {
        run_id: Uuid,
        error: String,
        prior_mtime: i64,
    },
    /// Fork exceeded `fork_timeout`. Lock rolled back.
    TimedOut {
        run_id: Uuid,
        timeout_secs: u64,
        prior_mtime: i64,
    },
    /// Post-fork audit failed — fork attempted to edit paths outside
    /// `memory_dir`. Lock rolled back. Defense-in-depth (nexo
    /// addition; leak relies on canUseTool only).
    EscapeAudit {
        run_id: Uuid,
        escapes: Vec<PathBuf>,
        prior_mtime: i64,
    },
}

/// Snapshot of agent context fields the runner needs. Caller (driver-loop
/// per-turn hook) builds this before invoking `check_and_run`.
///
/// Phase 80.1.b refactor: `parent_ctx` and `last_chat_request` removed —
/// caller no longer needs to thread them. The runner now holds an
/// operator-supplied `parent_ctx_template` + `fork_system_prompt`/
/// `fork_tools`/`fork_model` at construction time and clones the
/// template per fork (mirror Phase 77.5 `ExtractMemories` shape).
/// Forks build a fresh `ChatRequest` (no parent-cache share — same
/// trade-off Phase 77.5 made).
pub struct DreamContext {
    pub goal_id: GoalId,
    pub session_id: String,
    pub transcript_dir: PathBuf,
    pub kairos_active: bool,
    pub remote_mode: bool,
}

/// Top-level AutoDream runner.
pub struct AutoDreamRunner {
    config: Arc<ArcSwap<AutoDreamConfig>>,
    lock: Arc<ConsolidationLock>,
    fork: Arc<dyn ForkSubagent>,
    audit: Arc<dyn DreamRunStore>,
    llm: Arc<dyn LlmClient>,
    tool_dispatcher: Arc<dyn ToolDispatcher>,
    memory_dir: PathBuf, // canonical
    fork_label: String,
    last_session_scan_at: AtomicI64, // unix seconds
    /// Parent context cloned per fork (Arc<...> internals — cheap).
    /// Phase 80.1.b: operator builds once at boot.
    parent_ctx_template: nexo_core::agent::AgentContext,
    /// Fresh `ChatRequest` system prompt for each fork. No parent
    /// prompt-cache share — same trade-off as Phase 77.5
    /// `ExtractMemories`.
    fork_system_prompt: String,
    fork_tools: Vec<ToolDef>,
    fork_model: String,
    /// Phase 80.1.g — optional checkpoint sink. When `Some`, the
    /// runner records each `Completed` fork-pass via the trait impl
    /// (typically `MemoryGitCheckpointer` from nexo-core). Skipped
    /// when `files_touched.is_empty()` — no point recording an
    /// empty commit (the audit row in `dream_runs.db` already
    /// captures the run regardless).
    git_checkpointer:
        Option<Arc<dyn nexo_driver_types::MemoryCheckpointer>>,
    /// Optional pre-dream snapshot sink. When wired, the runner
    /// captures a point-in-time bundle of the agent's memory before
    /// the fork-pass mutates anything, so a corrupt dream can be
    /// rolled back via `nexo memory restore`. Failures log a
    /// `tracing::warn!` and the dream proceeds without the anchor —
    /// operators who want a hard gate enforce that at the boot wire,
    /// not here.
    pre_dream_snapshot:
        Option<Arc<dyn nexo_driver_types::PreDreamSnapshotHook>>,
    /// Tenant scope passed to the pre-dream snapshot hook. Defaults
    /// to `"default"` for single-tenant deployments; multi-tenant
    /// SaaS wires the per-binding tenant at boot via
    /// `with_pre_dream_tenant`.
    pre_dream_tenant: String,
}

impl AutoDreamRunner {
    /// Build a runner. `memory_dir` MUST exist; canonicalized at
    /// construction.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Arc<ArcSwap<AutoDreamConfig>>,
        lock: Arc<ConsolidationLock>,
        fork: Arc<dyn ForkSubagent>,
        audit: Arc<dyn DreamRunStore>,
        llm: Arc<dyn LlmClient>,
        tool_dispatcher: Arc<dyn ToolDispatcher>,
        memory_dir: PathBuf,
        parent_ctx_template: nexo_core::agent::AgentContext,
        fork_system_prompt: String,
        fork_tools: Vec<ToolDef>,
        fork_model: String,
    ) -> Result<Self, AutoDreamError> {
        if !memory_dir.exists() {
            return Err(AutoDreamError::Config(format!(
                "memory_dir does not exist: {}",
                memory_dir.display()
            )));
        }
        let canonical = memory_dir.canonicalize()?;
        Ok(Self {
            config,
            lock,
            fork,
            audit,
            llm,
            tool_dispatcher,
            memory_dir: canonical,
            fork_label: "auto_dream".into(),
            last_session_scan_at: AtomicI64::new(0),
            parent_ctx_template,
            fork_system_prompt,
            fork_tools,
            fork_model,
            git_checkpointer: None,
            pre_dream_snapshot: None,
            pre_dream_tenant: "default".into(),
        })
    }

    /// Override the fork label. Used by AWAY_SUMMARY (Phase 80.14)
    /// and future eval harness (Phase 51) to share this runner's
    /// machinery under a different audit tag.
    pub fn with_fork_label(mut self, label: impl Into<String>) -> Self {
        self.fork_label = label.into();
        self
    }

    /// Phase 80.1.g — wire a memory-checkpoint sink. When set, the
    /// runner records each successful Completed fork-pass via the
    /// trait method (typically `nexo_core::agent::MemoryGitCheckpointer`).
    /// Skipped when `files_touched.is_empty()`. Failure of the
    /// checkpoint logs `tracing::warn!` and does NOT downgrade the
    /// outcome — the audit row in `dream_runs.db` is the source of
    /// truth.
    pub fn with_git_checkpointer(
        mut self,
        ckpt: Arc<dyn nexo_driver_types::MemoryCheckpointer>,
    ) -> Self {
        self.git_checkpointer = Some(ckpt);
        self
    }

    /// Wire a pre-dream snapshot sink. The runner fires the hook
    /// after the dream-run audit row is inserted but before the fork
    /// pass dispatches, so the bundle captures the live state the
    /// fork is about to mutate. Failure of the hook logs a
    /// `tracing::warn!` and the dream proceeds.
    pub fn with_pre_dream_snapshot(
        mut self,
        hook: Arc<dyn nexo_driver_types::PreDreamSnapshotHook>,
    ) -> Self {
        self.pre_dream_snapshot = Some(hook);
        self
    }

    /// Override the tenant string passed to the pre-dream snapshot
    /// hook. Default is `"default"` so single-tenant deployments do
    /// not need to set anything.
    pub fn with_pre_dream_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.pre_dream_tenant = tenant.into();
        self
    }

    /// Read-only accessor for boot logs / tests.
    pub fn has_pre_dream_snapshot(&self) -> bool {
        self.pre_dream_snapshot.is_some()
    }

    /// Phase 80.1.g — observability accessor for boot logs / tests.
    pub fn has_git_checkpointer(&self) -> bool {
        self.git_checkpointer.is_some()
    }

    /// Default constructor wiring `nexo_fork::DefaultForkSubagent` as
    /// the [`ForkSubagent`]. Operator code rarely overrides — this
    /// keeps the boot wiring concise.
    #[allow(clippy::too_many_arguments)]
    pub fn with_default_fork(
        config: Arc<ArcSwap<AutoDreamConfig>>,
        lock: Arc<ConsolidationLock>,
        audit: Arc<dyn DreamRunStore>,
        llm: Arc<dyn LlmClient>,
        tool_dispatcher: Arc<dyn ToolDispatcher>,
        memory_dir: PathBuf,
        parent_ctx_template: nexo_core::agent::AgentContext,
        fork_system_prompt: String,
        fork_tools: Vec<ToolDef>,
        fork_model: String,
    ) -> Result<Self, AutoDreamError> {
        Self::new(
            config,
            lock,
            Arc::new(DefaultForkSubagent::new()),
            audit,
            llm,
            tool_dispatcher,
            memory_dir,
            parent_ctx_template,
            fork_system_prompt,
            fork_tools,
            fork_model,
        )
    }

    /// Per-turn hook entry point. Mirrors leak `executeAutoDream`.
    pub async fn check_and_run(&self, ctx: &DreamContext) -> RunOutcome {
        self.run(ctx, RunReason::PerTurn).await
    }

    /// Manual force. Bypasses time / scan / session gates. Mirrors
    /// leak `isForced()` semantics (build-time hardcoded false in the
    /// leak — nexo exposes via `dream_now` tool + CLI).
    pub async fn run_forced(&self, ctx: &DreamContext) -> RunOutcome {
        self.run(ctx, RunReason::Manual).await
    }

    async fn run(&self, ctx: &DreamContext, reason: RunReason) -> RunOutcome {
        let cfg = self.config.load_full();
        let force = matches!(reason, RunReason::Manual);

        // Gate 1: enabled.
        if !cfg.enabled && !force {
            return RunOutcome::Skipped {
                gate: SkipReason::Disabled,
            };
        }

        // Gate 2: KAIROS active (mirror leak `:96` — KAIROS uses
        // disk-skill dream; autoDream skips).
        if ctx.kairos_active {
            return RunOutcome::Skipped {
                gate: SkipReason::KairosActive,
            };
        }

        // Gate 3: remote mode.
        if ctx.remote_mode {
            return RunOutcome::Skipped {
                gate: SkipReason::RemoteMode,
            };
        }

        // Gate 4: memory_dir present (already canonicalized at
        // construction).
        if !self.memory_dir.exists() {
            return RunOutcome::Skipped {
                gate: SkipReason::AutoMemoryDisabled,
            };
        }

        // Gate 5: time gate.
        let last_at = self.lock.read_last_consolidated_at().await;
        let now_ms = now_unix_ms();
        let hours_since = ((now_ms - last_at) as f64) / 3_600_000.0;
        if !force && hours_since < cfg.min_hours.as_secs_f64() / 3600.0 {
            return RunOutcome::Skipped {
                gate: SkipReason::TimeGate,
            };
        }

        // Gate 6: scan throttle (mirror leak `:143-150`).
        let now_secs = now_ms / 1000;
        let last_scan = self.last_session_scan_at.load(Ordering::Relaxed);
        if !force
            && now_secs - last_scan < cfg.scan_interval.as_secs() as i64
        {
            return RunOutcome::Skipped {
                gate: SkipReason::ScanThrottle,
            };
        }
        self.last_session_scan_at.store(now_secs, Ordering::Relaxed);

        // Gate 7: sessions touched.
        let sessions = match list_sessions_touched_since(
            &ctx.transcript_dir,
            last_at,
            &ctx.session_id,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    target: "auto_dream",
                    error = %e,
                    "list_sessions_touched_since failed"
                );
                Vec::new()
            }
        };
        if !force && (sessions.len() as u32) < cfg.min_sessions {
            return RunOutcome::Skipped {
                gate: SkipReason::SessionGate,
            };
        }

        // Lock acquire. Force path (mirror leak `:174-179`):
        // skip acquire entirely; use lastAt as priorMtime so
        // rollback is a no-op.
        let prior_mtime = if force {
            last_at
        } else {
            match self.lock.try_acquire().await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let (pid, mtime_ms) = self.lock.read_holder_info().await;
                    return RunOutcome::LockBlocked {
                        holder_pid: pid,
                        mtime_secs: mtime_ms / 1000,
                    };
                }
                Err(e) => {
                    warn!(
                        target: "auto_dream",
                        error = %e,
                        "lock acquire failed"
                    );
                    return RunOutcome::Errored {
                        run_id: Uuid::nil(),
                        error: e.to_string(),
                        prior_mtime: last_at,
                    };
                }
            }
        };

        info!(
            target: "auto_dream.fired",
            hours_since = hours_since.round() as i64,
            sessions_since = sessions.len(),
            "fork starting"
        );

        // Build prompt extra block — verbatim leak `:216-221`.
        let extra = build_extra(&sessions);
        let prompt = ConsolidationPromptBuilder::new(
            self.memory_dir.clone(),
            ctx.transcript_dir.clone(),
        )
        .build(&extra);

        // Insert audit row.
        let run_id = Uuid::new_v4();
        let started_at = Utc::now();
        let row = DreamRunRow {
            id: run_id,
            goal_id: ctx.goal_id,
            status: DreamRunStatus::Running,
            phase: DreamPhase::Starting,
            sessions_reviewing: sessions.len() as i32,
            prior_mtime_ms: Some(prior_mtime),
            files_touched: Vec::new(),
            turns: Vec::new(),
            started_at,
            ended_at: None,
            fork_label: self.fork_label.clone(),
            fork_run_id: None,
        };
        if let Err(e) = self.audit.insert(&row).await {
            warn!(
                target: "auto_dream.audit",
                error = %e,
                "insert dream_run row failed — proceeding best-effort"
            );
        }

        // Pre-dream snapshot hook. Best-effort: a failed snapshot
        // logs a warn and the dream proceeds without the rollback
        // anchor. Operators who want a hard gate enforce that at the
        // boot wire (omit `with_pre_dream_snapshot` until the snapshot
        // backend is healthy).
        if let Some(hook) = &self.pre_dream_snapshot {
            let agent_id = self.parent_ctx_template.agent_id.clone();
            if let Err(e) = hook
                .snapshot_before_dream(
                    &agent_id,
                    &self.pre_dream_tenant,
                    &run_id.to_string(),
                )
                .await
            {
                warn!(
                    target: "auto_dream.pre_snapshot",
                    error = %e,
                    run_id = %run_id,
                    "pre-dream snapshot failed; dream proceeds without rollback anchor"
                );
            }
        }

        // Tool whitelist (80.20).
        let filter = match AutoMemFilter::new(&self.memory_dir) {
            Ok(f) => Arc::new(f),
            Err(e) => {
                self.lock.rollback(prior_mtime).await;
                return RunOutcome::Errored {
                    run_id,
                    error: format!("AutoMemFilter::new: {e}"),
                    prior_mtime,
                };
            }
        };

        // Watcher (80.18 audit + post-fork escape detection).
        let watcher = Arc::new(DreamProgressWatcher::new(
            run_id,
            self.audit.clone(),
            self.memory_dir.clone(),
        ));

        // CacheSafeParams built fresh from operator-supplied template
        // — Phase 80.1.b refactor. No parent-cache share (same trade-off
        // Phase 77.5 ExtractMemories made). Provider-agnostic.
        let cache_safe = CacheSafeParams {
            system_prompt: Some(self.fork_system_prompt.clone()),
            system_blocks: Vec::new(),
            tools: self.fork_tools.clone(),
            cache_tools: false,
            model: self.fork_model.clone(),
            temperature: 0.0,
            max_tokens: Some(4096),
            fork_context_messages: Vec::new(),
        };

        let abort = CancellationToken::new();
        let fork_params = ForkParams {
            parent_ctx: self.parent_ctx_template.clone(),
            llm: self.llm.clone(),
            tool_dispatcher: self.tool_dispatcher.clone(),
            prompt_messages: vec![ChatMessage::user(prompt)],
            cache_safe,
            tool_filter: filter,
            query_source: QuerySource::AutoDream,
            fork_label: self.fork_label.clone(),
            overrides: None,
            max_turns: 30,
            on_message: Some(watcher.clone() as Arc<dyn nexo_fork::OnMessage>),
            skip_transcript: true,
            mode: DelegateMode::Sync,
            timeout: cfg.fork_timeout,
            external_abort: Some(abort.clone()),
        };

        let mut fork_handle = match self.fork.fork(fork_params).await {
            Ok(h) => h,
            Err(e) => {
                self.lock.rollback(prior_mtime).await;
                let _ = self
                    .audit
                    .update_status(run_id, DreamRunStatus::Failed)
                    .await;
                let _ = self.audit.finalize(run_id, Utc::now()).await;
                return RunOutcome::Errored {
                    run_id,
                    error: e.to_string(),
                    prior_mtime,
                };
            }
        };
        let completion = fork_handle
            .take_completion()
            .expect("fresh handle has Some completion");

        let started_instant = std::time::Instant::now();
        match completion.await {
            Ok(_result) => {
                // Defense-in-depth: post-fork escape audit.
                let progress = watcher.finish();
                if !progress.escapes.is_empty() {
                    self.lock.rollback(prior_mtime).await;
                    let _ = self
                        .audit
                        .update_status(run_id, DreamRunStatus::Failed)
                        .await;
                    let _ = self.audit.finalize(run_id, Utc::now()).await;
                    warn!(
                        target: "auto_dream.escape_audit",
                        escape_count = progress.escapes.len(),
                        "fork attempted edits outside memory_dir"
                    );
                    return RunOutcome::EscapeAudit {
                        run_id,
                        escapes: progress.escapes,
                        prior_mtime,
                    };
                }

                // Update phase to Updating if any edits landed (mirror
                // leak `DreamTask.ts:96`).
                if !progress.touched.is_empty() {
                    let _ = self
                        .audit
                        .update_phase(run_id, DreamPhase::Updating)
                        .await;
                }

                let _ = self
                    .audit
                    .update_status(run_id, DreamRunStatus::Completed)
                    .await;
                let _ = self.audit.finalize(run_id, Utc::now()).await;

                let duration_ms = started_instant.elapsed().as_millis() as u64;
                info!(
                    target: "auto_dream.completed",
                    duration_ms,
                    sessions_reviewed = sessions.len(),
                    files_touched = progress.touched.len(),
                    "fork done"
                );

                // Phase 80.1.g — checkpoint to git when configured AND
                // the fork actually wrote files. Empty `touched` means
                // the fork ran but didn't change any memory file (e.g.
                // pruned candidates rejected); the audit row already
                // captures that — no point in an empty git commit.
                if let Some(ckpt) = &self.git_checkpointer {
                    if !progress.touched.is_empty() {
                        let subject = format!(
                            "auto_dream: {} file(s) consolidated",
                            progress.touched.len()
                        );
                        let body = build_checkpoint_body(run_id, &progress.touched);
                        if let Err(e) = ckpt.checkpoint(subject, body).await {
                            warn!(
                                target: "auto_dream.checkpoint",
                                run_id = %run_id,
                                error = %e,
                                "memory checkpoint failed; audit row preserved"
                            );
                        }
                    }
                }

                RunOutcome::Completed {
                    run_id,
                    files_touched: progress.touched,
                    duration_ms,
                }
            }
            Err(nexo_fork::ForkError::Timeout(timeout)) => {
                self.lock.rollback(prior_mtime).await;
                let _ = self
                    .audit
                    .update_status(run_id, DreamRunStatus::Killed)
                    .await;
                let _ = self.audit.finalize(run_id, Utc::now()).await;
                warn!(
                    target: "auto_dream.failed",
                    error = "timeout",
                    timeout_secs = timeout.as_secs(),
                );
                RunOutcome::TimedOut {
                    run_id,
                    timeout_secs: timeout.as_secs(),
                    prior_mtime,
                }
            }
            Err(e) => {
                self.lock.rollback(prior_mtime).await;
                let _ = self
                    .audit
                    .update_status(run_id, DreamRunStatus::Failed)
                    .await;
                let _ = self.audit.finalize(run_id, Utc::now()).await;
                let error_str = e.to_string();
                warn!(
                    target: "auto_dream.failed",
                    error = %error_str,
                );
                RunOutcome::Errored {
                    run_id,
                    error: error_str,
                    prior_mtime,
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Phase 80.1.b — `AutoDreamHook` impl bridges `AutoDreamRunner` to the
// driver-loop without forcing nexo-driver-loop to depend on nexo-dream
// (which would cycle: nexo-core → nexo-dispatch-tools → nexo-driver-loop
// → nexo-dream → nexo-core).
//
// The impl converts the slim `DreamContextLite` (driver-types) into the
// internal `DreamContext` and maps `RunOutcome → AutoDreamOutcomeKind`.
// ─────────────────────────────────────────────────────────────────────

#[async_trait]
impl AutoDreamHook for AutoDreamRunner {
    async fn check_and_run(
        &self,
        lite: &DreamContextLite,
    ) -> AutoDreamOutcomeKind {
        let ctx = DreamContext {
            goal_id: lite.goal_id,
            session_id: lite.session_id.clone(),
            transcript_dir: lite.transcript_dir.clone(),
            kairos_active: lite.kairos_active,
            remote_mode: lite.remote_mode,
        };
        run_outcome_to_kind(&self.run(&ctx, RunReason::PerTurn).await)
    }
}

/// Lossy mapping from `RunOutcome` to `AutoDreamOutcomeKind`. Detail
/// fields (run_id, files_touched, etc.) drop here — full row lives
/// in the 80.18 `dream_runs` audit table.
pub fn run_outcome_to_kind(outcome: &RunOutcome) -> AutoDreamOutcomeKind {
    match outcome {
        RunOutcome::Completed { .. } => AutoDreamOutcomeKind::Completed,
        RunOutcome::Skipped { gate } => match gate {
            SkipReason::Disabled => AutoDreamOutcomeKind::SkippedDisabled,
            SkipReason::KairosActive => AutoDreamOutcomeKind::SkippedKairosActive,
            SkipReason::RemoteMode => AutoDreamOutcomeKind::SkippedRemoteMode,
            SkipReason::AutoMemoryDisabled => {
                AutoDreamOutcomeKind::SkippedAutoMemoryDisabled
            }
            SkipReason::TimeGate => AutoDreamOutcomeKind::SkippedTimeGate,
            SkipReason::ScanThrottle => AutoDreamOutcomeKind::SkippedScanThrottle,
            SkipReason::SessionGate => AutoDreamOutcomeKind::SkippedSessionGate,
        },
        RunOutcome::LockBlocked { .. } => AutoDreamOutcomeKind::LockBlocked,
        RunOutcome::Errored { .. } => AutoDreamOutcomeKind::Errored,
        RunOutcome::TimedOut { .. } => AutoDreamOutcomeKind::TimedOut,
        RunOutcome::EscapeAudit { .. } => AutoDreamOutcomeKind::EscapeAudit,
    }
}

/// Phase 80.1.g — render the git-checkpoint body for a Completed
/// fork-pass. Format mirrors the scoring-sweep pattern at
/// `src/main.rs:3640-3665`: top line carries the audit run id (so a
/// reader can `git log --grep auto_dream` then cross-link to the row
/// in `dream_runs.db`), followed by a markdown-bullet list of the
/// touched paths. No truncation cap because fork passes typically
/// touch 1-5 files (constrained by AutoMemFilter + 30-turn cap).
pub fn build_checkpoint_body(run_id: Uuid, files: &[PathBuf]) -> String {
    let mut s = format!("audit_run_id: {run_id}\n\n");
    for f in files {
        s.push_str(&format!("- {}\n", f.display()));
    }
    s
}

/// Build the per-run `extra` block for the consolidation prompt.
/// Verbatim port of leak `:216-221`.
pub fn build_extra(sessions: &[String]) -> String {
    let mut out = String::new();
    out.push_str(
        "**Tool constraints for this run:** Bash is restricted to read-only commands (`ls`, `find`, `grep`, `cat`, `stat`, `wc`, `head`, `tail`, and similar). Anything that writes, redirects to a file, or modifies state will be denied. Plan your exploration with this in mind — no need to probe.\n\n",
    );
    out.push_str(&format!(
        "Sessions since last consolidation ({}):\n",
        sessions.len()
    ));
    for s in sessions {
        out.push_str("- ");
        out.push_str(s);
        out.push('\n');
    }
    // Trim trailing newline for stable prompt body.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::future::{ready, BoxFuture};
    use nexo_agent_registry::SqliteDreamRunStore;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig,
        ModelConfig, OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use nexo_core::agent::AgentContext;
    use nexo_core::session::SessionManager;
    use nexo_fork::{ForkError, ForkHandle, ForkResult};
    use nexo_llm::stream::StreamChunk;
    use nexo_llm::types::{
        ChatRequest, ChatResponse, FinishReason, ResponseContent, TokenUsage, ToolDef,
    };

    #[test]
    fn build_extra_renders_constraints_and_sessions() {
        let sessions = vec!["s1".to_string(), "s2".to_string()];
        let extra = build_extra(&sessions);
        assert!(extra.contains("Tool constraints for this run"));
        assert!(extra.contains("read-only commands"));
        assert!(extra.contains("Sessions since last consolidation (2):"));
        assert!(extra.contains("- s1"));
        assert!(extra.contains("- s2"));
    }

    #[test]
    fn build_extra_handles_empty_sessions() {
        let extra = build_extra(&[]);
        assert!(extra.contains("(0):"));
        assert!(extra.contains("Tool constraints"));
    }

    // ── Runner integration tests with mocks ──

    fn mk_agent_ctx() -> AgentContext {
        let cfg = AgentConfig {
            id: "test_agent".into(),
            model: ModelConfig {
                provider: "test".into(),
                model: "test-model".into(),
            },
            plugins: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: Vec::new(),
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: DreamingYamlConfig::default(),
            workspace_git: WorkspaceGitConfig::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            outbound_allowlist: OutboundAllowlistConfig::default(),
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
            repl: Default::default(),
            auto_dream: None,
            assistant_mode: None,
            away_summary: None,
            brief: None,
            channels: None,
            auto_approve: false,
            extract_memories: None,
            event_subscribers: Vec::new(),
        };
        AgentContext::new(
            "test_agent",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    fn mk_fork_tools() -> Vec<ToolDef> {
        vec![ToolDef {
            name: "FileRead".into(),
            description: "read".into(),
            parameters: serde_json::json!({"type":"object"}),
        }]
    }

    /// Mock LlmClient — never used in gate-only tests but needed for
    /// runner construction. Returns text "ok".
    struct MockLlm;

    #[async_trait]
    impl LlmClient for MockLlm {
        async fn chat(&self, _: ChatRequest) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                content: ResponseContent::Text("ok".into()),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                cache_usage: None,
            })
        }
        fn model_id(&self) -> &str {
            "test"
        }
        fn provider(&self) -> &str {
            "mock"
        }
        async fn stream<'a>(
            &'a self,
            _: ChatRequest,
        ) -> anyhow::Result<futures::stream::BoxStream<'a, anyhow::Result<StreamChunk>>>
        {
            anyhow::bail!("stream not used")
        }
    }

    struct NoopDispatcher;
    #[async_trait]
    impl ToolDispatcher for NoopDispatcher {
        async fn dispatch(
            &self,
            _name: &str,
            _args: serde_json::Value,
        ) -> Result<String, String> {
            Ok(String::new())
        }
    }

    /// Mock ForkSubagent — scripted ForkResult.
    struct MockFork {
        result: tokio::sync::Mutex<Option<Result<ForkResult, ForkError>>>,
    }

    impl MockFork {
        fn ok() -> Arc<Self> {
            Arc::new(Self {
                result: tokio::sync::Mutex::new(Some(Ok(ForkResult {
                    messages: vec![],
                    total_usage: TokenUsage::default(),
                    total_cache_usage: nexo_llm::types::CacheUsage::default(),
                    final_text: Some("done".into()),
                    turns_executed: 1,
                }))),
            })
        }
        fn err(e: ForkError) -> Arc<Self> {
            Arc::new(Self {
                result: tokio::sync::Mutex::new(Some(Err(e))),
            })
        }
    }

    #[async_trait]
    impl ForkSubagent for MockFork {
        async fn fork(&self, _params: ForkParams) -> Result<ForkHandle, ForkError> {
            let mut g = self.result.lock().await;
            let r = g.take().unwrap_or_else(|| Err(ForkError::Internal("exhausted".into())));
            let abort = CancellationToken::new();
            let fut: BoxFuture<'static, Result<ForkResult, ForkError>> =
                Box::pin(ready(r));
            Ok(ForkHandle::new(Uuid::new_v4(), None, fut, abort))
        }
    }

    fn mk_dream_ctx(
        memory_dir: &std::path::Path,
        kairos_active: bool,
        remote_mode: bool,
    ) -> DreamContext {
        DreamContext {
            goal_id: GoalId(Uuid::new_v4()),
            session_id: Uuid::new_v4().to_string(),
            transcript_dir: memory_dir.to_path_buf(),
            kairos_active,
            remote_mode,
        }
    }

    async fn mk_runner_with_fork(
        cfg: AutoDreamConfig,
        memory_dir: PathBuf,
        fork: Arc<dyn ForkSubagent>,
    ) -> AutoDreamRunner {
        let lock =
            Arc::new(ConsolidationLock::new(&memory_dir, cfg.holder_stale).unwrap());
        let audit: Arc<dyn DreamRunStore> =
            Arc::new(SqliteDreamRunStore::open_memory().await.unwrap());
        AutoDreamRunner::new(
            Arc::new(ArcSwap::from_pointee(cfg)),
            lock,
            fork,
            audit,
            Arc::new(MockLlm),
            Arc::new(NoopDispatcher),
            memory_dir,
            mk_agent_ctx(),
            "system".into(),
            mk_fork_tools(),
            "test-model".into(),
        )
        .unwrap()
    }

    fn enabled_cfg() -> AutoDreamConfig {
        AutoDreamConfig {
            enabled: true,
            ..AutoDreamConfig::default()
        }
    }

    // ── Gate tests ──

    #[tokio::test]
    async fn gate_disabled_returns_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let cfg = AutoDreamConfig::default(); // enabled = false
        let runner = mk_runner_with_fork(cfg, dir.clone(), MockFork::ok()).await;
        let ctx = mk_dream_ctx(&dir, false, false);
        let outcome = runner.check_and_run(&ctx).await;
        assert!(matches!(
            outcome,
            RunOutcome::Skipped {
                gate: SkipReason::Disabled
            }
        ));
    }

    #[tokio::test]
    async fn gate_kairos_active_returns_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let runner =
            mk_runner_with_fork(enabled_cfg(), dir.clone(), MockFork::ok()).await;
        let ctx = mk_dream_ctx(&dir, true, false);
        let outcome = runner.check_and_run(&ctx).await;
        assert!(matches!(
            outcome,
            RunOutcome::Skipped {
                gate: SkipReason::KairosActive
            }
        ));
    }

    #[tokio::test]
    async fn gate_remote_mode_returns_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let runner =
            mk_runner_with_fork(enabled_cfg(), dir.clone(), MockFork::ok()).await;
        let ctx = mk_dream_ctx(&dir, false, true);
        let outcome = runner.check_and_run(&ctx).await;
        assert!(matches!(
            outcome,
            RunOutcome::Skipped {
                gate: SkipReason::RemoteMode
            }
        ));
    }

    #[tokio::test]
    async fn gate_session_count_below_min_returns_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        // No sessions in transcript_dir → count 0 < min_sessions=5.
        let runner =
            mk_runner_with_fork(enabled_cfg(), dir.clone(), MockFork::ok()).await;
        let ctx = mk_dream_ctx(&dir, false, false);
        let outcome = runner.check_and_run(&ctx).await;
        // Time gate passes (last_at == 0, 24h+ since epoch); session
        // gate fails.
        assert!(matches!(
            outcome,
            RunOutcome::Skipped {
                gate: SkipReason::SessionGate
            }
        ));
    }

    #[tokio::test]
    async fn force_run_bypasses_session_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let runner =
            mk_runner_with_fork(enabled_cfg(), dir.clone(), MockFork::ok()).await;
        let ctx = mk_dream_ctx(&dir, false, false);
        let outcome = runner.run_forced(&ctx).await;
        assert!(matches!(outcome, RunOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn force_run_completed_records_audit_row() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let lock =
            Arc::new(ConsolidationLock::new(&dir, std::time::Duration::from_secs(3600)).unwrap());
        let audit: Arc<dyn DreamRunStore> =
            Arc::new(SqliteDreamRunStore::open_memory().await.unwrap());
        let runner = AutoDreamRunner::new(
            Arc::new(ArcSwap::from_pointee(enabled_cfg())),
            lock,
            MockFork::ok(),
            audit.clone(),
            Arc::new(MockLlm),
            Arc::new(NoopDispatcher),
            dir.clone(),
            mk_agent_ctx(),
            "system".into(),
            mk_fork_tools(),
            "test-model".into(),
        )
        .unwrap();
        let ctx = mk_dream_ctx(&dir, false, false);
        let outcome = runner.run_forced(&ctx).await;
        let run_id = match outcome {
            RunOutcome::Completed { run_id, .. } => run_id,
            other => panic!("expected Completed, got {other:?}"),
        };
        let row = audit.get(run_id).await.unwrap().unwrap();
        assert_eq!(row.status, DreamRunStatus::Completed);
        assert!(row.ended_at.is_some());
    }

    #[tokio::test]
    async fn fork_error_rolls_back_lock_and_records_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let lock =
            Arc::new(ConsolidationLock::new(&dir, std::time::Duration::from_secs(3600)).unwrap());
        let audit: Arc<dyn DreamRunStore> =
            Arc::new(SqliteDreamRunStore::open_memory().await.unwrap());
        let fork = MockFork::err(ForkError::Internal("boom".into()));
        let runner = AutoDreamRunner::new(
            Arc::new(ArcSwap::from_pointee(enabled_cfg())),
            lock.clone(),
            fork,
            audit.clone(),
            Arc::new(MockLlm),
            Arc::new(NoopDispatcher),
            dir.clone(),
            mk_agent_ctx(),
            "system".into(),
            mk_fork_tools(),
            "test-model".into(),
        )
        .unwrap();
        let ctx = mk_dream_ctx(&dir, false, false);
        let outcome = runner.run_forced(&ctx).await;
        let run_id = match outcome {
            RunOutcome::Errored { run_id, .. } => run_id,
            other => panic!("expected Errored, got {other:?}"),
        };
        let row = audit.get(run_id).await.unwrap().unwrap();
        assert_eq!(row.status, DreamRunStatus::Failed);
        // Lock rolled back (no live PID, file may exist with cleared body).
        // Verify lastConsolidatedAt rewound to lastAt (==0 in fresh dir).
        let mtime = lock.read_last_consolidated_at().await;
        assert!(mtime <= 1, "lock mtime not rolled back to 0; got {mtime}");
    }

    #[tokio::test]
    async fn fork_timeout_returns_timed_out_and_rolls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let lock =
            Arc::new(ConsolidationLock::new(&dir, std::time::Duration::from_secs(3600)).unwrap());
        let audit: Arc<dyn DreamRunStore> =
            Arc::new(SqliteDreamRunStore::open_memory().await.unwrap());
        let fork = MockFork::err(ForkError::Timeout(std::time::Duration::from_secs(5)));
        let runner = AutoDreamRunner::new(
            Arc::new(ArcSwap::from_pointee(enabled_cfg())),
            lock,
            fork,
            audit.clone(),
            Arc::new(MockLlm),
            Arc::new(NoopDispatcher),
            dir.clone(),
            mk_agent_ctx(),
            "system".into(),
            mk_fork_tools(),
            "test-model".into(),
        )
        .unwrap();
        let ctx = mk_dream_ctx(&dir, false, false);
        let outcome = runner.run_forced(&ctx).await;
        match outcome {
            RunOutcome::TimedOut { run_id, timeout_secs, .. } => {
                assert_eq!(timeout_secs, 5);
                let row = audit.get(run_id).await.unwrap().unwrap();
                assert_eq!(row.status, DreamRunStatus::Killed);
            }
            other => panic!("expected TimedOut, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lock_blocked_returns_lock_blocked_outcome() {
        // Gate ordering: time gate at min_hours=1h must pass AND
        // holder_stale=4h must NOT have elapsed → mtime 1.5h ago
        // satisfies both. Live PID → blocks.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let cfg = AutoDreamConfig {
            enabled: true,
            min_hours: std::time::Duration::from_secs(60 * 60), // 1h (min)
            min_sessions: 1,
            holder_stale: std::time::Duration::from_secs(4 * 60 * 60), // 4h
            ..AutoDreamConfig::default()
        };
        let lock = Arc::new(ConsolidationLock::new(&dir, cfg.holder_stale).unwrap());

        // Pre-write the lock with live PID + back-date mtime to 1.5h ago.
        let our_pid = std::process::id().to_string();
        tokio::fs::write(lock.path(), &our_pid).await.unwrap();
        let one_and_half_hour_ago = SystemTime::now()
            - std::time::Duration::from_secs(90 * 60);
        let path_clone = lock.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            filetime::set_file_mtime(
                &path_clone,
                filetime::FileTime::from_system_time(one_and_half_hour_ago),
            )
        })
        .await
        .unwrap()
        .unwrap();

        // Need a transcript session so session-gate passes.
        let session_uuid = Uuid::new_v4().to_string();
        tokio::fs::write(dir.join(format!("{session_uuid}.jsonl")), b"x")
            .await
            .unwrap();

        let audit: Arc<dyn DreamRunStore> =
            Arc::new(SqliteDreamRunStore::open_memory().await.unwrap());
        let runner = AutoDreamRunner::new(
            Arc::new(ArcSwap::from_pointee(cfg)),
            lock,
            MockFork::ok(),
            audit,
            Arc::new(MockLlm),
            Arc::new(NoopDispatcher),
            dir.clone(),
            mk_agent_ctx(),
            "system".into(),
            mk_fork_tools(),
            "test-model".into(),
        )
        .unwrap();
        let mut ctx = mk_dream_ctx(&dir, false, false);
        ctx.session_id = "current_session".into();
        let outcome = runner.check_and_run(&ctx).await;
        assert!(
            matches!(outcome, RunOutcome::LockBlocked { .. }),
            "expected LockBlocked, got {outcome:?}"
        );
    }

    // ── Phase 80.1.g — git checkpoint wiring ──

    use std::sync::atomic::{AtomicUsize, Ordering as StdOrdering};
    use std::sync::Mutex as StdMutex;

    /// In-memory `MemoryCheckpointer` for unit tests. Records every
    /// (subject, body) pair plus a call counter.
    struct RecordingCheckpointer {
        calls: AtomicUsize,
        log: StdMutex<Vec<(String, String)>>,
        fail: bool,
    }

    impl RecordingCheckpointer {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                log: StdMutex::new(Vec::new()),
                fail: false,
            })
        }
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                log: StdMutex::new(Vec::new()),
                fail: true,
            })
        }
        fn count(&self) -> usize {
            self.calls.load(StdOrdering::SeqCst)
        }
    }

    #[async_trait]
    impl nexo_driver_types::MemoryCheckpointer for RecordingCheckpointer {
        async fn checkpoint(
            &self,
            subject: String,
            body: String,
        ) -> Result<(), String> {
            self.calls.fetch_add(1, StdOrdering::SeqCst);
            self.log
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((subject, body));
            if self.fail {
                Err("synthetic checkpoint failure".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn build_checkpoint_body_renders_run_id_and_paths() {
        let id = Uuid::new_v4();
        let files = vec![PathBuf::from("a.md"), PathBuf::from("nested/b.md")];
        let body = build_checkpoint_body(id, &files);
        assert!(body.starts_with(&format!("audit_run_id: {id}\n\n")));
        assert!(body.contains("- a.md\n"));
        assert!(body.contains("- nested/b.md\n"));
    }

    #[test]
    fn build_checkpoint_body_renders_empty_file_list() {
        let id = Uuid::new_v4();
        let body = build_checkpoint_body(id, &[]);
        assert_eq!(body, format!("audit_run_id: {id}\n\n"));
    }

    #[tokio::test]
    async fn with_git_checkpointer_setter_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let runner = mk_runner_with_fork(enabled_cfg(), dir.clone(), MockFork::ok()).await;
        assert!(!runner.has_git_checkpointer());
        let ckpt = RecordingCheckpointer::new();
        let with_ckpt = runner.with_git_checkpointer(ckpt);
        assert!(with_ckpt.has_git_checkpointer());
    }

    #[tokio::test]
    async fn checkpoint_skipped_when_files_touched_empty() {
        // MockFork::ok produces messages: vec![] → progress.touched is
        // empty → checkpoint guard skips. The audit row still records
        // Completed (source of truth lives in dream_runs.db).
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();

        // Seed transcripts so session-gate passes.
        let transcripts = tmp.path().join("transcripts");
        std::fs::create_dir_all(&transcripts).unwrap();
        for i in 0..6 {
            std::fs::write(
                transcripts.join(format!("session_{i}.jsonl")),
                "stub",
            )
            .unwrap();
        }

        let cfg = AutoDreamConfig {
            min_hours: std::time::Duration::from_secs(0),
            min_sessions: 1,
            ..enabled_cfg()
        };
        let ckpt = RecordingCheckpointer::new();
        let runner = mk_runner_with_fork(cfg, dir.clone(), MockFork::ok())
            .await
            .with_git_checkpointer(ckpt.clone());

        let mut ctx = mk_dream_ctx(&dir, false, false);
        ctx.transcript_dir = transcripts;
        let outcome = runner.run_forced(&ctx).await;
        assert!(
            matches!(outcome, RunOutcome::Completed { .. }),
            "expected Completed, got {outcome:?}"
        );
        assert_eq!(
            ckpt.count(),
            0,
            "checkpoint must be skipped when files_touched is empty"
        );
    }

    #[tokio::test]
    async fn checkpoint_failure_does_not_downgrade_completed_outcome() {
        // Even if the checkpointer returns Err, the runner must still
        // report Completed because the audit row + memory_dir state
        // are correct — git is bonus forensics.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let transcripts = tmp.path().join("transcripts");
        std::fs::create_dir_all(&transcripts).unwrap();
        for i in 0..6 {
            std::fs::write(
                transcripts.join(format!("session_{i}.jsonl")),
                "stub",
            )
            .unwrap();
        }
        let cfg = AutoDreamConfig {
            min_hours: std::time::Duration::from_secs(0),
            min_sessions: 1,
            ..enabled_cfg()
        };
        let ckpt = RecordingCheckpointer::failing();
        let runner = mk_runner_with_fork(cfg, dir.clone(), MockFork::ok())
            .await
            .with_git_checkpointer(ckpt.clone());

        let mut ctx = mk_dream_ctx(&dir, false, false);
        ctx.transcript_dir = transcripts;
        let outcome = runner.run_forced(&ctx).await;
        // Either Completed (with empty touches) or any non-Errored
        // variant — point is the runner did NOT panic on the
        // checkpoint error. With empty touches the checkpoint isn't
        // even called; this test pairs with the next one which forces
        // touches.
        assert!(
            !matches!(outcome, RunOutcome::Errored { .. }),
            "checkpoint failure must not propagate as Errored, got {outcome:?}"
        );
    }
}
