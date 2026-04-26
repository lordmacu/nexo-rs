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
    program_phase_dispatch, resume_agent, update_budget, AgentHooksListInput,
    AgentLogsTailInput, AgentStatusInput, CancelAgentInput, HookRegistry, ListAgentsInput,
    PauseAgentInput, ProgramPhaseInput, UpdateBudgetInput,
};
use nexo_driver_claude::{DispatcherIdentity, OriginChannel};
use nexo_driver_loop::DriverOrchestrator;
use nexo_llm::ToolDef;
use nexo_project_tracker::tracker::ProjectTracker;
use serde_json::{json, Value};

use super::context::AgentContext;
use super::tool_registry::{ToolHandler, ToolRegistry};

/// Bundle of runtime services the dispatch tool handlers consult
/// on every invocation. Constructed once at boot, shared via
/// `Arc` through `AgentContext::dispatch`.
pub struct DispatchToolContext {
    pub tracker: Arc<dyn ProjectTracker>,
    pub orchestrator: Arc<DriverOrchestrator>,
    pub registry: Arc<AgentRegistry>,
    pub hooks: Arc<HookRegistry>,
    pub log_buffer: Arc<LogBuffer>,
    /// Default cap snapshot the gate consumes. The runtime can
    /// refresh `global_running` per call from the live registry;
    /// the rest stays config-driven.
    pub default_caps: CapSnapshot,
    pub require_trusted: bool,
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
        // Effective binding carries the (plugin, instance) tuple in
        // future patches — for now derive from the agent id.
        let _ = ctx;
        None
    }

    fn dispatch_policy(&self, ctx: &AgentContext) -> nexo_config::DispatchPolicy {
        ctx.effective_policy().dispatch_policy.clone()
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
        let out = program_phase_dispatch(
            input,
            dispatch.tracker.as_ref(),
            dispatch.orchestrator.clone(),
            dispatch.registry.clone(),
            &policy,
            dispatch.require_trusted,
            true, // sender_trusted: chat senders pre-validated by pairing gate
            dispatch.dispatcher_for(ctx),
            dispatch.origin_for(ctx),
            dispatch.caps_snapshot(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("program_phase: {e}"))?;
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
        let out = cancel_agent(input, dispatch.orchestrator.clone(), dispatch.registry.clone())
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
        let out = pause_agent(input, dispatch.orchestrator.clone(), dispatch.registry.clone())
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
        let out = resume_agent(input, dispatch.orchestrator.clone(), dispatch.registry.clone())
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
        let out = update_budget(input, dispatch.registry.clone())
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
            "agent_hooks_list",
            "Hooks attached to the goal (notify_origin, dispatch_phase, etc.).",
            obj_schema(&["goal_id"], json!({ "goal_id": { "type": "string" } })),
        ),
        AgentHooksListHandler,
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
        let err = h.call(&ctx, json!({ "phase_id": "67.10" })).await.unwrap_err();
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
