//! Goal-level loop. Takes a `Goal`, drives it to completion through
//! the per-turn attempt loop. Emits NATS events at every major
//! transition.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nexo_config::types::llm::AutoCompactionConfig;
use nexo_driver_claude::SessionBindingStore;
use nexo_driver_permission::PermissionDecider;
use nexo_driver_types::{
    AcceptanceVerdict, AttemptOutcome, AttemptParams, AutoCompactBreaker, AutoDreamHook,
    AutoDreamOutcomeKind, BudgetGuards, BudgetUsage, CancellationToken, CompactContext,
    CompactPolicy, CompactSummary, CompactSummaryStore, DefaultCompactPolicy, DreamContextLite,
    Goal, GoalId,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken as TokioCancel;

use crate::acceptance::{AcceptanceEvaluator, NoopAcceptanceEvaluator};
use crate::attempt::{run_attempt, AttemptContext};
use crate::compact_store::NoopCompactSummaryStore;
use crate::extract_memories::ExtractMemories;
use crate::post_compact_cleanup::PostCompactCleanup;
use crate::error::DriverError;
use crate::events::{DriverEvent, DriverEventSink, NoopEventSink};
use crate::mcp_config::write_mcp_config;
use crate::proactive::{build_tick_prompt, wait_for_wake, ScheduledWake, WakeResult};
use crate::replay::{
    DefaultReplayPolicy, ReplayContext, ReplayDecision, ReplayOutcomeHint, ReplayPolicy,
};
use crate::socket::DriverSocketServer;
use crate::workspace::WorkspaceManager;
use dashmap::DashMap;
use tokio::sync::watch;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalOutcome {
    pub goal_id: GoalId,
    pub outcome: AttemptOutcome,
    pub total_turns: u32,
    pub usage: BudgetUsage,
    pub final_text: Option<String>,
    pub acceptance: Option<AcceptanceVerdict>,
    #[serde(with = "humantime_serde")]
    pub elapsed: Duration,
}

pub struct DriverOrchestrator {
    claude_cfg: nexo_driver_claude::ClaudeConfig,
    binding_store: Arc<dyn SessionBindingStore>,
    acceptance: Arc<dyn AcceptanceEvaluator>,
    workspace_manager: Arc<WorkspaceManager>,
    event_sink: Arc<dyn DriverEventSink>,
    /// Phase 67.8 — replay-policy classifies mid-turn errors.
    replay_policy: Arc<dyn ReplayPolicy>,
    /// Phase 67.9 + 77.2 — opportunistic /compact policy.
    compact_policy: Arc<dyn CompactPolicy>,
    compact_context_window: u64,
    /// Phase 77.2 — token + age auto-compaction config.
    auto_config: Option<AutoCompactionConfig>,
    /// Phase 77.2 — circuit breaker for compaction failures.
    compact_breaker: Mutex<AutoCompactBreaker>,
    /// Phase 77.3 — persist compact summaries for session resume.
    compact_store: Arc<dyn CompactSummaryStore>,
    /// Phase 67.C.1 — emit `DriverEvent::Progress` after every Nth
    /// completed attempt so chat hooks can show 'still going'. `0`
    /// disables periodic progress beacons.
    progress_every_turns: u32,
    /// Phase 67.C.2 — per-goal pause flag. `pause_goal(id)` flips
    /// the watch to `true`; the loop blocks on the next iteration
    /// until `resume_goal(id)` flips it back. The current Claude
    /// turn is *not* killed — pause only takes effect at the
    /// natural boundary between turns.
    pause_signals: Arc<DashMap<GoalId, watch::Sender<bool>>>,
    /// Phase 67.G.2 — per-goal CancellationToken so `cancel_agent`
    /// can stop one goal without taking down the whole orchestrator
    /// via `cancel_root`. The token is a child of `cancel_root`
    /// so a global shutdown still cancels every running goal.
    cancel_tokens: Arc<DashMap<GoalId, CancellationToken>>,
    /// B2 — operator overrides for in-flight goal budgets. The
    /// loop consults this map at the top of every iteration so
    /// `update_budget` actually changes the cap (not just the
    /// snapshot). Currently only `max_turns` is grow-only mutable.
    budget_overrides: Arc<DashMap<GoalId, BudgetGuards>>,
    /// Operator-side messages queued for injection into the next
    /// turn's prompt. The agent-side `interrupt_agent` tool fills
    /// this; the loop drains it at the top of every iteration so
    /// Claude sees the operator's note in its next turn input.
    /// FIFO so multiple rapid interrupts arrive in order.
    pending_interrupts: Arc<DashMap<GoalId, std::collections::VecDeque<String>>>,
    /// Phase 77.5 — post-turn LLM memory extraction.
    extract_memories: Option<Arc<ExtractMemories>>,
    /// Phase 77.5 — root directory for persistent memory files.
    memory_dir: Option<PathBuf>,
    /// Phase 80.1.b — post-turn autoDream consolidation hook.
    /// `None` disables; runner is owned by `nexo-dream` impl.
    /// Phase 80.1.b.b.b.b — wrapped in a `Mutex<Option<...>>` so
    /// the boot wire can attach the runner AFTER the orchestrator
    /// is constructed (the per-agent loop runs after the
    /// orchestrator builder fires, by `boot_dispatch_ctx_if_enabled`
    /// design). `arc_swap::ArcSwapOption` requires `T: Sized`, so
    /// a stdlib mutex is the lowest-friction wrap for a `dyn`
    /// trait object. Per-turn read cost = one uncontended lock
    /// acquire.
    auto_dream: std::sync::Mutex<Option<Arc<dyn AutoDreamHook>>>,
    bin_path: PathBuf,
    socket_path: PathBuf,
    /// Owns the spawned socket server; cancelling kills it.
    _socket_handle: tokio::task::JoinHandle<Result<(), DriverError>>,
    socket_cancel: TokioCancel,
    cancel_root: CancellationToken,
}

#[derive(Default)]
pub struct DriverOrchestratorBuilder {
    claude_cfg: Option<nexo_driver_claude::ClaudeConfig>,
    binding_store: Option<Arc<dyn SessionBindingStore>>,
    acceptance: Option<Arc<dyn AcceptanceEvaluator>>,
    decider: Option<Arc<dyn PermissionDecider>>,
    workspace_manager: Option<Arc<WorkspaceManager>>,
    event_sink: Option<Arc<dyn DriverEventSink>>,
    replay_policy: Option<Arc<dyn ReplayPolicy>>,
    compact_policy: Option<Arc<dyn CompactPolicy>>,
    compact_context_window: u64,
    auto_config: Option<AutoCompactionConfig>,
    compact_store: Option<Arc<dyn CompactSummaryStore>>,
    extract_memories: Option<Arc<ExtractMemories>>,
    memory_dir: Option<PathBuf>,
    /// Phase 80.1.b — post-turn autoDream hook.
    auto_dream: Option<Arc<dyn AutoDreamHook>>,
    progress_every_turns: u32,
    bin_path: Option<PathBuf>,
    socket_path: Option<PathBuf>,
    cancel_root: Option<CancellationToken>,
}

impl DriverOrchestratorBuilder {
    pub fn claude_config(mut self, cfg: nexo_driver_claude::ClaudeConfig) -> Self {
        self.claude_cfg = Some(cfg);
        self
    }
    pub fn binding_store(mut self, s: Arc<dyn SessionBindingStore>) -> Self {
        self.binding_store = Some(s);
        self
    }
    pub fn acceptance(mut self, a: Arc<dyn AcceptanceEvaluator>) -> Self {
        self.acceptance = Some(a);
        self
    }
    pub fn decider(mut self, d: Arc<dyn PermissionDecider>) -> Self {
        self.decider = Some(d);
        self
    }
    pub fn workspace_manager(mut self, w: Arc<WorkspaceManager>) -> Self {
        self.workspace_manager = Some(w);
        self
    }
    pub fn event_sink(mut self, e: Arc<dyn DriverEventSink>) -> Self {
        self.event_sink = Some(e);
        self
    }
    pub fn bin_path(mut self, p: impl Into<PathBuf>) -> Self {
        self.bin_path = Some(p.into());
        self
    }
    pub fn socket_path(mut self, p: impl Into<PathBuf>) -> Self {
        self.socket_path = Some(p.into());
        self
    }
    pub fn cancel_root(mut self, c: CancellationToken) -> Self {
        self.cancel_root = Some(c);
        self
    }
    pub fn replay_policy(mut self, p: Arc<dyn ReplayPolicy>) -> Self {
        self.replay_policy = Some(p);
        self
    }
    pub fn compact_policy(mut self, p: Arc<dyn CompactPolicy>) -> Self {
        self.compact_policy = Some(p);
        self
    }
    pub fn compact_context_window(mut self, n: u64) -> Self {
        self.compact_context_window = n;
        self
    }
    pub fn auto_config(mut self, a: AutoCompactionConfig) -> Self {
        self.auto_config = Some(a);
        self
    }
    pub fn compact_store(mut self, s: Arc<dyn CompactSummaryStore>) -> Self {
        self.compact_store = Some(s);
        self
    }
    pub fn extract_memories(mut self, e: Arc<ExtractMemories>) -> Self {
        self.extract_memories = Some(e);
        self
    }
    pub fn memory_dir(mut self, p: impl Into<PathBuf>) -> Self {
        self.memory_dir = Some(p.into());
        self
    }
    /// Phase 80.1.b — wire the autoDream post-turn hook. `None`
    /// disables. Mirror Phase 77.5 `extract_memories` builder.
    pub fn auto_dream(mut self, hook: Arc<dyn AutoDreamHook>) -> Self {
        self.auto_dream = Some(hook);
        self
    }
    pub fn progress_every_turns(mut self, n: u32) -> Self {
        self.progress_every_turns = n;
        self
    }

    pub async fn build(self) -> Result<DriverOrchestrator, DriverError> {
        let claude_cfg = self
            .claude_cfg
            .ok_or_else(|| DriverError::Config("claude config required".into()))?;
        let binding_store = self
            .binding_store
            .ok_or_else(|| DriverError::Config("binding_store required".into()))?;
        let decider = self
            .decider
            .ok_or_else(|| DriverError::Config("decider required".into()))?;
        let workspace_manager = self
            .workspace_manager
            .ok_or_else(|| DriverError::Config("workspace_manager required".into()))?;
        let bin_path = self
            .bin_path
            .ok_or_else(|| DriverError::Config("bin_path required".into()))?;
        let socket_path = self
            .socket_path
            .ok_or_else(|| DriverError::Config("socket_path required".into()))?;
        let acceptance: Arc<dyn AcceptanceEvaluator> = self
            .acceptance
            .unwrap_or_else(|| Arc::new(NoopAcceptanceEvaluator));
        let event_sink: Arc<dyn DriverEventSink> =
            self.event_sink.unwrap_or_else(|| Arc::new(NoopEventSink));
        let replay_policy: Arc<dyn ReplayPolicy> = self
            .replay_policy
            .unwrap_or_else(|| Arc::new(DefaultReplayPolicy::default()));
        let compact_policy: Arc<dyn CompactPolicy> = self
            .compact_policy
            .unwrap_or_else(|| Arc::new(DefaultCompactPolicy::default()));
        let compact_context_window = self.compact_context_window;
        let auto_config = self.auto_config;
        let compact_store: Arc<dyn CompactSummaryStore> = self
            .compact_store
            .unwrap_or_else(|| Arc::new(NoopCompactSummaryStore));
        let progress_every_turns = self.progress_every_turns;
        let cancel_root = self.cancel_root.unwrap_or_default();

        // Bind the socket server.
        let socket_cancel = TokioCancel::new();
        let server = DriverSocketServer::bind(&socket_path, decider, socket_cancel.clone()).await?;
        let socket_handle = tokio::spawn(server.run());

        Ok(DriverOrchestrator {
            claude_cfg,
            binding_store,
            acceptance,
            workspace_manager,
            event_sink,
            replay_policy,
            compact_policy,
            compact_context_window,
            auto_config,
            compact_breaker: Mutex::new(AutoCompactBreaker::default()),
            compact_store,
            extract_memories: self.extract_memories,
            memory_dir: self.memory_dir,
            auto_dream: std::sync::Mutex::new(self.auto_dream),
            progress_every_turns,
            pause_signals: Arc::new(DashMap::new()),
            cancel_tokens: Arc::new(DashMap::new()),
            budget_overrides: Arc::new(DashMap::new()),
            pending_interrupts: Arc::new(DashMap::new()),
            bin_path,
            socket_path,
            _socket_handle: socket_handle,
            socket_cancel,
            cancel_root,
        })
    }
}

impl DriverOrchestrator {
    pub fn builder() -> DriverOrchestratorBuilder {
        DriverOrchestratorBuilder::default()
    }

    /// Phase 80.1.b.b.b.b — runtime-attach the autoDream hook.
    /// Used when the boot wire constructs the orchestrator before
    /// the per-agent runner loop runs (the standard order today,
    /// see `boot_dispatch_ctx_if_enabled`). Pass `None` to detach.
    /// Atomic + thread-safe: subsequent `run_turn` calls observe
    /// the new value on their next mutex acquire.
    pub fn set_auto_dream(&self, hook: Option<Arc<dyn AutoDreamHook>>) {
        let mut guard = self
            .auto_dream
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *guard = hook;
    }

    /// Read-only accessor used by tests + observability.
    pub fn has_auto_dream(&self) -> bool {
        self.auto_dream
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    /// Phase 67.C.2 — request the goal's loop to hold before its
    /// next turn. Idempotent. No-op when the goal isn't running.
    pub fn pause_goal(&self, goal_id: GoalId) -> bool {
        if let Some(tx) = self.pause_signals.get(&goal_id) {
            // B11 — `send` returns Err when no receivers exist
            // (e.g. between pre_register_goal and run_goal start).
            // `send_replace` updates the value unconditionally so
            // the loop sees `true` whenever it does subscribe.
            tx.send_replace(true);
            return true;
        }
        false
    }

    /// Phase 67.C.2 — release a paused goal's loop. Idempotent.
    pub fn resume_goal(&self, goal_id: GoalId) -> bool {
        if let Some(tx) = self.pause_signals.get(&goal_id) {
            tx.send_replace(false);
            return true;
        }
        false
    }

    /// True if the goal is currently paused. False if it's running
    /// or unknown.
    pub fn is_paused(&self, goal_id: GoalId) -> bool {
        self.pause_signals
            .get(&goal_id)
            .map(|tx| *tx.borrow())
            .unwrap_or(false)
    }

    /// B2 — install or grow the budget override for a running
    /// goal. `max_turns` only goes up (caller is expected to
    /// guard against shrink-below-used). Returns the effective
    /// `max_turns` after merge with the prior override (if any).
    pub fn set_goal_max_turns(&self, goal_id: GoalId, new_max: u32) -> Option<u32> {
        if !self.cancel_tokens.contains_key(&goal_id) {
            return None;
        }
        let mut entry = self
            .budget_overrides
            .entry(goal_id)
            .or_insert_with(|| BudgetGuards {
                max_turns: new_max,
                max_wall_time: Duration::from_secs(60 * 60 * 24 * 365),
                max_tokens: u64::MAX,
                max_consecutive_denies: u32::MAX,
                max_consecutive_errors: u32::MAX,
            });
        if new_max > entry.value().max_turns {
            entry.value_mut().max_turns = new_max;
        }
        Some(entry.value().max_turns)
    }

    /// Operator-interrupt — push a free-form message that the
    /// next turn's prompt will see prepended as a "[OPERATOR
    /// INTERRUPT]" block. Use this when you want to redirect
    /// Claude mid-run without cancelling the goal. Multiple
    /// queued interrupts are concatenated in FIFO order.
    /// Returns the queue depth after the push.
    pub fn interrupt_goal(&self, goal_id: GoalId, message: impl Into<String>) -> usize {
        let mut entry = self.pending_interrupts.entry(goal_id).or_default();
        entry.value_mut().push_back(message.into());
        entry.value().len()
    }

    /// Drain queued operator interrupts. The run loop calls this
    /// at the top of every iteration so the next `AttemptParams`
    /// extras carry an `operator_messages` array Claude sees in
    /// its prompt.
    pub(crate) fn drain_interrupts(&self, goal_id: GoalId) -> Vec<String> {
        self.pending_interrupts
            .get_mut(&goal_id)
            .map(|mut e| e.value_mut().drain(..).collect::<Vec<_>>())
            .unwrap_or_default()
    }

    /// B11 — pre-register the per-goal cancel + pause tokens for a
    /// goal that's being reattached after daemon restart but whose
    /// `run_goal` hasn't started yet. Lets `cancel_agent` /
    /// `pause_agent` target the goal in the gap between reattach
    /// and the actual respawn. The tokens are removed when the
    /// real `run_goal` exits, same as for fresh dispatches.
    /// Idempotent — re-registering returns the existing tokens.
    pub fn pre_register_goal(&self, goal_id: GoalId) {
        self.pause_signals.entry(goal_id).or_insert_with(|| {
            let (tx, _rx) = watch::channel(false);
            tx
        });
        self.cancel_tokens
            .entry(goal_id)
            .or_insert_with(|| self.cancel_root.child_token());
    }

    /// Phase 67.G.2 — cancel a single in-flight goal. Idempotent.
    /// Returns `true` when a goal was found and signalled. The
    /// underlying `run_goal` loop will exit at the next safe point
    /// (between turns, or when the active turn's
    /// `CancellationToken` propagates into the Claude subprocess).
    pub fn cancel_goal(&self, goal_id: GoalId) -> bool {
        if let Some(tok) = self.cancel_tokens.get(&goal_id) {
            tok.cancel();
            return true;
        }
        false
    }

    /// True iff the per-goal token has been cancelled.
    pub fn is_cancelled(&self, goal_id: GoalId) -> bool {
        self.cancel_tokens
            .get(&goal_id)
            .map(|t| t.is_cancelled())
            .unwrap_or(false)
    }

    /// Phase 67.C.1 — fire-and-forget spawn. Returns the
    /// [`tokio::task::JoinHandle`] so the caller (typically the
    /// `program_phase` tool) can register the goal in the agent
    /// registry without waiting for completion. The orchestrator is
    /// reference-counted (`Arc<Self>`); long-running goals don't
    /// pin a single owner.
    pub fn spawn_goal(
        self: Arc<Self>,
        goal: Goal,
    ) -> tokio::task::JoinHandle<Result<GoalOutcome, DriverError>> {
        tokio::spawn(async move { self.run_goal(goal).await })
    }

    /// Drive a single goal to completion. Long-running.
    pub async fn run_goal(&self, goal: Goal) -> Result<GoalOutcome, DriverError> {
        let started = Instant::now();
        let goal_id = goal.id;

        // Phase 67.C.2 + B11 — register pause signal. If
        // pre_register_goal already populated the entry (reattach
        // path), reuse the existing sender so any in-flight
        // pause request is honoured by the new loop.
        let mut pause_rx = match self.pause_signals.get(&goal_id) {
            Some(existing) => existing.value().subscribe(),
            None => {
                let (tx, rx) = watch::channel(false);
                self.pause_signals.insert(goal_id, tx);
                rx
            }
        };
        // Phase 67.G.2 + B11 — same reuse rule for the cancel
        // token: a pre-registered token has already been handed
        // out to anyone holding `cancel_goal`; clobbering it here
        // would silently drop the cancellation signal.
        let goal_cancel = match self.cancel_tokens.get(&goal_id) {
            Some(t) => t.value().clone(),
            None => {
                let t = self.cancel_root.child_token();
                self.cancel_tokens.insert(goal_id, t.clone());
                t
            }
        };

        let _ = self
            .event_sink
            .publish(DriverEvent::GoalStarted { goal: goal.clone() })
            .await;

        // 1. Workspace + mcp config.
        let workspace = self.workspace_manager.ensure(&goal).await?;
        let mcp_config_path = write_mcp_config(&workspace, &self.bin_path, &self.socket_path)?;

        // 2. Loop turns.
        let mut usage = BudgetUsage::default();
        let mut prior_failures: Vec<nexo_driver_types::AcceptanceFailure> = Vec::new();
        let mut last_acceptance: Option<AcceptanceVerdict> = None;
        let mut final_text: Option<String> = None;
        let mut total_turns: u32 = 0;
        // Phase 67.9 + 77.2 compact-policy state.
        // Phase 77.3 — load prior compact summary for session resume.
        let mut next_extras: Option<serde_json::Map<String, serde_json::Value>> =
            match self.compact_store.load(&goal.id.0.to_string(), &goal_id).await {
                Ok(Some(prior)) => {
                    let mut e = serde_json::Map::new();
                    e.insert(
                        "compact_summary".into(),
                        serde_json::Value::String(prior.summary),
                    );
                    Some(e)
                }
                _ => None,
            };
        let mut last_was_compact = false;
        // Phase 77.3 — token count before the compact turn (captured at
        // schedule time, consumed in the compact-turn handler for
        // CompactSummary persistence).
        let mut compact_before_tokens: u64 = 0;
        let final_outcome: AttemptOutcome;

        loop {
            // Phase 67.C.2 — honour pause requests before advancing
            // to the next turn. We hold here in a cancellation-aware
            // wait; cancel still wins over a stuck pause.
            while *pause_rx.borrow() {
                if goal_cancel.is_cancelled() {
                    break;
                }
                tokio::select! {
                    _ = pause_rx.changed() => {}
                    _ = goal_cancel.cancelled() => break,
                }
            }

            // B2 — operator override (set_goal_max_turns) lifts the
            // turn cap for in-flight goals. Other axes still come
            // from the original goal.budget.
            let effective_budget = match self.budget_overrides.get(&goal_id) {
                Some(o) => BudgetGuards {
                    max_turns: o.value().max_turns.max(goal.budget.max_turns),
                    ..goal.budget.clone()
                },
                None => goal.budget.clone(),
            };
            if let Some(axis) = effective_budget.is_exhausted(&usage) {
                let _ = self
                    .event_sink
                    .publish(DriverEvent::BudgetExhausted {
                        goal_id,
                        axis,
                        usage: usage.clone(),
                    })
                    .await;
                final_outcome = AttemptOutcome::BudgetExhausted { axis };
                break;
            }
            if goal_cancel.is_cancelled() {
                final_outcome = AttemptOutcome::Cancelled;
                break;
            }

            let _ = self
                .event_sink
                .publish(DriverEvent::AttemptStarted {
                    goal_id,
                    turn_index: total_turns,
                    usage: usage.clone(),
                })
                .await;

            let cancel = goal_cancel.clone();
            let mut extras = next_extras.take().unwrap_or_else(|| {
                build_attempt_extras(&prior_failures, &goal.budget, total_turns)
            });
            // Operator-interrupt drain — the agent-side
            // `interrupt_agent` tool queues messages here; we
            // surface them to the turn under
            // `operator_messages` so attempt.rs can prepend them
            // to the Claude prompt as an `[OPERATOR INTERRUPT]`
            // block.
            let pending = self.drain_interrupts(goal_id);
            if !pending.is_empty() {
                extras.insert(
                    "operator_messages".into(),
                    serde_json::Value::Array(
                        pending.into_iter().map(serde_json::Value::String).collect(),
                    ),
                );
            }
            let params = AttemptParams {
                goal: goal.clone(),
                turn_index: total_turns,
                usage: usage.clone(),
                prior_decisions: Vec::new(),
                cancel,
                extras,
            };

            // Phase 67.6 — checkpoint pre-attempt. Sentinel
            // `<no-git>` short-circuits diff_stat below.
            let cp_label = format!("turn-{total_turns}-pre");
            let cp_sha = self
                .workspace_manager
                .checkpoint(&workspace, &cp_label)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(target: "driver-loop", "checkpoint failed: {e}");
                    crate::workspace::WorkspaceManager::NO_GIT_SENTINEL.to_string()
                });

            let ctx = AttemptContext {
                claude_cfg: &self.claude_cfg,
                binding_store: &self.binding_store,
                acceptance: &self.acceptance,
                workspace: &workspace,
                mcp_config_path: &mcp_config_path,
                bin_path: &self.bin_path,
                cancel: goal_cancel.clone(),
            };
            tracing::info!(
                target: "driver-loop",
                goal_id = ?goal_id,
                turn_index = total_turns,
                "phase78: spawning attempt",
            );
            let mut result = run_attempt(ctx, params).await?;
            tracing::info!(
                target: "driver-loop",
                goal_id = ?goal_id,
                turn_index = total_turns,
                outcome = ?result.outcome,
                "phase78: attempt returned",
            );

            // Phase 67.6 — best-effort diff_stat injection.
            if cp_sha != crate::workspace::WorkspaceManager::NO_GIT_SENTINEL {
                if let Ok(diff) = self.workspace_manager.diff_stat(&workspace, &cp_sha).await {
                    if !diff.trim().is_empty() {
                        result
                            .harness_extras
                            .insert("worktree.diff_stat".into(), serde_json::Value::String(diff));
                        result.harness_extras.insert(
                            "worktree.checkpoint_sha".into(),
                            serde_json::Value::String(cp_sha.clone()),
                        );
                    }
                }
            }

            usage = result.usage_after.clone();

            // Phase 67.9 + 77.2 — compact turns are meta: absorb tokens,
            // do not bump turn counter, do not process outcome as work.
            if last_was_compact {
                last_was_compact = false;
                // Circuit breaker: record failure if compact turn errored.
                let compact_ok = matches!(
                    result.outcome,
                    AttemptOutcome::Done | AttemptOutcome::NeedsRetry { .. }
                );
                if compact_ok {
                    self.compact_breaker.lock().unwrap().record_success(total_turns);
                } else {
                    self.compact_breaker.lock().unwrap().record_failure();
                }
                let _ = self
                    .event_sink
                    .publish(DriverEvent::AttemptCompleted {
                        result: result.clone(),
                    })
                    .await;
                let _ = self
                    .event_sink
                    .publish(DriverEvent::CompactCompleted {
                        goal_id,
                        turn_index: total_turns,
                        after_tokens: usage.tokens,
                    })
                    .await;
                // Phase 77.3 — persist compact summary for session resume.
                if compact_ok {
                    if let Some(ref summary_text) = result.final_text {
                        let summary = CompactSummary {
                            agent_id: goal.id.0.to_string(),
                            summary: summary_text.clone(),
                            turn_index: total_turns,
                            before_tokens: compact_before_tokens,
                            after_tokens: usage.tokens,
                            stored_at: chrono::Utc::now(),
                        };
                        let _ = self.compact_store.store(summary).await;
                    }
                    let _ = self
                        .event_sink
                        .publish(DriverEvent::CompactSummaryStored {
                            goal_id,
                            turn_index: total_turns,
                            before_tokens: compact_before_tokens,
                            after_tokens: usage.tokens,
                        })
                        .await;
                    // Phase 77.3 + 77.5 — post-compact cleanup + extraction tick.
                    let mut cleanup = PostCompactCleanup::new();
                    if let (Some(ref extract), Some(ref memory_dir)) =
                        (&self.extract_memories, &self.memory_dir)
                    {
                        cleanup = cleanup
                            .with_extract_memories(Arc::clone(extract), memory_dir.clone());
                    }
                    cleanup.run().await;
                }
                continue;
            }

            final_text = result.final_text.clone();
            last_acceptance = result.acceptance.clone();
            total_turns += 1;
            usage.turns = total_turns;

            let _ = self
                .event_sink
                .publish(DriverEvent::AttemptCompleted {
                    result: result.clone(),
                })
                .await;

            // Phase 67.C.1 — periodic progress beacon. `0` disables.
            if self.progress_every_turns > 0
                && total_turns > 0
                && total_turns % self.progress_every_turns == 0
            {
                let _ = self
                    .event_sink
                    .publish(DriverEvent::Progress {
                        goal_id,
                        turn_index: total_turns,
                        usage: usage.clone(),
                        last_text: result.final_text.clone(),
                    })
                    .await;
            }

            // Phase 77.5 — post-turn memory extraction.
            if let Some(ref extract) = self.extract_memories {
                extract.tick();
                match extract.check_gates() {
                    Ok(()) => {
                        // Gates passed — spawn extraction.
                        if let (Some(ref memory_dir), Some(ref final_text)) =
                            (&self.memory_dir, &result.final_text)
                        {
                            let messages_text = final_text.clone();
                            let dir = memory_dir.clone();
                            extract.extract(goal_id, total_turns, messages_text, dir);
                        }
                    }
                    Err(skip_reason) => {
                        let _ = self
                            .event_sink
                            .publish(DriverEvent::ExtractMemoriesSkipped {
                                goal_id,
                                reason: skip_reason,
                            })
                            .await;
                    }
                }
            }

            // Phase 80.1.b — post-turn autoDream consolidation. Mirrors
            // leak's stopHooks invocation (`autoDream.ts:316-324`).
            // Per-turn cost when enabled: one stat (lock mtime) — gates
            // fail cheap when cadence not yet due. Errors absorbed via
            // `let _ = ...` so a runner failure NEVER breaks the
            // driver-loop turn.
            // Phase 80.1.b.b.b.b — clone the Arc out of the
            // mutex so we drop the lock before awaiting the hook.
            let ad_opt = self
                .auto_dream
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            if let Some(ad) = ad_opt {
                let transcript_dir = self
                    .workspace_manager
                    .root()
                    .join(".transcripts")
                    .join(goal_id.0.to_string());
                let dream_ctx = DreamContextLite {
                    goal_id,
                    session_id: goal_id.0.to_string(),
                    transcript_dir,
                    kairos_active: false, // Phase 80.15 future
                    remote_mode: false,
                };
                let outcome_kind: AutoDreamOutcomeKind =
                    ad.check_and_run(&dream_ctx).await;
                let _ = self
                    .event_sink
                    .publish(DriverEvent::AutoDreamOutcome {
                        goal_id,
                        outcome_kind,
                    })
                    .await;
            }

            // Phase 67.9 + 77.2 — opportunistic /compact: if pressure
            // crosses threshold or session age expires, schedule next
            // iteration as a compact turn.
            let session_age_minutes = started.elapsed().as_secs() / 60;
            let max_failures = self
                .auto_config
                .as_ref()
                .map(|a| a.max_consecutive_failures)
                .unwrap_or(3);
            let should_check = {
                let breaker = self.compact_breaker.lock().unwrap();
                !breaker.is_tripped(max_failures)
            };
            if should_check {
                let last_compact = self.compact_breaker.lock().unwrap().last_compact_turn;
                let compact_ctx = CompactContext {
                    goal_id,
                    turn_index: total_turns,
                    usage: &usage,
                    context_window: self.compact_context_window,
                    last_compact_turn: last_compact,
                    goal_description: &goal.description,
                    session_age_minutes,
                    auto_config: self.auto_config.as_ref(),
                };
                if let Some((focus, trigger)) = self.compact_policy.classify(&compact_ctx).await {
                    let before_tokens = usage.tokens;
                    compact_before_tokens = before_tokens;
                    let age_minutes = session_age_minutes;
                    let pressure = if self.compact_context_window > 0 {
                        usage.tokens as f64 / self.compact_context_window as f64
                    } else {
                        0.0
                    };
                    let _ = self
                        .event_sink
                        .publish(DriverEvent::CompactRequested {
                            goal_id,
                            turn_index: total_turns,
                            focus: focus.clone(),
                            token_pressure: pressure,
                            before_tokens,
                            age_minutes,
                            trigger,
                        })
                        .await;
                    let mut e = serde_json::Map::new();
                    e.insert("compact_turn".into(), serde_json::Value::Bool(true));
                    e.insert("compact_focus".into(), serde_json::Value::String(focus));
                    next_extras = Some(e);
                    last_was_compact = true;
                }
            }
            if let Some(v) = &result.acceptance {
                let _ = self
                    .event_sink
                    .publish(DriverEvent::Acceptance {
                        goal_id,
                        verdict: v.clone(),
                    })
                    .await;
            }

            match &result.outcome {
                AttemptOutcome::Done => {
                    usage.consecutive_errors = 0;
                    final_outcome = AttemptOutcome::Done;
                    break;
                }
                AttemptOutcome::NeedsRetry { failures } => {
                    usage.consecutive_errors = 0;
                    prior_failures = failures.clone();
                    continue;
                }
                AttemptOutcome::Sleep {
                    duration_ms,
                    reason,
                } => {
                    usage.consecutive_errors = 0;
                    prior_failures.clear();
                    let wake = ScheduledWake {
                        duration_ms: *duration_ms,
                        reason: reason.clone(),
                        sleep_started_at: Instant::now(),
                    };
                    match wait_for_wake(&wake, &goal_cancel).await {
                        WakeResult::Cancelled => {
                            final_outcome = AttemptOutcome::Cancelled;
                            break;
                        }
                        WakeResult::Fired { elapsed_ms } => {
                            let mut extras =
                                build_attempt_extras(&prior_failures, &goal.budget, total_turns);
                            extras.insert(
                                "synthetic_tick_prompt".into(),
                                serde_json::Value::String(build_tick_prompt(&wake, elapsed_ms)),
                            );
                            next_extras = Some(extras);
                            continue;
                        }
                    }
                }
                AttemptOutcome::Continue { reason } | AttemptOutcome::Escalate { reason } => {
                    // Phase 67.8 — replay-policy classifies the
                    // error and decides whether to retry the same
                    // turn with a fresh session, advance to the
                    // next turn, or escalate.
                    let hint = match &result.outcome {
                        AttemptOutcome::Continue { .. } => ReplayOutcomeHint::Continue,
                        _ => ReplayOutcomeHint::Escalate,
                    };
                    let cp_for_replay =
                        if cp_sha == crate::workspace::WorkspaceManager::NO_GIT_SENTINEL {
                            None
                        } else {
                            Some(cp_sha.as_str())
                        };
                    let ctx = ReplayContext {
                        goal_id,
                        turn_index: total_turns,
                        pre_turn_checkpoint: cp_for_replay,
                        usage: &usage,
                        error_message: reason,
                        last_outcome_hint: hint,
                    };
                    let decision = self.replay_policy.classify(&ctx).await;
                    tracing::info!(
                        target: "driver-loop",
                        goal_id = ?goal_id,
                        turn_index = total_turns,
                        reason = %reason,
                        decision = ?decision,
                        "phase78: replay decision",
                    );
                    let _ = self
                        .event_sink
                        .publish(DriverEvent::ReplayDecision {
                            goal_id,
                            turn_index: total_turns,
                            decision: decision.clone(),
                            error_message: reason.clone(),
                        })
                        .await;
                    match decision {
                        ReplayDecision::FreshSessionRetry { rollback_to } => {
                            let _ = self.binding_store.mark_invalid(goal_id).await;
                            if let Some(sha) = rollback_to {
                                let _ = self.workspace_manager.rollback(&workspace, &sha).await;
                            }
                            usage.consecutive_errors = usage.consecutive_errors.saturating_add(1);
                            // NO bump turn_index — same logical turn retries.
                            // Undo the +1 bump that happened above.
                            total_turns = total_turns.saturating_sub(1);
                            usage.turns = total_turns;
                            prior_failures.clear();
                            tracing::info!(
                                target: "driver-loop",
                                goal_id = ?goal_id,
                                "phase78: FreshSessionRetry — looping",
                            );
                            continue;
                        }
                        ReplayDecision::NextTurn { rollback_to } => {
                            if let Some(sha) = rollback_to {
                                let _ = self.workspace_manager.rollback(&workspace, &sha).await;
                            }
                            prior_failures.clear();
                            tracing::info!(
                                target: "driver-loop",
                                goal_id = ?goal_id,
                                next_turn = total_turns,
                                "phase78: NextTurn — looping",
                            );
                            continue;
                        }
                        ReplayDecision::Escalate { reason } => {
                            let _ = self
                                .event_sink
                                .publish(DriverEvent::Escalate {
                                    goal_id,
                                    reason: reason.clone(),
                                })
                                .await;
                            final_outcome = AttemptOutcome::Escalate { reason };
                            break;
                        }
                    }
                }
                AttemptOutcome::Cancelled => {
                    final_outcome = AttemptOutcome::Cancelled;
                    break;
                }
                AttemptOutcome::BudgetExhausted { axis } => {
                    let _ = self
                        .event_sink
                        .publish(DriverEvent::BudgetExhausted {
                            goal_id,
                            axis: *axis,
                            usage: usage.clone(),
                        })
                        .await;
                    final_outcome = AttemptOutcome::BudgetExhausted { axis: *axis };
                    break;
                }
            }
        }

        let outcome = GoalOutcome {
            goal_id,
            outcome: final_outcome,
            total_turns,
            usage,
            final_text,
            acceptance: last_acceptance,
            elapsed: started.elapsed(),
        };
        let _ = self
            .event_sink
            .publish(DriverEvent::GoalCompleted {
                outcome: outcome.clone(),
            })
            .await;
        // Phase 67.C.2 — clean up pause signal once the loop exits.
        self.pause_signals.remove(&goal_id);
        // Phase 67.G.2 — drop the per-goal cancel token.
        self.cancel_tokens.remove(&goal_id);
        // B2 — drop budget override entry so a new spawn of the
        // same id starts fresh.
        self.budget_overrides.remove(&goal_id);
        // S3 — also clear any queued operator interrupts that
        // never made it into a turn (e.g. queued late while the
        // goal was already wrapping up). Without this they leak.
        self.pending_interrupts.remove(&goal_id);
        Ok(outcome)
    }

    /// Cancel every in-flight goal + drain socket server.
    pub async fn shutdown(self) -> Result<(), DriverError> {
        self.cancel_root.cancel();
        self.socket_cancel.cancel();
        let _ = self._socket_handle.await;
        Ok(())
    }
}

fn build_attempt_extras(
    prior_failures: &[nexo_driver_types::AcceptanceFailure],
    budget: &BudgetGuards,
    turn_index: u32,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert(
        "turn_index".into(),
        serde_json::Value::Number(turn_index.into()),
    );
    m.insert(
        "max_turns".into(),
        serde_json::Value::Number(budget.max_turns.into()),
    );
    if !prior_failures.is_empty() {
        m.insert(
            "prior_failures".into(),
            serde_json::to_value(prior_failures).unwrap_or(serde_json::Value::Null),
        );
    }
    m
}
