#![allow(clippy::all)] // Phase 79 scaffolding — re-enable when 79.x fully shipped

//! Phase 79.1 — `EnterPlanMode` and `ExitPlanMode` tool handlers.
//!
//! These two handlers manage transitions on
//! `AgentContext.plan_mode`. The dispatcher gate
//! (`crates/dispatch-tools/src/policy_gate.rs`, step 10) consults the
//! same `Arc<RwLock<PlanModeState>>` to short-circuit mutating tool
//! calls while plan mode is on.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/EnterPlanModeTool/EnterPlanModeTool.ts:21-25,55-72,77-101`
//!     (zero-arg input, read-only flag, sub-agent guard, transition
//!     to `'plan'` mode).
//!   * `claude-code-leak/src/tools/ExitPlanModeTool/ExitPlanModeV2Tool.ts:147-220,243-403`
//!     (validate-input refusal pattern, restoreMode logic, teammate
//!     mailbox path — step 13 takes the equivalent role with
//!     TaskFlow wait/resume).
//!
//! v0 caveat: this step ships the state transitions and the tool
//! shapes. The pairing approval gate (step 13) wraps the
//! `ExitPlanMode::call` body in a `FlowManager::set_waiting` /
//! `resume` round-trip; until then `ExitPlanMode` is "trust the
//! model" — useful in tests and dev, NOT safe for production until
//! step 13 lands.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use crate::plan_mode::{
    format_notify_entered, format_notify_exited, PlanModeReason, PlanModeState, PriorMode,
};
use async_trait::async_trait;
use dashmap::DashMap;
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tokio::time::Duration;
use uuid::Uuid;

/// Hard cap on the plan body the model may submit via
/// `ExitPlanMode`. Brainstorm decision **a** — when real workloads
/// hit this, follow-up `final_plan_path` variant lifts from the
/// leak's disk-backed approach.
pub const PLAN_MODE_MAX_PLAN_BYTES: usize = 8 * 1024;

/// Operator decision delivered via `plan_mode_resolve` (or, in
/// production, parsed from the pairing channel as
/// `[plan-mode] approve|reject plan_id=…`).
#[derive(Debug, Clone)]
pub enum PlanApprovalDecision {
    Approve,
    Reject { reason: String },
}

/// Process-wide registry of pending plan-mode approvals. Each
/// `ExitPlanMode` call (when `require_approval: true`) installs a
/// `oneshot::Sender` keyed by `plan_id`; the resolver tool fires the
/// matching `Sender` to wake the awaiting handler.
///
/// In-process only: a daemon restart loses pending approvals — the
/// turn-log audit trail (Phase 72) preserves history, and the goal
/// reattach path (Phase 71) flips the goal to `LostOnApproval` when
/// it reads a plan-mode-On state without a live waiter (future
/// hardening: persist plan_id → goal_id mapping for full restart
/// recovery).
#[derive(Default)]
pub struct PlanApprovalRegistry {
    pending: DashMap<String, oneshot::Sender<PlanApprovalDecision>>,
}

impl PlanApprovalRegistry {
    /// Install a waiter for `plan_id`. The returned `Receiver`
    /// resolves when `resolve` is called with the matching id.
    pub fn install(&self, plan_id: String) -> oneshot::Receiver<PlanApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(plan_id, tx);
        rx
    }

    /// Resolve the waiter for `plan_id`. Returns `false` when no
    /// matching waiter exists (already resolved, expired, or never
    /// installed).
    pub fn resolve(&self, plan_id: &str, decision: PlanApprovalDecision) -> bool {
        if let Some((_, tx)) = self.pending.remove(plan_id) {
            tx.send(decision).is_ok()
        } else {
            false
        }
    }

    /// Drop the waiter without resolving — used when a timeout fires
    /// so the resolver doesn't accidentally fire a stale plan_id.
    pub fn drop_waiter(&self, plan_id: &str) {
        self.pending.remove(plan_id);
    }

    /// `true` when a waiter exists for `plan_id`. Test-only helper.
    #[cfg(test)]
    pub fn has_pending(&self, plan_id: &str) -> bool {
        self.pending.contains_key(plan_id)
    }

    /// Clear every waiter. Test-only.
    #[cfg(test)]
    pub fn clear(&self) {
        self.pending.clear();
    }
}

/// `EnterPlanMode` — read-only mid-turn mode toggle. The tool is
/// classified `ReadOnly` in `nexo_core::plan_mode::READ_ONLY_TOOLS`
/// so it stays callable even when plan mode is already on
/// (idempotent — re-entering returns the existing `entered_at`).
pub struct EnterPlanModeTool;

impl EnterPlanModeTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "EnterPlanMode".to_string(),
            description: "Enter plan mode for non-trivial implementation tasks. While in plan mode, mutating tools (Bash writes, FileEdit, FileWrite, delegation, scheduling) are refused so the model can explore the codebase and design an approach before writing code. Call ExitPlanMode { final_plan } when ready.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Optional one-line reason for entering plan mode."
                    }
                },
                "required": []
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for EnterPlanModeTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        // Sub-agent / non-interactive guard. Lift from leak
        // `EnterPlanModeTool.ts:78-80`, refined with OpenClaw
        // `research/src/acp/session-interaction-mode.ts:4-15`: only
        // goals rooted in a live channel can plan, because only they
        // have a path to deliver the operator approval that ends
        // plan mode.
        if !ctx.is_interactive() {
            anyhow::bail!(
                "EnterPlanMode is unavailable in this context (sub-agent, cron, poller, or heartbeat-spawned goal). Plan mode requires an interactive channel that can deliver an approval message."
            );
        }

        let reason_str = args
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let now = chrono::Utc::now().timestamp();
        let plan_mode_reason = PlanModeReason::ModelRequested {
            reason: reason_str.clone(),
        };
        let mut guard = ctx.plan_mode.write().await;
        let already_on = guard.is_on();
        if !already_on {
            *guard = PlanModeState::On {
                entered_at: now,
                reason: plan_mode_reason.clone(),
                prior_mode: PriorMode::default(),
            };
        }
        let entered_at = match &*guard {
            PlanModeState::On { entered_at, .. } => *entered_at,
            PlanModeState::Off => unreachable!("just set to On"),
        };
        drop(guard);

        // Phase 79.1 — emit canonical notify line. Tracing-only for
        // now; pairing parser hookup lands in step 17/follow-up.
        if !already_on {
            let notify = format_notify_entered(entered_at, &plan_mode_reason);
            tracing::info!(
                agent_id = %ctx.agent_id,
                event = "plan_mode_entered",
                "{notify}"
            );
        }

        Ok(json!({
            "entered_plan_mode": true,
            "already_in_plan_mode": already_on,
            "entered_at": entered_at,
            "reason": reason_str,
            "instructions": "You are in plan mode. Use FileRead, Glob, Grep, WebSearch and any read-only tool to explore. Mutating tools refuse with PlanModeRefusal. When the plan is ready, call ExitPlanMode { final_plan } (≤ 8 KiB)."
        }))
    }
}

/// `ExitPlanMode` — submits the plan and flips state back to `Off`.
/// v0: no operator approval; step 13 wraps this in TaskFlow
/// wait/resume so an operator must explicitly approve via the
/// pairing channel.
pub struct ExitPlanModeTool;

impl ExitPlanModeTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "ExitPlanMode".to_string(),
            description: "Submit the final plan and exit plan mode. The plan body is logged to the turn audit log and surfaced to the operator on the pairing channel for approval before mutating tools re-arm. final_plan must be ≤ 8192 bytes (UTF-8).".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "final_plan": {
                        "type": "string",
                        "description": "The complete plan to submit for approval. Markdown encouraged. Hard cap 8 KiB."
                    }
                },
                "required": ["final_plan"]
            }),
        }
    }
}

/// Reject-followup prompt template. Lift from OpenClaw
/// `research/src/agents/bash-tools.exec-approval-followup.ts:27-40`
/// — when the operator rejects the plan, the model receives this
/// canonical text so it knows to revise rather than retry the same
/// plan blindly.
fn build_reject_followup_prompt(reason: &str) -> String {
    format!(
        "Plan rejected by operator. Reason: {reason}.\n\
         Do not call ExitPlanMode again with the same plan.\n\
         Adjust the plan based on the rejection reason and present a revised plan.",
        reason = reason.trim()
    )
}

#[async_trait]
impl ToolHandler for ExitPlanModeTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let final_plan = args
            .get("final_plan")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("ExitPlanMode requires `final_plan` (string)"))?
            .to_string();

        // Phase 79.1 brainstorm decision **a** — 8 KiB hard cap;
        // structured error so the model retries with a shorter plan.
        let actual = final_plan.len();
        if actual > PLAN_MODE_MAX_PLAN_BYTES {
            return Err(anyhow::anyhow!(
                "PlanTooLarge: actual={actual} max={max}. Trim the plan and retry.",
                actual = actual,
                max = PLAN_MODE_MAX_PLAN_BYTES
            ));
        }

        // Refusal when called outside plan mode (lift leak
        // `ExitPlanModeV2Tool.ts:195-220`). Distinct from
        // `PlanModeRefusal` (which gates mutators while plan-mode is
        // on); this is the inverse rail.
        {
            let guard = ctx.plan_mode.read().await;
            if guard.is_off() {
                return Err(anyhow::anyhow!(
                    "ExitPlanMode called outside plan mode. Use EnterPlanMode first; the tool is only meaningful when a plan is in progress."
                ));
            }
        }

        // Resolve approval policy + timeout from the binding's
        // effective policy. `agent.plan_mode` is the source of
        // truth; per-binding overrides land via
        // `EffectiveBindingPolicy` follow-up.
        let policy = &ctx.config.plan_mode;
        let require_approval = policy.require_approval;
        let timeout_secs = policy.approval_timeout_secs;

        // Allocate a unique plan_id for this submission. Used both
        // for the registry key and the operator-side
        // `[plan-mode] approve|reject plan_id=…` correlation.
        let plan_id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp();

        if require_approval {
            // Install a waiter BEFORE we surface plan_id to the
            // operator so a fast resolve can never race past us.
            let registry = ctx.plan_approval_registry.clone();
            let rx = registry.install(plan_id.clone());

            // notify_origin (frozen format) — tracing for now.
            tracing::info!(
                agent_id = %ctx.agent_id,
                event = "plan_mode_awaiting_approval",
                plan_id = %plan_id,
                timeout_secs,
                "[plan-mode] awaiting approval plan_id={plan_id} (resolve via plan_mode_resolve {{ plan_id, decision: approve|reject }})"
            );

            let timeout = Duration::from_secs(timeout_secs.max(1));
            let outcome = tokio::time::timeout(timeout, rx).await;
            match outcome {
                Err(_elapsed) => {
                    // Timeout — drop the waiter so a stale
                    // resolve cannot fire later.
                    registry.drop_waiter(&plan_id);
                    tracing::warn!(
                        agent_id = %ctx.agent_id,
                        event = "plan_mode_approval_timeout",
                        plan_id = %plan_id,
                        "[plan-mode] approval timed out plan_id={plan_id}"
                    );
                    return Err(anyhow::anyhow!(
                        "ApprovalTimeout: no operator decision within {timeout_secs}s for plan_id={plan_id}. Plan mode remains active; revisit the plan or reduce scope."
                    ));
                }
                Ok(Err(_canceled)) => {
                    // Sender dropped — treat like a reject so the
                    // model gets a clear followup.
                    return Err(anyhow::anyhow!(
                        "Plan approval channel closed unexpectedly for plan_id={plan_id}. Plan mode remains active; rebuild the plan and try again."
                    ));
                }
                Ok(Ok(PlanApprovalDecision::Reject { reason })) => {
                    let followup = build_reject_followup_prompt(&reason);
                    tracing::info!(
                        agent_id = %ctx.agent_id,
                        event = "plan_mode_rejected",
                        plan_id = %plan_id,
                        "[plan-mode] rejected plan_id={plan_id} reason={reason}"
                    );
                    // Plan-mode stays ON: the model must revise.
                    return Err(anyhow::anyhow!("{followup}"));
                }
                Ok(Ok(PlanApprovalDecision::Approve)) => {
                    tracing::info!(
                        agent_id = %ctx.agent_id,
                        event = "plan_mode_approved",
                        plan_id = %plan_id,
                        "[plan-mode] approved plan_id={plan_id}"
                    );
                    // fall through to unlock
                }
            }
        }

        // Unlock — read entered_at one more time for the response,
        // then flip state to Off.
        let entered_at = {
            let mut guard = ctx.plan_mode.write().await;
            let entered_at = match &*guard {
                PlanModeState::On { entered_at, .. } => *entered_at,
                PlanModeState::Off => {
                    // Defensive: if someone else (test, race)
                    // already flipped Off between our check and
                    // here, just return.
                    return Err(anyhow::anyhow!(
                        "Plan mode flipped Off concurrently — likely an external resolver. Refusing to ack the unlock twice."
                    ));
                }
            };
            *guard = PlanModeState::Off;
            entered_at
        };

        let plan_chars = final_plan.chars().count();
        let exit_notify = format_notify_exited(&final_plan, 0); // turn-log index resolved by caller
        tracing::info!(
            agent_id = %ctx.agent_id,
            event = "plan_mode_exited",
            plan_id = %plan_id,
            "{exit_notify}"
        );

        Ok(json!({
            "exited_plan_mode": true,
            "unlocked_at": now,
            "entered_at": entered_at,
            "plan_bytes": actual,
            "plan_chars": plan_chars,
            "plan_id": plan_id,
            "awaiting_acceptance": false,
            "approval_required": require_approval,
        }))
    }
}

/// `plan_mode_resolve` — operator-side resolver tool. The pairing
/// parser (future wiring) calls this when it sees
/// `[plan-mode] approve plan_id=<ulid>` or
/// `[plan-mode] reject plan_id=<ulid> reason=<…>` on an inbound
/// message. Direct callable too — tests and CLI ops use it without
/// the parser layer.
pub struct PlanModeResolveTool;

impl PlanModeResolveTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "plan_mode_resolve".to_string(),
            description: "Operator-side resolver for a pending ExitPlanMode approval. Fires the matching waiter so the goal unlocks (approve) or surfaces the rejection reason to the model (reject).".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "Plan id from the ExitPlanMode response (UUID)."
                    },
                    "decision": {
                        "type": "string",
                        "enum": ["approve", "reject"],
                        "description": "Operator decision."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Rejection reason (required when decision=reject)."
                    }
                },
                "required": ["plan_id", "decision"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for PlanModeResolveTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let plan_id = args
            .get("plan_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("plan_mode_resolve requires `plan_id`"))?
            .to_string();
        let decision_str = args
            .get("decision")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("plan_mode_resolve requires `decision`"))?;
        let decision = match decision_str {
            "approve" => PlanApprovalDecision::Approve,
            "reject" => {
                let reason = args
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("plan_mode_resolve(decision=reject) requires `reason`")
                    })?;
                PlanApprovalDecision::Reject { reason }
            }
            other => {
                return Err(anyhow::anyhow!(
                    "plan_mode_resolve: unknown decision `{other}` (expected approve|reject)"
                ));
            }
        };
        let resolved = ctx.plan_approval_registry.resolve(&plan_id, decision);
        if !resolved {
            return Err(anyhow::anyhow!(
                "no pending plan_mode approval for plan_id={plan_id}"
            ));
        }
        Ok(json!({ "resolved": true, "plan_id": plan_id }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use std::sync::Arc;

    fn ctx_interactive() -> AgentContext {
        let cfg = AgentConfig {
            id: "a".into(),
            model: ModelConfig {
                provider: "x".into(),
                model: "y".into(),
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
        };
        AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
        .with_inbound_origin("whatsapp", "default", "+1234")
    }

    fn ctx_non_interactive() -> AgentContext {
        let mut c = ctx_interactive();
        c.inbound_origin = None;
        c
    }

    #[tokio::test]
    async fn enter_plan_mode_flips_state_on() {
        let ctx = ctx_interactive();
        let tool = EnterPlanModeTool;
        let out = tool
            .call(&ctx, json!({"reason": "explore auth flow"}))
            .await
            .unwrap();
        assert_eq!(out["entered_plan_mode"], true);
        assert_eq!(out["already_in_plan_mode"], false);
        assert!(ctx.plan_mode.read().await.is_on());
    }

    #[tokio::test]
    async fn enter_plan_mode_is_idempotent() {
        let ctx = ctx_interactive();
        let tool = EnterPlanModeTool;
        let _ = tool.call(&ctx, json!({})).await.unwrap();
        let snap1 = match &*ctx.plan_mode.read().await {
            PlanModeState::On { entered_at, .. } => *entered_at,
            _ => panic!("expected On"),
        };
        // Second call must not bump entered_at.
        let _ = tool
            .call(&ctx, json!({"reason": "different"}))
            .await
            .unwrap();
        let snap2 = match &*ctx.plan_mode.read().await {
            PlanModeState::On { entered_at, .. } => *entered_at,
            _ => panic!("expected On"),
        };
        assert_eq!(snap1, snap2, "entered_at should be sticky");
    }

    #[tokio::test]
    async fn enter_plan_mode_rejects_sub_agent() {
        let ctx = ctx_non_interactive();
        let tool = EnterPlanModeTool;
        let err = tool.call(&ctx, json!({})).await.unwrap_err().to_string();
        assert!(
            err.contains("unavailable") || err.contains("sub-agent"),
            "unexpected error: {err}"
        );
        assert!(ctx.plan_mode.read().await.is_off());
    }

    #[tokio::test]
    async fn exit_plan_mode_unlocks_state() {
        let ctx = ctx_interactive();
        EnterPlanModeTool.call(&ctx, json!({})).await.unwrap();
        let out = ExitPlanModeTool
            .call(
                &ctx,
                json!({"final_plan": "1. Read auth.rs\n2. Add OAuth handler"}),
            )
            .await
            .unwrap();
        assert_eq!(out["exited_plan_mode"], true);
        assert!(ctx.plan_mode.read().await.is_off());
    }

    #[tokio::test]
    async fn exit_plan_mode_rejects_oversize_plan() {
        let ctx = ctx_interactive();
        EnterPlanModeTool.call(&ctx, json!({})).await.unwrap();
        let big = "x".repeat(PLAN_MODE_MAX_PLAN_BYTES + 1);
        let err = ExitPlanModeTool
            .call(&ctx, json!({"final_plan": big}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("PlanTooLarge"), "got: {err}");
        // State must remain on after rejection.
        assert!(ctx.plan_mode.read().await.is_on());
    }

    #[tokio::test]
    async fn exit_plan_mode_rejects_outside_plan_mode() {
        let ctx = ctx_interactive();
        // Never entered plan mode.
        let err = ExitPlanModeTool
            .call(&ctx, json!({"final_plan": "trivial"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside plan mode"), "got: {err}");
    }

    #[tokio::test]
    async fn exit_plan_mode_requires_final_plan() {
        let ctx = ctx_interactive();
        EnterPlanModeTool.call(&ctx, json!({})).await.unwrap();
        let err = ExitPlanModeTool
            .call(&ctx, json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("final_plan"), "got: {err}");
    }

    fn ctx_with_approval(timeout_secs: u64) -> AgentContext {
        let cfg = AgentConfig {
            id: "a".into(),
            model: ModelConfig {
                provider: "x".into(),
                model: "y".into(),
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
            plan_mode: nexo_config::types::plan_mode::PlanModePolicy {
                enabled: true,
                auto_enter_on_destructive: false,
                default_active: None,
                approval_timeout_secs: timeout_secs,
                require_approval: true,
            },
            remote_triggers: Vec::new(),
        };
        AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
        .with_inbound_origin("whatsapp", "default", "+1234")
    }

    #[tokio::test]
    async fn exit_plan_mode_with_approval_then_approve_unlocks() {
        let ctx = ctx_with_approval(5);
        EnterPlanModeTool.call(&ctx, json!({})).await.unwrap();

        // Spawn the ExitPlanMode call so we can resolve while it
        // waits.
        let ctx_for_task = ctx.clone();
        let exit_handle = tokio::spawn(async move {
            ExitPlanModeTool
                .call(
                    &ctx_for_task,
                    json!({"final_plan": "1. read auth.rs\n2. patch"}),
                )
                .await
        });

        // Fish out the plan_id from the registry — exactly one
        // pending entry must exist after a brief yield.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let plan_id = {
            let reg = ctx.plan_approval_registry.clone();
            let mut found = None;
            for entry in reg.pending.iter() {
                found = Some(entry.key().clone());
                break;
            }
            found.expect("expected exactly one pending plan_id")
        };

        // Resolve via the operator tool.
        let resolve = PlanModeResolveTool
            .call(
                &ctx,
                json!({"plan_id": plan_id.clone(), "decision": "approve"}),
            )
            .await
            .unwrap();
        assert_eq!(resolve["resolved"], true);

        let exit_result = exit_handle.await.unwrap().unwrap();
        assert_eq!(exit_result["exited_plan_mode"], true);
        assert_eq!(exit_result["approval_required"], true);
        assert_eq!(exit_result["plan_id"], plan_id);
        assert!(ctx.plan_mode.read().await.is_off());
    }

    #[tokio::test]
    async fn exit_plan_mode_with_approval_then_reject_keeps_state_on() {
        let ctx = ctx_with_approval(5);
        EnterPlanModeTool.call(&ctx, json!({})).await.unwrap();

        let ctx_for_task = ctx.clone();
        let exit_handle = tokio::spawn(async move {
            ExitPlanModeTool
                .call(&ctx_for_task, json!({"final_plan": "trivial plan"}))
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let plan_id = {
            let reg = ctx.plan_approval_registry.clone();
            let mut found = None;
            for entry in reg.pending.iter() {
                found = Some(entry.key().clone());
                break;
            }
            found.unwrap()
        };

        PlanModeResolveTool
            .call(
                &ctx,
                json!({
                    "plan_id": plan_id,
                    "decision": "reject",
                    "reason": "scope too small — research more"
                }),
            )
            .await
            .unwrap();

        let err = exit_handle.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("Plan rejected by operator"), "got: {err}");
        assert!(err.contains("scope too small"), "got: {err}");
        // Crucially: plan-mode stays on so the model must revise.
        assert!(ctx.plan_mode.read().await.is_on());
    }

    #[tokio::test]
    async fn exit_plan_mode_with_approval_times_out() {
        let ctx = ctx_with_approval(1); // 1s timeout
        EnterPlanModeTool.call(&ctx, json!({})).await.unwrap();

        let err = ExitPlanModeTool
            .call(&ctx, json!({"final_plan": "trivial"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("ApprovalTimeout"), "got: {err}");
        assert!(ctx.plan_mode.read().await.is_on());
    }

    #[tokio::test]
    async fn plan_mode_resolve_unknown_id_errors() {
        let ctx = ctx_interactive();
        let err = PlanModeResolveTool
            .call(
                &ctx,
                json!({"plan_id": "deadbeef-no-such-id", "decision": "approve"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no pending"), "got: {err}");
    }

    #[tokio::test]
    async fn plan_mode_resolve_reject_requires_reason() {
        let ctx = ctx_interactive();
        let err = PlanModeResolveTool
            .call(&ctx, json!({"plan_id": "x", "decision": "reject"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires `reason`"), "got: {err}");
    }
}
