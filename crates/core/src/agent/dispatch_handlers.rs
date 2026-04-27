//! Phase 67-PT-1 — `ToolHandler` adapters for the dispatch
//! subsystem. Bridges the agent loop's `(AgentContext, Value) ->
//! Result<Value>` shape onto the typed
//! `program_phase_dispatch` / `list_agents` / `agent_status` /
//! `cancel_agent` / `pause_agent` / `resume_agent` /
//! `update_budget` / `agent_logs_tail` / `agent_hooks_list`
//! functions that ship in `nexo-dispatch-tools`.
//!
//! The adapter lives in `nexo-core` (not in `dispatch-tools`)
//! because dispatch-tools does not depend on nexo-core; the
//! reverse direction is the one in the crate graph.
//!
//! Runtime context — orchestrator, registry, tracker, hook
//! registry — flows through `AgentContext::dispatch`. The runtime
//! attaches it once at boot (see `register_dispatch_tools_into`)
//! and every subsequent tool invocation reads through the same
//! Arc.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_agent_registry::{AgentRegistry, LogBuffer};
use nexo_dispatch_tools::policy_gate::CapSnapshot;
use nexo_dispatch_tools::{
    agent_hooks_list, agent_logs_tail, agent_status, cancel_agent, list_agents, pause_agent,
    program_phase_dispatch, resume_agent, update_budget, AgentHooksListInput, AgentLogsTailInput,
    AgentStatusInput, CancelAgentInput, DispatchDeniedPayload, DispatchSpawnedPayload,
    HookRegistry, ListAgentsInput, PauseAgentInput, ProgramPhaseInput, ProgramPhaseOutput,
    UpdateBudgetInput,
};
use nexo_driver_claude::{DispatcherIdentity, OriginChannel};
use nexo_driver_loop::DriverOrchestrator;
use nexo_driver_types::GoalId;
use nexo_llm::ToolDef;
#[allow(unused_imports)]
use nexo_project_tracker::tracker::ProjectTracker;
use nexo_project_tracker::MutableTracker;
use serde_json::{json, Value};

use super::context::AgentContext;
use super::tool_registry::{ToolHandler, ToolRegistry};

/// Bundle of runtime services the dispatch tool handlers consult
/// on every invocation. Constructed once at boot, shared via
/// `Arc` through `AgentContext::dispatch`.
pub struct DispatchToolContext {
    /// Hot-swappable tracker. The runtime can install a fresh
    /// `FsProjectTracker` mid-conversation when Cody calls
    /// `set_active_workspace` or `init_project`. Day-to-day reads
    /// stay lock-free thanks to `MutableTracker`'s `ArcSwap`.
    pub tracker: Arc<MutableTracker>,
    pub orchestrator: Arc<DriverOrchestrator>,
    pub registry: Arc<AgentRegistry>,
    pub hooks: Arc<HookRegistry>,
    pub log_buffer: Arc<LogBuffer>,
    /// Phase 71.3 — exposed so the shutdown drain in `src/main.rs`
    /// can fire `notify_origin` / `notify_channel` on every Running
    /// goal before the channel plugins go down. Same `Arc` the
    /// `EventForwarder` was wired with at boot. `None` in tests
    /// that don't exercise hook firing.
    pub hook_dispatcher: Option<Arc<dyn nexo_dispatch_tools::HookDispatcher>>,
    /// Phase 72 — durable per-turn audit log. `None` keeps the
    /// legacy in-memory-only behaviour; production boot wires
    /// `SqliteTurnLogStore` so `agent_turns_tail` can replay every
    /// turn after a restart.
    pub turn_log: Option<Arc<dyn nexo_agent_registry::TurnLogStore>>,
    /// Default cap snapshot the gate consumes. The runtime can
    /// refresh `global_running` per call from the live registry;
    /// the rest stays config-driven.
    pub default_caps: CapSnapshot,
    pub require_trusted: bool,
    /// PT-3 — telemetry sink consulted on every dispatch /
    /// hook outcome. Defaults to `NoopTelemetry`; production
    /// boot wires `NatsDispatchTelemetry` (PT-7).
    pub telemetry: Arc<dyn nexo_dispatch_tools::DispatchTelemetry>,
    /// Self-modify gate. When `false`, dispatch tools that would
    /// target the daemon's own source workspace (i.e. the daemon
    /// is running under the same git root the goal is about to
    /// modify) are refused with a clean error. Default `false` to
    /// fail-safe in production; dev sets
    /// `NEXO_ALLOW_SELF_MODIFY=1` to flip it on.
    pub allow_self_modify: bool,
    /// Path the daemon process is running from. Compared against
    /// the active tracker root to decide whether a dispatch is a
    /// self-modify attempt. Snapshot at boot.
    pub daemon_source_root: std::path::PathBuf,
    /// When `true`, every successful goal admit auto-attaches a
    /// `DispatchAudit` hook so a fresh audit goal runs after the
    /// parent's acceptance passes. The audit reports findings
    /// (bugs, incomplete followups, missing tests) without
    /// fixing them — the operator decides which to dispatch as
    /// fix-goals. Default `true` for the canonical Cody flow;
    /// set to `false` for noisy throwaway runs.
    pub audit_before_done: bool,
    /// Chainer used by hook dispatcher to spawn DispatchPhase /
    /// DispatchAudit goals. `None` keeps hook chaining disabled
    /// (useful for read-only configurations).
    pub chainer: Option<Arc<dyn nexo_dispatch_tools::DispatchPhaseChainer>>,
}

impl DispatchToolContext {
    fn caps_snapshot(&self) -> CapSnapshot {
        let mut c = self.default_caps;
        c.global_running = self.registry.count_running();
        c
    }

    fn dispatcher_for(&self, ctx: &AgentContext) -> DispatcherIdentity {
        DispatcherIdentity {
            agent_id: ctx.agent_id.clone(),
            sender_id: None,
            parent_goal_id: None,
            chain_depth: 0,
        }
    }

    fn origin_for(&self, ctx: &AgentContext) -> Option<OriginChannel> {
        // B3 — runtime intake stamps `inbound_origin` into the
        // context after binding resolution; we lift it into an
        // OriginChannel so notify_origin knows where to send the
        // completion summary.
        ctx.inbound_origin
            .as_ref()
            .map(|(plugin, instance, sender)| OriginChannel {
                plugin: plugin.clone(),
                instance: instance.clone(),
                sender_id: sender.clone(),
                correlation_id: None,
            })
    }

    fn dispatch_policy(&self, ctx: &AgentContext) -> nexo_config::DispatchPolicy {
        ctx.effective_policy().dispatch_policy.clone()
    }

    /// True when the active tracker root resolves to the same
    /// path the daemon was launched from. Used by
    /// ProgramPhaseHandler to refuse self-modify attempts when
    /// `allow_self_modify=false`.
    pub fn is_self_modify_target(&self) -> bool {
        let active = self.tracker.root();
        let daemon = &self.daemon_source_root;
        // Canonicalise both sides so '/proj' and '/proj/.' compare
        // equal. Failing canonicalise (path missing on disk) falls
        // back to direct equality so we err on the side of warning.
        let a = std::fs::canonicalize(&active).unwrap_or(active);
        let b = std::fs::canonicalize(daemon).unwrap_or_else(|_| daemon.clone());
        a == b
    }
}

fn missing_dispatch_ctx() -> anyhow::Error {
    anyhow::anyhow!("dispatch tools require AgentContext.dispatch to be set at boot")
}

fn dispatch_ctx(ctx: &AgentContext) -> anyhow::Result<Arc<DispatchToolContext>> {
    ctx.dispatch.clone().ok_or_else(missing_dispatch_ctx)
}

// ─── Handlers ──────────────────────────────────────────────────

pub struct ProgramPhaseHandler;

#[async_trait]
impl ToolHandler for ProgramPhaseHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: ProgramPhaseInput = serde_json::from_value(args)?;
        let policy = dispatch.dispatch_policy(ctx);
        // Self-modify gate. Refuses when the operator is asking
        // Cody to dispatch a goal against the same source the
        // daemon runs from, AND the env hasn't opted in to
        // self-modification. Production deploys leave this off.
        if dispatch.is_self_modify_target() && !dispatch.allow_self_modify {
            return Ok(serde_json::to_value(
                nexo_dispatch_tools::ProgramPhaseOutput::Forbidden {
                    phase_id: input.phase_id.clone(),
                    reason: "self-modify is disabled (set NEXO_ALLOW_SELF_MODIFY=1 in dev to enable, or switch to a different workspace via init_project / set_active_workspace)".into(),
                },
            )?);
        }
        let out = program_phase_dispatch(
            input,
            dispatch.tracker.as_ref(),
            dispatch.orchestrator.clone(),
            dispatch.registry.clone(),
            &policy,
            dispatch.require_trusted,
            ctx.sender_trusted,
            dispatch.dispatcher_for(ctx),
            dispatch.origin_for(ctx),
            dispatch.caps_snapshot(),
            Some(dispatch.hooks.clone()),
        )
        .await
        .map_err(|e| anyhow::anyhow!("program_phase: {e}"))?;
        // PT-3 — telemetry on dispatch outcome.
        match &out {
            ProgramPhaseOutput::Dispatched { goal_id, phase_id } => {
                // Auto-attach audit hook on admit.
                if dispatch.audit_before_done {
                    // B19 + S1 — idempotent attach + clean uuid id.
                    dispatch.hooks.add_unique(
                        *goal_id,
                        nexo_dispatch_tools::CompletionHook {
                            id: format!("auto-audit-{}", goal_id.0.simple()),
                            on: nexo_dispatch_tools::HookTrigger::Done,
                            action: nexo_dispatch_tools::HookAction::DispatchAudit {
                                only_if: nexo_dispatch_tools::HookTrigger::Done,
                            },
                        },
                    );
                }
                dispatch
                    .telemetry
                    .dispatch_spawned(DispatchSpawnedPayload {
                        goal_id: *goal_id,
                        phase_id: phase_id.clone(),
                        queued_position: None,
                        dispatcher_agent_id: ctx.agent_id.clone(),
                    })
                    .await;
            }
            ProgramPhaseOutput::Queued {
                goal_id,
                phase_id,
                position,
            } => {
                dispatch
                    .telemetry
                    .dispatch_spawned(DispatchSpawnedPayload {
                        goal_id: *goal_id,
                        phase_id: phase_id.clone(),
                        queued_position: Some(*position),
                        dispatcher_agent_id: ctx.agent_id.clone(),
                    })
                    .await;
            }
            ProgramPhaseOutput::Forbidden { phase_id, reason }
            | ProgramPhaseOutput::Rejected { phase_id, reason } => {
                dispatch
                    .telemetry
                    .dispatch_denied(DispatchDeniedPayload {
                        phase_id: phase_id.clone(),
                        reason: reason.clone(),
                        dispatcher_agent_id: ctx.agent_id.clone(),
                    })
                    .await;
            }
            // B13 — NotFound / NotTracked also emit so dashboards
            // see "operator asked for a phase that doesn't exist"
            // / "no PHASES.md in workspace" without grepping logs.
            ProgramPhaseOutput::NotFound { phase_id } => {
                dispatch
                    .telemetry
                    .dispatch_denied(DispatchDeniedPayload {
                        phase_id: phase_id.clone(),
                        reason: "phase_id not in PHASES.md".into(),
                        dispatcher_agent_id: ctx.agent_id.clone(),
                    })
                    .await;
            }
            ProgramPhaseOutput::NotTracked => {
                dispatch
                    .telemetry
                    .dispatch_denied(DispatchDeniedPayload {
                        phase_id: String::new(),
                        reason: "project not tracked: PHASES.md missing".into(),
                        dispatcher_agent_id: ctx.agent_id.clone(),
                    })
                    .await;
            }
        }
        Ok(serde_json::to_value(out)?)
    }
}

pub struct ListAgentsHandler;

#[async_trait]
impl ToolHandler for ListAgentsHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: ListAgentsInput = serde_json::from_value(args).unwrap_or_default();
        let out = list_agents(input, dispatch.registry.clone()).await;
        Ok(json!({ "markdown": out }))
    }
}

pub struct AgentStatusHandler;

#[async_trait]
impl ToolHandler for AgentStatusHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: AgentStatusInput = serde_json::from_value(args)?;
        let out = agent_status(input, dispatch.registry.clone()).await;
        Ok(json!({ "markdown": out }))
    }
}

pub struct CancelAgentHandler;

#[async_trait]
impl ToolHandler for CancelAgentHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: CancelAgentInput = serde_json::from_value(args)?;
        let out = cancel_agent(
            input,
            dispatch.orchestrator.clone(),
            dispatch.registry.clone(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("cancel_agent: {e}"))?;
        Ok(serde_json::to_value(out)?)
    }
}

pub struct PauseAgentHandler;

#[async_trait]
impl ToolHandler for PauseAgentHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: PauseAgentInput = serde_json::from_value(args)?;
        let out = pause_agent(
            input,
            dispatch.orchestrator.clone(),
            dispatch.registry.clone(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("pause_agent: {e}"))?;
        Ok(serde_json::to_value(out)?)
    }
}

pub struct ResumeAgentHandler;

#[async_trait]
impl ToolHandler for ResumeAgentHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: PauseAgentInput = serde_json::from_value(args)?;
        let out = resume_agent(
            input,
            dispatch.orchestrator.clone(),
            dispatch.registry.clone(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("resume_agent: {e}"))?;
        Ok(serde_json::to_value(out)?)
    }
}

pub struct UpdateBudgetHandler;

#[async_trait]
impl ToolHandler for UpdateBudgetHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: UpdateBudgetInput = serde_json::from_value(args)?;
        let out = update_budget(
            input,
            dispatch.registry.clone(),
            dispatch.orchestrator.clone(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("update_budget: {e}"))?;
        Ok(serde_json::to_value(out)?)
    }
}

pub struct AgentLogsTailHandler;

#[async_trait]
impl ToolHandler for AgentLogsTailHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: AgentLogsTailInput = serde_json::from_value(args)?;
        let out = agent_logs_tail(input, dispatch.log_buffer.clone()).await;
        Ok(json!({ "markdown": out }))
    }
}

pub struct AgentTurnsTailHandler;

#[async_trait]
impl ToolHandler for AgentTurnsTailHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: nexo_dispatch_tools::AgentTurnsTailInput = serde_json::from_value(args)?;
        let Some(store) = dispatch.turn_log.clone() else {
            return Ok(json!({
                "markdown": "turn log not enabled — set `agent_registry.store` in project_tracker.yaml so the daemon opens a sqlite-backed log."
            }));
        };
        let out = nexo_dispatch_tools::agent_turns_tail(input, store).await;
        Ok(json!({ "markdown": out }))
    }
}

pub struct AgentHooksListHandler;

#[async_trait]
impl ToolHandler for AgentHooksListHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: AgentHooksListInput = serde_json::from_value(args)?;
        let out = agent_hooks_list(input, dispatch.hooks.clone()).await;
        Ok(json!({ "markdown": out }))
    }
}

// ─── Audit chainer ─────────────────────────────────────────

/// Simple chainer that synthesises an audit goal on demand. The
/// audit prompt is hardcoded — short, decisive, no fixing. The
/// orchestrator runs it like any other goal; its acceptance is
/// just `true` so the audit's verdict goes via notify_origin
/// rather than via cargo build.
pub struct AuditChainer {
    pub orchestrator: Arc<DriverOrchestrator>,
    pub registry: Arc<nexo_agent_registry::AgentRegistry>,
    pub hooks: Arc<nexo_dispatch_tools::HookRegistry>,
    pub log_buffer: Arc<nexo_agent_registry::LogBuffer>,
    pub default_caps: nexo_dispatch_tools::policy_gate::CapSnapshot,
    /// B22 — driver workspace root. The audit goal targets the
    /// PARENT's worktree (`<workspace_root>/<parent_id>`) so
    /// Claude inside the audit sees the parent's commits.
    pub workspace_root: std::path::PathBuf,
    /// B24 — separate cap for audit goals. None = unlimited.
    pub audit_cap: Option<u32>,
}

#[async_trait]
impl nexo_dispatch_tools::DispatchPhaseChainer for AuditChainer {
    async fn chain(
        &self,
        _parent: &nexo_dispatch_tools::HookPayload,
        _phase_id: &str,
    ) -> Result<GoalId, String> {
        // Plain chain (DispatchPhase) needs the tracker handle —
        // that lives in DispatchToolContext, which we don't have
        // here. The runtime wiring will pick a richer chainer
        // when chaining via DispatchPhase is needed; this impl
        // covers the audit-only flow.
        Err("AuditChainer only supports audit(); use a richer chainer for DispatchPhase".into())
    }

    async fn audit(&self, parent: &nexo_dispatch_tools::HookPayload) -> Result<GoalId, String> {
        use nexo_agent_registry::{AgentHandle, AgentRunStatus, AgentSnapshot};
        use nexo_driver_types::{AcceptanceCriterion, BudgetGuards, Goal};

        // B24 — gate by audit_cap. Count running audit:* rows; if
        // we're at the cap, refuse the dispatch with a clear
        // reason rather than queueing forever.
        if let Some(cap) = self.audit_cap {
            let rows = self
                .registry
                .list()
                .await
                .map_err(|e| format!("registry: {e}"))?;
            let running = rows
                .iter()
                .filter(|r| {
                    matches!(r.status, AgentRunStatus::Running) && r.phase_id.starts_with("audit:")
                })
                .count() as u32;
            if running >= cap {
                return Err(format!(
                    "audit cap reached ({running}/{cap}) — parent goal {} done without audit",
                    parent.goal_id.0.simple()
                ));
            }
        }

        // B22 — parent's diff_stat lives in the registry's
        // snapshot; lift it into the prompt so Claude in the
        // audit goal has concrete context even when a different
        // worktree path doesn't replay the changes.
        let parent_diff = self
            .registry
            .handle(parent.goal_id)
            .and_then(|h| h.snapshot.last_diff_stat)
            .unwrap_or_else(|| "(diff stat unavailable)".into());

        let prompt = format!(
            "Audit the changes made by goal {parent_id} (phase {phase}).\n\n\
             ## Parent diff stat\n\
             {parent_diff}\n\n\
             ## Instructions\n\
             You are running INSIDE the parent goal's worktree, so `git diff`\n\
             / `git log` show its commits directly.\n\n\
             Look for:\n\
             - bugs introduced by the diff\n\
             - incomplete follow-ups in FOLLOWUPS.md the diff touches\n\
             - missing tests for new code paths\n\
             - stale doc lines (mdBook / inline rustdoc) the diff invalidates\n\n\
             Do NOT fix anything. Produce a numbered list with severity\n\
             (high / medium / low) and a one-line description per finding.\n\
             If nothing is found, output exactly: 'audit_clean'.",
            parent_id = parent.goal_id.0.simple(),
            phase = parent.phase_id,
            parent_diff = parent_diff,
        );

        // B22 — point at the parent's worktree so Claude sees the
        // commits. WorkspaceManager creates them deterministically
        // under <workspace_root>/<goal_id>; the goal's worktree
        // path is reachable by simple join.
        let parent_worktree = self.workspace_root.join(parent.goal_id.0.to_string());
        let parent_worktree = if parent_worktree.exists() {
            Some(parent_worktree.display().to_string())
        } else {
            // Worktree was cleaned up (cleanup_on_done=true) —
            // best-effort fallback to fresh checkout, audit will
            // mostly see no changes but at least the prompt holds
            // the diff stat.
            None
        };

        let goal = Goal {
            id: GoalId::new(),
            description: prompt,
            // Audit succeeds when Claude produces output and exits
            // cleanly. We don't run cargo here — the audit's
            // value is the report itself, surfaced via
            // notify_origin.
            acceptance: vec![AcceptanceCriterion::shell("true")],
            budget: BudgetGuards {
                max_turns: 8,
                max_wall_time: std::time::Duration::from_secs(60 * 30),
                max_tokens: 500_000,
                max_consecutive_denies: 3,
                max_consecutive_errors: 5,
            },
            workspace: parent_worktree,
            metadata: serde_json::Map::new(),
        };
        let goal_id = goal.id;

        let handle = AgentHandle {
            goal_id,
            phase_id: format!("audit:{}", parent.phase_id),
            status: AgentRunStatus::Running,
            origin: parent.origin.clone(),
            dispatcher: None,
            started_at: chrono::Utc::now(),
            finished_at: None,
            snapshot: AgentSnapshot {
                max_turns: goal.budget.max_turns,
                ..AgentSnapshot::default()
            },
            plan_mode: None,
        };
        // B24 — admit with enqueue=false so audits don't queue
        // behind main dispatch; if the audit_cap check above
        // passed, the registry's global cap shouldn't refuse
        // either, but we fail-fast here just in case.
        self.registry
            .admit(handle, false)
            .await
            .map_err(|e| format!("audit admit: {e}"))?;
        self.registry.set_max_turns(goal_id, goal.budget.max_turns);

        // Audit goals get a notify_origin hook so findings reach
        // the operator. B19 + S1.
        self.hooks.add_unique(
            goal_id,
            nexo_dispatch_tools::CompletionHook {
                id: format!("audit-notify-{}", goal_id.0.simple()),
                on: nexo_dispatch_tools::HookTrigger::Done,
                action: nexo_dispatch_tools::HookAction::NotifyOrigin,
            },
        );
        let _ = self.log_buffer.tail(goal_id, 1);
        let _ = self.default_caps.queue_when_full;

        std::mem::drop(self.orchestrator.clone().spawn_goal(goal));
        Ok(goal_id)
    }
}

// ─── Operator interrupt ─────────────────────────────────────

#[derive(serde::Deserialize)]
struct InterruptAgentInput {
    pub goal_id: GoalId,
    pub message: String,
}

pub struct InterruptAgentHandler;

#[async_trait]
impl ToolHandler for InterruptAgentHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: InterruptAgentInput = serde_json::from_value(args)?;
        let depth = dispatch
            .orchestrator
            .interrupt_goal(input.goal_id, input.message);
        Ok(json!({
            "goal_id": input.goal_id,
            "queued": true,
            "queue_depth": depth,
        }))
    }
}

// ─── Tracker read handlers ─────────────────────────────────

/// `project_phases_list` — return phases parsed from PHASES.md,
/// optionally filtered by status. Phase 67.E.x backlog item; the
/// canonical name lives in `dispatch-tools::tool_names::READ_TOOL_NAMES`
/// but the handler / register call had not been wired, so every
/// invocation came back as "unknown tool". This is the minimal
/// implementation needed to unblock chat-side queries like "qué
/// fases nos faltan" without forcing the LLM to fall back to file
/// reads (which it does not have access to).
pub struct ProjectPhasesListHandler;

#[async_trait]
impl ToolHandler for ProjectPhasesListHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let filter = args
            .get("filter")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_lowercase());
        let prefix = args
            .get("phase_prefix")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let phases = dispatch
            .tracker
            .phases()
            .await
            .map_err(|e| anyhow::anyhow!("tracker phases() failed: {e}"))?;
        let mut rows: Vec<serde_json::Value> = Vec::new();
        let want = filter.as_deref();
        for phase in &phases {
            for sub in &phase.sub_phases {
                let status_label = match sub.status {
                    nexo_project_tracker::PhaseStatus::Done => "done",
                    nexo_project_tracker::PhaseStatus::InProgress => "in_progress",
                    nexo_project_tracker::PhaseStatus::Pending => "pending",
                };
                if let Some(w) = want {
                    if !w.is_empty() && w != "all" && w != status_label {
                        continue;
                    }
                }
                if let Some(pfx) = prefix.as_deref() {
                    if !sub.id.starts_with(pfx) {
                        continue;
                    }
                }
                rows.push(serde_json::json!({
                    "phase": phase.id,
                    "phase_title": phase.title,
                    "id": sub.id,
                    "title": sub.title,
                    "status": status_label,
                }));
            }
        }
        Ok(serde_json::json!({
            "filter": filter.unwrap_or_else(|| "all".into()),
            "count": rows.len(),
            "phases": rows,
        }))
    }
}

/// `project_status` — high-level snapshot of the active workspace's
/// roadmap: counts by status + a couple of representative entries.
/// `kind` lets the LLM ask for a narrower slice (`current_phase`,
/// `followups`).
pub struct ProjectStatusHandler;

#[async_trait]
impl ToolHandler for ProjectStatusHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let kind = args
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("summary")
            .to_lowercase();
        let phases = dispatch
            .tracker
            .phases()
            .await
            .map_err(|e| anyhow::anyhow!("tracker phases() failed: {e}"))?;
        let mut done = 0usize;
        let mut in_progress: Vec<&nexo_project_tracker::SubPhase> = Vec::new();
        let mut pending: Vec<&nexo_project_tracker::SubPhase> = Vec::new();
        for phase in &phases {
            for sub in &phase.sub_phases {
                match sub.status {
                    nexo_project_tracker::PhaseStatus::Done => done += 1,
                    nexo_project_tracker::PhaseStatus::InProgress => in_progress.push(sub),
                    nexo_project_tracker::PhaseStatus::Pending => pending.push(sub),
                }
            }
        }
        let total = done + in_progress.len() + pending.len();
        match kind.as_str() {
            "current_phase" => {
                let current = in_progress.first().or(pending.first());
                Ok(serde_json::json!({
                    "current_phase": current.map(|s| serde_json::json!({
                        "id": s.id,
                        "title": s.title,
                        "status": match s.status {
                            nexo_project_tracker::PhaseStatus::InProgress => "in_progress",
                            _ => "pending",
                        },
                    })),
                }))
            }
            "followups" => {
                let followups = dispatch
                    .tracker
                    .followups()
                    .await
                    .map_err(|e| anyhow::anyhow!("tracker followups() failed: {e}"))?;
                let open: Vec<_> = followups
                    .iter()
                    .filter(|f| matches!(f.status, nexo_project_tracker::FollowUpStatus::Open))
                    .map(|f| {
                        serde_json::json!({
                            "code": f.code,
                            "title": f.title,
                            "section": f.section,
                        })
                    })
                    .collect();
                Ok(serde_json::json!({
                    "open_count": open.len(),
                    "items": open,
                }))
            }
            _ => Ok(serde_json::json!({
                "total_subphases": total,
                "done": done,
                "in_progress_count": in_progress.len(),
                "pending_count": pending.len(),
                "in_progress_ids": in_progress.iter().map(|s| &s.id).collect::<Vec<_>>(),
                "next_pending_ids": pending.iter().take(5).map(|s| &s.id).collect::<Vec<_>>(),
            })),
        }
    }
}

/// `followup_detail` — fetch the full body of one follow-up by code.
pub struct FollowupDetailHandler;

#[async_trait]
impl ToolHandler for FollowupDetailHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let code = args
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("`code` is required"))?
            .to_string();
        let followups = dispatch
            .tracker
            .followups()
            .await
            .map_err(|e| anyhow::anyhow!("tracker followups() failed: {e}"))?;
        match followups.into_iter().find(|f| f.code == code) {
            Some(f) => Ok(serde_json::json!({
                "code": f.code,
                "title": f.title,
                "section": f.section,
                "status": match f.status {
                    nexo_project_tracker::FollowUpStatus::Open => "open",
                    nexo_project_tracker::FollowUpStatus::Resolved => "resolved",
                },
                "body": f.body,
            })),
            None => Ok(serde_json::json!({
                "error": format!("no follow-up with code `{code}`"),
            })),
        }
    }
}

// ─── Cody-flow handlers (preflight + workspace ops) ─────────

pub struct PreflightHandler;

#[async_trait]
impl ToolHandler for PreflightHandler {
    async fn call(&self, ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        let llm_provider = &ctx.config.model.provider;
        let llm_model = &ctx.config.model.model;
        let dispatch_ready = ctx.dispatch.is_some();
        let dispatch_capability = format!("{:?}", ctx.effective_policy().dispatch_policy.mode);
        let workspace = ctx
            .dispatch
            .as_ref()
            .map(|d| d.tracker.root().display().to_string())
            .unwrap_or_else(|| "<unset>".into());
        let tracker_ok = if let Some(d) = ctx.dispatch.as_ref() {
            d.tracker.phases().await.is_ok()
        } else {
            false
        };
        let (is_self_modify, allow_self_modify, daemon_source) =
            ctx.dispatch
                .as_ref()
                .map_or((false, false, String::from("<unset>")), |d| {
                    (
                        d.is_self_modify_target(),
                        d.allow_self_modify,
                        d.daemon_source_root.display().to_string(),
                    )
                });
        let report = serde_json::json!({
            "llm_provider": llm_provider,
            "llm_model": llm_model,
            "llm_ready": llm_provider == "anthropic" || llm_provider == "minimax",
            "dispatch_ready": dispatch_ready,
            "dispatch_capability": dispatch_capability,
            "tracker_workspace": workspace,
            "tracker_readable": tracker_ok,
            "sender_trusted": ctx.sender_trusted,
            "daemon_source_root": daemon_source,
            "is_self_modify_target": is_self_modify,
            "allow_self_modify": allow_self_modify,
        });
        Ok(report)
    }
}

#[derive(serde::Deserialize)]
struct SetActiveWorkspaceInput {
    path: String,
}

pub struct SetActiveWorkspaceHandler;

#[async_trait]
impl ToolHandler for SetActiveWorkspaceHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: SetActiveWorkspaceInput = serde_json::from_value(args)?;
        let path = std::path::PathBuf::from(input.path);
        match dispatch.tracker.switch_to(&path) {
            Ok(prev) => {
                if let Err(e) = nexo_project_tracker::state::write_active_workspace(&path) {
                    tracing::warn!(error = %e, path = %path.display(), "failed to persist active workspace — restart will revert to default");
                }
                Ok(serde_json::json!({
                    "status": "switched",
                    "previous": prev.display().to_string(),
                    "current": path.display().to_string(),
                }))
            }
            Err(e) => Ok(serde_json::json!({
                "status": "error",
                "error": e.to_string(),
                "current": dispatch.tracker.root().display().to_string(),
            })),
        }
    }
}

#[derive(serde::Deserialize)]
struct InitProjectInput {
    /// Folder name relative to the cwd (or absolute).
    name: String,
    /// One-line description for the README + first phase body.
    description: String,
    /// Optional caller-supplied list of phases. When `None`, a
    /// minimal scaffolding template is used and the LLM is
    /// expected to fill in real phases via `/forge spec` later.
    #[serde(default)]
    phases: Option<Vec<InitPhaseInput>>,
}

#[derive(Clone, serde::Deserialize)]
struct InitPhaseInput {
    /// `1.1`, `2.3`, etc. Must match `<digits>.<digits>` shape.
    id: String,
    title: String,
    /// Optional body shown in `project_status --phase`.
    #[serde(default)]
    body: Option<String>,
}

pub struct InitProjectHandler;

#[async_trait]
impl ToolHandler for InitProjectHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let dispatch = dispatch_ctx(ctx)?;
        let input: InitProjectInput = serde_json::from_value(args)?;
        let target_root: std::path::PathBuf = if std::path::Path::new(&input.name).is_absolute() {
            std::path::PathBuf::from(&input.name)
        } else {
            std::env::current_dir()
                .unwrap_or_default()
                .join(&input.name)
        };
        if let Err(e) = std::fs::create_dir_all(&target_root) {
            return Ok(serde_json::json!({
                "status": "error",
                "error": format!("create_dir_all: {e}"),
            }));
        }

        let phases_md = render_phases_md(&input);
        let followups_md = render_followups_md(&input);
        if let Err(e) = std::fs::write(target_root.join("PHASES.md"), phases_md) {
            return Ok(serde_json::json!({
                "status": "error",
                "error": format!("write PHASES.md: {e}"),
            }));
        }
        if let Err(e) = std::fs::write(target_root.join("FOLLOWUPS.md"), followups_md) {
            return Ok(serde_json::json!({
                "status": "error",
                "error": format!("write FOLLOWUPS.md: {e}"),
            }));
        }

        // Phase 76 — initialise a git repo at the project root when
        // one isn't already present and the dir lives outside the
        // daemon's source repo. Without this, the orchestrator
        // falls back to cloning the *parent* repo (nexo-rs) into
        // every per-goal worktree and Claude can't tell which
        // sub-tree is the actual project. The init runs as
        // best-effort; the project is still scaffolded if `git`
        // isn't on PATH or the parent dir is already inside another
        // repo (in which case the caller's workspace is the parent).
        let git_init_log = if !target_root.join(".git").exists() {
            init_git_repo(&target_root)
        } else {
            None
        };

        // Switch the active tracker to the new project so the next
        // `program_phase` lands inside it.
        if let Err(e) = dispatch.tracker.switch_to(&target_root) {
            return Ok(serde_json::json!({
                "status": "scaffolded_but_not_active",
                "path": target_root.display().to_string(),
                "switch_error": e.to_string(),
            }));
        }
        if let Err(e) = nexo_project_tracker::state::write_active_workspace(&target_root) {
            tracing::warn!(error = %e, path = %target_root.display(), "failed to persist active workspace — restart will revert to default");
        }

        Ok(serde_json::json!({
            "status": "ready",
            "path": target_root.display().to_string(),
            "files_created": ["PHASES.md", "FOLLOWUPS.md"],
            "active_workspace": target_root.display().to_string(),
            "git_init": git_init_log,
        }))
    }
}

/// Phase 76 — `git init` + initial commit on a fresh project so
/// the per-goal worktree can branch from it instead of cloning the
/// daemon's outer repo. Returns a short status string for the
/// `init_project` response payload; `None` when the git CLI is
/// missing entirely.
///
/// We INTENTIONALLY init even when the parent is already a git
/// repo. The whole point is that `program_phase` will detect this
/// project's own `.git` and pin the per-goal worktree to it; if
/// we deferred to the parent the worktree would clone the outer
/// repo (typically nexo-rs) and Claude would land in a tree where
/// the project root isn't obvious. Operators that want the
/// project to live outside any other repo should pass an absolute
/// path (e.g. `/tmp/<name>`) — both branches now produce a
/// stand-alone repo.
fn init_git_repo(target_root: &std::path::Path) -> Option<String> {
    use std::process::Command;
    let init = Command::new("git")
        .args([
            "-C",
            &target_root.display().to_string(),
            "init",
            "-q",
            "-b",
            "main",
        ])
        .output();
    if let Err(e) = init.as_ref() {
        return Some(format!("git init failed: {e}"));
    }
    let _ = Command::new("git")
        .args([
            "-C",
            &target_root.display().to_string(),
            "add",
            "PHASES.md",
            "FOLLOWUPS.md",
        ])
        .output();
    let _ = Command::new("git")
        .args([
            "-C",
            &target_root.display().to_string(),
            "-c",
            "user.email=nexo-driver@localhost",
            "-c",
            "user.name=nexo-driver",
            "commit",
            "-q",
            "-m",
            "init: scaffolded by nexo-driver",
        ])
        .output();
    Some("initialised empty git repo at HEAD".into())
}

fn render_phases_md(input: &InitProjectInput) -> String {
    let mut out = format!(
        "# {name} — Implementation phases\n\n{description}\n\n## Status\n\nFresh project. Sub-phases below are pending until\n`/forge ejecutar` ships them.\n\n",
        name = input.name,
        description = input.description,
    );
    let phases = input.phases.clone().unwrap_or_else(default_phases_template);
    let mut last_phase = "";
    for p in &phases {
        let phase_num = p.id.split('.').next().unwrap_or("1");
        if phase_num != last_phase {
            out.push_str(&format!("## Phase {phase_num} — Phase {phase_num}\n\n"));
            last_phase = phase_num;
        }
        out.push_str(&format!("#### {} — {}   ⬜\n", p.id, p.title));
        if let Some(body) = &p.body {
            out.push('\n');
            out.push_str(body);
            out.push_str("\n\n");
        }
    }
    out
}

fn render_followups_md(input: &InitProjectInput) -> String {
    format!(
        "# Follow-ups\n\nActive backlog for {name}.\n\n## Open items\n\n_(empty — populated as deferred work surfaces during /forge ejecutar)_\n\n## Resolved (recent highlights)\n",
        name = input.name,
    )
}

fn default_phases_template() -> Vec<InitPhaseInput> {
    vec![
        InitPhaseInput {
            id: "1.1".into(),
            title: "Project scaffold".into(),
            body: Some(
                "Initialise the build system, README, LICENSE. Acceptance: build runs end-to-end."
                    .into(),
            ),
        },
        InitPhaseInput {
            id: "1.2".into(),
            title: "Smoke test".into(),
            body: Some("First test passes. Wires CI / cargo test.".into()),
        },
        InitPhaseInput {
            id: "2.1".into(),
            title: "Core feature".into(),
            body: Some(
                "Replace this with the actual first feature. /forge spec generates the body."
                    .into(),
            ),
        },
    ]
}

// ─── Registration helper ──────────────────────────────────────

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef {
        name: name.into(),
        description: description.into(),
        parameters: schema,
    }
}

fn obj_schema(req: &[&str], props: Value) -> Value {
    json!({
        "type": "object",
        "properties": props,
        "required": req,
    })
}

/// Register the full dispatch handler suite into a base registry.
/// `ToolRegistry::apply_dispatch_capability` (run after this) is
/// what the binding-level filter uses to drop tools the binding's
/// `DispatchPolicy` does not allow.
pub fn register_dispatch_tools_into(registry: &ToolRegistry) {
    // Tracker reads — Phase 67.E backlog had named these in
    // `dispatch-tools::tool_names::READ_TOOL_NAMES` without
    // landing the handlers, so every chat call to
    // `project_phases_list` / `project_status` /
    // `followup_detail` came back as "unknown tool". Wire them
    // first so they're available even when WRITE tools get
    // stripped by per-binding capability filtering.
    registry.register(
        def(
            "project_phases_list",
            "List sub-phases parsed from PHASES.md in the active workspace. Optional `filter`: 'pending' / 'in_progress' / 'done' / 'all' (default 'all'). Optional `phase_prefix` to narrow by id prefix (e.g. '67.').",
            obj_schema(
                &[],
                json!({
                    "filter": { "type": ["string", "null"] },
                    "phase_prefix": { "type": ["string", "null"] }
                }),
            ),
        ),
        ProjectPhasesListHandler,
    );
    registry.register(
        def(
            "project_status",
            "Snapshot of the active workspace's roadmap. `kind` selects the view: 'summary' (counts + next pending ids), 'current_phase' (next phase to work on), or 'followups' (open follow-ups).",
            obj_schema(
                &[],
                json!({
                    "kind": { "type": ["string", "null"] }
                }),
            ),
        ),
        ProjectStatusHandler,
    );
    registry.register(
        def(
            "followup_detail",
            "Return the full body of one follow-up by `code` (e.g. '67.E.x' or any short code defined in FOLLOWUPS.md).",
            obj_schema(
                &["code"],
                json!({
                    "code": { "type": "string" }
                }),
            ),
        ),
        FollowupDetailHandler,
    );

    registry.register(
        def(
            "program_phase",
            "Dispatch a Goal to the driver subsystem for the given PHASES.md sub-phase id.",
            obj_schema(
                &["phase_id"],
                json!({
                    "phase_id": { "type": "string" },
                    "acceptance_override": { "type": ["array", "null"] },
                    "budget_override": { "type": ["object", "null"] }
                }),
            ),
        ),
        ProgramPhaseHandler,
    );
    registry.register(
        def(
            "list_agents",
            "List every in-flight or recent driver goal as a markdown table.",
            obj_schema(
                &[],
                json!({
                    "filter": { "type": ["string", "null"] },
                    "phase_prefix": { "type": ["string", "null"] }
                }),
            ),
        ),
        ListAgentsHandler,
    );
    registry.register(
        def(
            "agent_status",
            "Detailed snapshot for one in-flight goal.",
            obj_schema(&["goal_id"], json!({ "goal_id": { "type": "string" } })),
        ),
        AgentStatusHandler,
    );
    registry.register(
        def(
            "cancel_agent",
            "Cancel a running goal. The orchestrator stops it at the next safe point.",
            obj_schema(
                &["goal_id"],
                json!({
                    "goal_id": { "type": "string" },
                    "reason": { "type": ["string", "null"] }
                }),
            ),
        ),
        CancelAgentHandler,
    );
    registry.register(
        def(
            "pause_agent",
            "Pause a running goal between turns.",
            obj_schema(&["goal_id"], json!({ "goal_id": { "type": "string" } })),
        ),
        PauseAgentHandler,
    );
    registry.register(
        def(
            "resume_agent",
            "Resume a paused goal.",
            obj_schema(&["goal_id"], json!({ "goal_id": { "type": "string" } })),
        ),
        ResumeAgentHandler,
    );
    registry.register(
        def(
            "update_budget",
            "Grow a running goal's max_turns. Cannot shrink below current usage.",
            obj_schema(
                &["goal_id"],
                json!({
                    "goal_id": { "type": "string" },
                    "max_turns": { "type": ["integer", "null"] }
                }),
            ),
        ),
        UpdateBudgetHandler,
    );
    registry.register(
        def(
            "agent_logs_tail",
            "Last N events recorded for the goal.",
            obj_schema(
                &["goal_id"],
                json!({
                    "goal_id": { "type": "string" },
                    "lines": { "type": ["integer", "null"] }
                }),
            ),
        ),
        AgentLogsTailHandler,
    );
    registry.register(
        def(
            "agent_turns_tail",
            "Phase 72 — durable per-turn audit log. Last N rows from the goal_turns table for the goal: outcome, last decision, summary, error per turn. Survives daemon restart. Default n=20, capped at 1000.",
            obj_schema(
                &["goal_id"],
                json!({
                    "goal_id": { "type": "string" },
                    "n": { "type": ["integer", "null"] }
                }),
            ),
        ),
        AgentTurnsTailHandler,
    );
    registry.register(
        def(
            "agent_hooks_list",
            "Hooks attached to the goal (notify_origin, dispatch_phase, etc.).",
            obj_schema(&["goal_id"], json!({ "goal_id": { "type": "string" } })),
        ),
        AgentHooksListHandler,
    );
    registry.register(
        def(
            "interrupt_agent",
            "Inject an operator note into a running goal's NEXT turn. The note appears as an [OPERATOR INTERRUPT] block on top of the prompt so Claude treats it as a high-priority directive. Use this when you want to redirect Claude mid-run without cancelling. Multiple queued notes concatenate FIFO.",
            obj_schema(
                &["goal_id", "message"],
                json!({
                    "goal_id": { "type": "string" },
                    "message": { "type": "string" }
                }),
            ),
        ),
        InterruptAgentHandler,
    );
    // Cody-flow tools.
    registry.register(
        def(
            "preflight",
            "Health check: reports whether the LLM provider, dispatch capability, and project tracker are wired so Cody can program. Use it FIRST when the operator asks for any dispatch flow — refuse to dispatch if `dispatch_ready=false` or `tracker_readable=false`.",
            obj_schema(&[], json!({})),
        ),
        PreflightHandler,
    );
    registry.register(
        def(
            "set_active_workspace",
            "Point the tracker at an existing folder that already has PHASES.md / FOLLOWUPS.md. Use when the operator says 'work in /path/X'.",
            obj_schema(
                &["path"],
                json!({ "path": { "type": "string" } }),
            ),
        ),
        SetActiveWorkspaceHandler,
    );
    registry.register(
        def(
            "init_project",
            "Create a new project folder, scaffold PHASES.md + FOLLOWUPS.md from the description, and switch the active tracker to it. Use when the operator says 'create folder X and help me build Y'. The optional `phases` array lets Cody plan the work upfront; without it a minimal three-phase scaffold lands so /forge spec can fill the bodies.",
            obj_schema(
                &["name", "description"],
                json!({
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "phases": {
                        "type": ["array", "null"],
                        "items": {
                            "type": "object",
                            "required": ["id", "title"],
                            "properties": {
                                "id": { "type": "string" },
                                "title": { "type": "string" },
                                "body": { "type": ["string", "null"] }
                            }
                        }
                    }
                }),
            ),
        ),
        InitProjectHandler,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };

    use crate::session::SessionManager;

    fn empty_config() -> Arc<AgentConfig> {
        Arc::new(AgentConfig {
            id: "tester".into(),
            model: ModelConfig {
                provider: "anthropic".into(),
                model: "x".into(),
            },
            plugins: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: Vec::new(),
            skills_dir: String::new(),
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
        })
    }

    #[tokio::test]
    async fn handler_returns_friendly_error_when_dispatch_ctx_unset() {
        let cfg = empty_config();
        let ctx = AgentContext::new(
            "tester",
            cfg,
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 64)),
        );
        let h = ProgramPhaseHandler;
        let err = h
            .call(&ctx, json!({ "phase_id": "67.10" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("AgentContext.dispatch"));
    }

    #[tokio::test]
    async fn list_agents_handler_also_requires_dispatch_ctx() {
        let cfg = empty_config();
        let ctx = AgentContext::new(
            "tester",
            cfg,
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 64)),
        );
        let err = ListAgentsHandler.call(&ctx, json!({})).await.unwrap_err();
        assert!(err.to_string().contains("AgentContext.dispatch"));
    }
}
