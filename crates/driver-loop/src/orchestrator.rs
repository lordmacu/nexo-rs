//! Goal-level loop. Takes a `Goal`, drives it to completion through
//! the per-turn attempt loop. Emits NATS events at every major
//! transition.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nexo_driver_claude::SessionBindingStore;
use nexo_driver_permission::PermissionDecider;
use nexo_driver_types::{
    AcceptanceVerdict, AttemptOutcome, AttemptParams, BudgetGuards, BudgetUsage, CancellationToken,
    Goal, GoalId,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken as TokioCancel;

use crate::acceptance::{AcceptanceEvaluator, NoopAcceptanceEvaluator};
use crate::attempt::{run_attempt, AttemptContext};
use crate::compact::{CompactContext, CompactPolicy, DefaultCompactPolicy};
use crate::error::DriverError;
use crate::events::{DriverEvent, DriverEventSink, NoopEventSink};
use crate::mcp_config::write_mcp_config;
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
    /// Phase 67.9 — opportunistic /compact policy.
    compact_policy: Arc<dyn CompactPolicy>,
    compact_context_window: u64,
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
            progress_every_turns,
            pause_signals: Arc::new(DashMap::new()),
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

    /// Phase 67.C.2 — request the goal's loop to hold before its
    /// next turn. Idempotent. No-op when the goal isn't running.
    pub fn pause_goal(&self, goal_id: GoalId) -> bool {
        if let Some(tx) = self.pause_signals.get(&goal_id) {
            let _ = tx.send(true);
            return true;
        }
        false
    }

    /// Phase 67.C.2 — release a paused goal's loop. Idempotent.
    pub fn resume_goal(&self, goal_id: GoalId) -> bool {
        if let Some(tx) = self.pause_signals.get(&goal_id) {
            let _ = tx.send(false);
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

        // Phase 67.C.2 — register pause signal for this goal.
        let (pause_tx, mut pause_rx) = watch::channel(false);
        self.pause_signals.insert(goal_id, pause_tx);

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
        // Phase 67.9 compact-policy state.
        let mut last_compact_turn: Option<u32> = None;
        let mut next_extras: Option<serde_json::Map<String, serde_json::Value>> = None;
        let mut last_was_compact = false;
        let final_outcome: AttemptOutcome;

        loop {
            // Phase 67.C.2 — honour pause requests before advancing
            // to the next turn. We hold here in a cancellation-aware
            // wait; cancel still wins over a stuck pause.
            while *pause_rx.borrow() {
                if self.cancel_root.is_cancelled() {
                    break;
                }
                tokio::select! {
                    _ = pause_rx.changed() => {}
                    _ = self.cancel_root.cancelled() => break,
                }
            }

            if let Some(axis) = goal.budget.is_exhausted(&usage) {
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
            if self.cancel_root.is_cancelled() {
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

            let cancel = self.cancel_root.clone();
            let extras = next_extras
                .take()
                .unwrap_or_else(|| build_attempt_extras(&prior_failures, &goal.budget, total_turns));
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
                cancel: self.cancel_root.clone(),
            };
            let mut result = run_attempt(ctx, params).await?;

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

            // Phase 67.9 — compact turns are meta: absorb tokens, do
            // not bump turn counter, do not process outcome as work.
            if last_was_compact {
                last_compact_turn = Some(total_turns);
                last_was_compact = false;
                let _ = self
                    .event_sink
                    .publish(DriverEvent::AttemptCompleted {
                        result: result.clone(),
                    })
                    .await;
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
                && total_turns.is_multiple_of(self.progress_every_turns)
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

            // Phase 67.9 — opportunistic /compact: if pressure crosses
            // threshold, schedule next iteration as a compact turn.
            let compact_ctx = CompactContext {
                goal_id,
                turn_index: total_turns,
                usage: &usage,
                context_window: self.compact_context_window,
                last_compact_turn,
                goal_description: &goal.description,
            };
            if let Some(focus) = self.compact_policy.classify(&compact_ctx).await {
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
                    })
                    .await;
                let mut e = serde_json::Map::new();
                e.insert("compact_turn".into(), serde_json::Value::Bool(true));
                e.insert("compact_focus".into(), serde_json::Value::String(focus));
                next_extras = Some(e);
                last_was_compact = true;
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
                            continue;
                        }
                        ReplayDecision::NextTurn { rollback_to } => {
                            if let Some(sha) = rollback_to {
                                let _ = self.workspace_manager.rollback(&workspace, &sha).await;
                            }
                            prior_failures.clear();
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
