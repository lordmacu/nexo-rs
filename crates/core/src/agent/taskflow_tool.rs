use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use chrono::Utc;
use nexo_llm::ToolDef;
use nexo_taskflow::{CreateManagedInput, Flow, FlowError, FlowManager, FlowStatus, WaitCondition};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

/// Guardrails applied to tool-driven `wait` requests. The `FlowManager` is
/// transport-neutral; these limits are LLM-UX concerns kept on the tool side.
#[derive(Debug, Clone)]
pub struct TaskFlowToolGuardrails {
    /// Maximum future deadline allowed for `WaitCondition::Timer`.
    pub timer_max_horizon: chrono::Duration,
}

impl Default for TaskFlowToolGuardrails {
    fn default() -> Self {
        Self {
            timer_max_horizon: chrono::Duration::days(30),
        }
    }
}

/// LLM-facing tool that lets an agent start and drive durable multi-step
/// flows. Revision handling is hidden from the model — every mutating call
/// refetches the latest record internally.
pub struct TaskFlowTool {
    manager: FlowManager,
    guardrails: TaskFlowToolGuardrails,
}
impl TaskFlowTool {
    pub fn new(manager: FlowManager) -> Self {
        Self {
            manager,
            guardrails: TaskFlowToolGuardrails::default(),
        }
    }
    pub fn with_guardrails(mut self, g: TaskFlowToolGuardrails) -> Self {
        self.guardrails = g;
        self
    }
    pub fn into_arc(self) -> Arc<dyn ToolHandler> {
        Arc::new(self)
    }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "taskflow".to_string(),
            description: "Create and drive durable multi-step flows that survive process restart. \
                Actions: start (create a new flow), status (inspect one flow), advance (update state / move to next step), \
                wait (pause until timer/external_event/manual signal), finish (mark completed with optional final state), \
                fail (mark failed with reason), cancel (stop a flow), list_mine (list this session's flows). \
                Flow identity is a UUID.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["start", "status", "advance", "wait", "finish", "fail", "cancel", "list_mine"]
                    },
                    "flow_id": {
                        "type": "string",
                        "description": "UUID of the flow. Required for status/advance/wait/finish/fail/cancel."
                    },
                    "controller_id": {
                        "type": "string",
                        "description": "Logical identifier of the caller (e.g. 'kate/inbox-triage'). Required for start."
                    },
                    "goal": {
                        "type": "string",
                        "description": "Short human-readable goal of the flow. Required for start."
                    },
                    "current_step": {
                        "type": "string",
                        "description": "Step name for start or advance. Optional on advance (keeps current)."
                    },
                    "state": {
                        "type": "object",
                        "description": "Initial state_json on start."
                    },
                    "patch": {
                        "type": "object",
                        "description": "State patch for advance. Merged shallowly into state_json."
                    },
                    "wait_condition": {
                        "type": "object",
                        "description": "WaitCondition object. Required for wait. Shape: {kind:'timer', at:'<RFC3339>'} OR {kind:'external_event', topic, correlation_id} OR {kind:'manual'}."
                    },
                    "final_state": {
                        "type": "object",
                        "description": "Optional final state patch merged before transition to Finished."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Failure reason. Required for fail."
                    }
                },
                "required": ["action"]
            }),
        }
    }
}

fn validate_wait_condition(cond: &WaitCondition, g: &TaskFlowToolGuardrails) -> anyhow::Result<()> {
    match cond {
        WaitCondition::Timer { at } => {
            let now = Utc::now();
            if *at <= now {
                anyhow::bail!("timer.at must be in the future");
            }
            if *at - now > g.timer_max_horizon {
                anyhow::bail!(
                    "timer.at exceeds max horizon ({} days)",
                    g.timer_max_horizon.num_days()
                );
            }
        }
        WaitCondition::ExternalEvent {
            topic,
            correlation_id,
        } => {
            if topic.trim().is_empty() {
                anyhow::bail!("external_event.topic cannot be empty");
            }
            if correlation_id.trim().is_empty() {
                anyhow::bail!("external_event.correlation_id cannot be empty");
            }
        }
        WaitCondition::Manual => {}
    }
    Ok(())
}
#[async_trait]
impl ToolHandler for TaskFlowTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let action = args["action"].as_str().unwrap_or("").trim();
        let owner_key = owner_session_key(ctx)?;
        let requester = ctx
            .session_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        match action {
            "start" => {
                let controller_id = required_string(&args, "controller_id")?;
                let goal = required_string(&args, "goal")?;
                let current_step =
                    optional_string(&args, "current_step").unwrap_or_else(|| "init".to_string());
                let state_json = args.get("state").cloned().unwrap_or_else(|| json!({}));
                let flow = self
                    .manager
                    .create_managed(CreateManagedInput {
                        controller_id,
                        goal,
                        owner_session_key: owner_key,
                        requester_origin: requester,
                        current_step,
                        state_json,
                    })
                    .await?;
                // Auto-start: move Created → Running so the LLM sees a flow
                // that is actively progressing. The model can `set_waiting` via
                // advance+flow.wait_json in a future iteration, or the host
                // can call `manager.set_waiting` directly.
                let flow = self.manager.start_running(flow.id).await?;
                Ok(render_flow(&flow))
            }
            "status" => {
                let id = required_uuid(&args, "flow_id")?;
                let flow = self.manager.get(id).await?;
                match flow {
                    Some(f) => {
                        require_owner(&f, &owner_key)?;
                        Ok(render_flow(&f))
                    }
                    None => {
                        Ok(json!({ "ok": false, "error": "not_found", "flow_id": id.to_string() }))
                    }
                }
            }
            "advance" => {
                let id = required_uuid(&args, "flow_id")?;
                // Verify ownership first for a clean error.
                if let Some(f) = self.manager.get(id).await? {
                    require_owner(&f, &owner_key)?;
                } else {
                    return Ok(
                        json!({ "ok": false, "error": "not_found", "flow_id": id.to_string() }),
                    );
                }
                let patch = args.get("patch").cloned().unwrap_or_else(|| json!({}));
                let next_step = optional_string(&args, "current_step");
                let flow = self.manager.update_state(id, patch, next_step).await?;
                Ok(render_flow(&flow))
            }
            "cancel" => {
                let id = required_uuid(&args, "flow_id")?;
                if let Some(f) = self.manager.get(id).await? {
                    require_owner(&f, &owner_key)?;
                } else {
                    return Ok(
                        json!({ "ok": false, "error": "not_found", "flow_id": id.to_string() }),
                    );
                }
                let flow = self.manager.cancel(id).await?;
                Ok(render_flow(&flow))
            }
            "wait" => {
                let id = required_uuid(&args, "flow_id")?;
                if let Some(f) = self.manager.get(id).await? {
                    require_owner(&f, &owner_key)?;
                } else {
                    return Ok(
                        json!({ "ok": false, "error": "not_found", "flow_id": id.to_string() }),
                    );
                }
                let cond_value = args
                    .get("wait_condition")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing `wait_condition`"))?;
                let cond = WaitCondition::from_value(&cond_value).ok_or_else(|| {
                    anyhow::anyhow!(
                        "invalid wait_condition shape; expected {{kind:'timer'|'external_event'|'manual', ...}}"
                    )
                })?;
                validate_wait_condition(&cond, &self.guardrails)?;
                let flow = self.manager.set_waiting(id, cond.into_value()).await?;
                Ok(render_flow(&flow))
            }
            "finish" => {
                let id = required_uuid(&args, "flow_id")?;
                if let Some(f) = self.manager.get(id).await? {
                    require_owner(&f, &owner_key)?;
                } else {
                    return Ok(
                        json!({ "ok": false, "error": "not_found", "flow_id": id.to_string() }),
                    );
                }
                let final_state = args.get("final_state").cloned();
                let flow = self.manager.finish(id, final_state).await?;
                Ok(render_flow(&flow))
            }
            "fail" => {
                let id = required_uuid(&args, "flow_id")?;
                if let Some(f) = self.manager.get(id).await? {
                    require_owner(&f, &owner_key)?;
                } else {
                    return Ok(
                        json!({ "ok": false, "error": "not_found", "flow_id": id.to_string() }),
                    );
                }
                let reason = required_string(&args, "reason")?;
                let flow = self.manager.fail(id, reason).await?;
                Ok(render_flow(&flow))
            }
            "list_mine" => {
                let flows = self.manager.list_by_owner(&owner_key).await?;
                let items: Vec<Value> = flows.iter().map(render_flow).collect();
                Ok(json!({
                    "ok": true,
                    "count": items.len(),
                    "flows": items,
                }))
            }
            other => Err(anyhow::anyhow!(
                "unknown action `{other}`; expected start|status|advance|wait|finish|fail|cancel|list_mine"
            )),
        }
    }
}
fn owner_session_key(ctx: &AgentContext) -> anyhow::Result<String> {
    let Some(session_id) = ctx.session_id else {
        return Err(anyhow::anyhow!(
            "taskflow tool requires a session context; call was made without session_id"
        ));
    };
    Ok(format!("agent:{}:session:{}", ctx.agent_id, session_id))
}
fn require_owner(flow: &Flow, expected: &str) -> anyhow::Result<()> {
    if flow.owner_session_key != expected {
        return Err(anyhow::anyhow!(
            "flow {} belongs to a different session; refusing cross-session access",
            flow.id
        ));
    }
    Ok(())
}
fn required_string(args: &Value, key: &str) -> anyhow::Result<String> {
    let s = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing or invalid `{key}`"))?
        .trim()
        .to_string();
    if s.is_empty() {
        anyhow::bail!("`{key}` cannot be empty");
    }
    Ok(s)
}
fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
fn required_uuid(args: &Value, key: &str) -> anyhow::Result<Uuid> {
    let s = required_string(args, key)?;
    Uuid::parse_str(&s).map_err(|e| anyhow::anyhow!("`{key}` is not a valid UUID: {e}"))
}
fn render_flow(flow: &Flow) -> Value {
    json!({
        "ok": true,
        "flow": {
            "id": flow.id.to_string(),
            "controller_id": flow.controller_id,
            "goal": flow.goal,
            "current_step": flow.current_step,
            "status": flow.status.as_str(),
            "cancel_requested": flow.cancel_requested,
            "state": flow.state_json,
            "wait": flow.wait_json,
            "revision": flow.revision,
            "created_at": flow.created_at.to_rfc3339(),
            "updated_at": flow.updated_at.to_rfc3339(),
        }
    })
}
// Re-export helper used by tests.
#[doc(hidden)]
pub fn __for_tests_status_is(v: &Value, expected: FlowStatus) -> bool {
    v.pointer("/flow/status")
        .and_then(|s| s.as_str())
        .map(|s| s == expected.as_str())
        .unwrap_or(false)
}
#[allow(dead_code)]
fn _suppress_flowerror_use(e: FlowError) -> FlowError {
    e
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use nexo_broker::{AnyBroker, BrokerHandle};
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig,
    };
    use nexo_taskflow::SqliteFlowStore;
    use std::sync::Arc as StdArc;
    use std::time::Duration;
    async fn ctx_with_session() -> (AgentContext, Uuid) {
        let broker = AnyBroker::local();
        let _ = broker.subscribe("plugin.outbound.whatsapp").await;
        let cfg = StdArc::new(AgentConfig {
            id: "kate".into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m1".into(),
            },
            plugins: vec![],
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: vec![],
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: Default::default(),
            workspace_git: Default::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            outbound_allowlist: Default::default(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
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
        });
        let sessions = StdArc::new(SessionManager::new(Duration::from_secs(60), 20));
        let sid = Uuid::new_v4();
        let ctx = AgentContext::new("kate", cfg, broker, sessions).with_session_id(sid);
        (ctx, sid)
    }
    async fn tool() -> TaskFlowTool {
        let store = Arc::new(SqliteFlowStore::open(":memory:").await.unwrap());
        TaskFlowTool::new(FlowManager::new(store))
    }
    #[tokio::test]
    async fn start_creates_and_runs_flow() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let out = tool
            .call(
                &ctx,
                json!({
                    "action": "start",
                    "controller_id": "kate/demo",
                    "goal": "test flow",
                    "state": {"step_count": 0}
                }),
            )
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert!(__for_tests_status_is(&out, FlowStatus::Running));
        assert_eq!(out["flow"]["controller_id"], "kate/demo");
        assert_eq!(out["flow"]["state"]["step_count"], 0);
    }
    #[tokio::test]
    async fn status_returns_current_state() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let created = tool
            .call(
                &ctx,
                json!({ "action": "start", "controller_id": "c", "goal": "g" }),
            )
            .await
            .unwrap();
        let id = created["flow"]["id"].as_str().unwrap().to_string();
        let status = tool
            .call(&ctx, json!({ "action": "status", "flow_id": id }))
            .await
            .unwrap();
        assert_eq!(status["flow"]["id"], id);
        assert!(__for_tests_status_is(&status, FlowStatus::Running));
    }
    #[tokio::test]
    async fn advance_merges_patch_and_updates_step() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let created = tool
            .call(
                &ctx,
                json!({
                    "action": "start", "controller_id": "c", "goal": "g",
                    "state": { "a": 1 }
                }),
            )
            .await
            .unwrap();
        let id = created["flow"]["id"].as_str().unwrap().to_string();
        let advanced = tool
            .call(
                &ctx,
                json!({
                    "action": "advance",
                    "flow_id": id,
                    "patch": { "b": 2 },
                    "current_step": "fetch"
                }),
            )
            .await
            .unwrap();
        assert_eq!(advanced["flow"]["state"]["a"], 1);
        assert_eq!(advanced["flow"]["state"]["b"], 2);
        assert_eq!(advanced["flow"]["current_step"], "fetch");
    }
    #[tokio::test]
    async fn cancel_flips_to_cancelled() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let created = tool
            .call(
                &ctx,
                json!({ "action": "start", "controller_id": "c", "goal": "g" }),
            )
            .await
            .unwrap();
        let id = created["flow"]["id"].as_str().unwrap().to_string();
        let cancelled = tool
            .call(&ctx, json!({ "action": "cancel", "flow_id": id }))
            .await
            .unwrap();
        assert!(__for_tests_status_is(&cancelled, FlowStatus::Cancelled));
    }
    #[tokio::test]
    async fn list_mine_returns_only_this_session_flows() {
        let tool = tool().await;
        let (ctx_a, _) = ctx_with_session().await;
        let (ctx_b, _) = ctx_with_session().await;
        tool.call(
            &ctx_a,
            json!({ "action": "start", "controller_id": "c1", "goal": "A" }),
        )
        .await
        .unwrap();
        tool.call(
            &ctx_a,
            json!({ "action": "start", "controller_id": "c2", "goal": "A2" }),
        )
        .await
        .unwrap();
        tool.call(
            &ctx_b,
            json!({ "action": "start", "controller_id": "c3", "goal": "B" }),
        )
        .await
        .unwrap();
        let list_a = tool
            .call(&ctx_a, json!({ "action": "list_mine" }))
            .await
            .unwrap();
        let list_b = tool
            .call(&ctx_b, json!({ "action": "list_mine" }))
            .await
            .unwrap();
        assert_eq!(list_a["count"], 2);
        assert_eq!(list_b["count"], 1);
    }
    #[tokio::test]
    async fn cross_session_access_is_rejected() {
        let tool = tool().await;
        let (ctx_a, _) = ctx_with_session().await;
        let (ctx_b, _) = ctx_with_session().await;
        let created = tool
            .call(
                &ctx_a,
                json!({ "action": "start", "controller_id": "c", "goal": "g" }),
            )
            .await
            .unwrap();
        let id = created["flow"]["id"].as_str().unwrap().to_string();
        let err = tool
            .call(&ctx_b, json!({ "action": "status", "flow_id": id }))
            .await
            .expect_err("cross-session must fail");
        let msg = err.to_string();
        assert!(msg.contains("different session"), "unexpected msg: {msg}");
    }
    #[tokio::test]
    async fn missing_session_id_errors() {
        let tool = tool().await;
        let broker = AnyBroker::local();
        let _ = broker.subscribe("_").await;
        let cfg = StdArc::new(AgentConfig {
            id: "kate".into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m1".into(),
            },
            plugins: vec![],
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: vec![],
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: Default::default(),
            workspace_git: Default::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            outbound_allowlist: Default::default(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
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
        });
        let sessions = StdArc::new(SessionManager::new(Duration::from_secs(60), 20));
        let ctx = AgentContext::new("kate", cfg, broker, sessions);
        let err = tool
            .call(&ctx, json!({ "action": "list_mine" }))
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("session_id"));
    }
    #[tokio::test]
    async fn unknown_flow_id_returns_not_found_ok_false() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let out = tool
            .call(
                &ctx,
                json!({ "action": "status", "flow_id": Uuid::new_v4().to_string() }),
            )
            .await
            .unwrap();
        assert_eq!(out["ok"], false);
        assert_eq!(out["error"], "not_found");
    }

    async fn started_flow_id(tool: &TaskFlowTool, ctx: &AgentContext) -> String {
        let created = tool
            .call(
                ctx,
                json!({ "action": "start", "controller_id": "c", "goal": "g" }),
            )
            .await
            .unwrap();
        created["flow"]["id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn wait_with_past_timer_errors() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let id = started_flow_id(&tool, &ctx).await;
        let past = (Utc::now() - chrono::Duration::seconds(60)).to_rfc3339();
        let err = tool
            .call(
                &ctx,
                json!({
                    "action": "wait",
                    "flow_id": id,
                    "wait_condition": {"kind": "timer", "at": past}
                }),
            )
            .await
            .expect_err("past timer must fail");
        assert!(err.to_string().contains("must be in the future"));
    }

    #[tokio::test]
    async fn wait_with_future_timer_succeeds_and_sets_status_waiting() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let id = started_flow_id(&tool, &ctx).await;
        let future = (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
        let out = tool
            .call(
                &ctx,
                json!({
                    "action": "wait",
                    "flow_id": id,
                    "wait_condition": {"kind": "timer", "at": future}
                }),
            )
            .await
            .unwrap();
        assert!(__for_tests_status_is(&out, FlowStatus::Waiting));
        assert_eq!(out["flow"]["wait"]["kind"], "timer");
    }

    #[tokio::test]
    async fn wait_with_external_event_validates_topic_nonempty() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let id = started_flow_id(&tool, &ctx).await;
        let err = tool
            .call(
                &ctx,
                json!({
                    "action": "wait",
                    "flow_id": id,
                    "wait_condition": {
                        "kind": "external_event",
                        "topic": "   ",
                        "correlation_id": "c1"
                    }
                }),
            )
            .await
            .expect_err("empty topic must fail");
        assert!(err.to_string().contains("topic cannot be empty"));
    }

    #[tokio::test]
    async fn wait_horizon_exceeded_errors() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let id = started_flow_id(&tool, &ctx).await;
        let too_far = (Utc::now() + chrono::Duration::days(60)).to_rfc3339();
        let err = tool
            .call(
                &ctx,
                json!({
                    "action": "wait",
                    "flow_id": id,
                    "wait_condition": {"kind": "timer", "at": too_far}
                }),
            )
            .await
            .expect_err("horizon must fail");
        assert!(err.to_string().contains("max horizon"));
    }

    #[tokio::test]
    async fn finish_returns_finished_status() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let id = started_flow_id(&tool, &ctx).await;
        let out = tool
            .call(
                &ctx,
                json!({
                    "action": "finish",
                    "flow_id": id,
                    "final_state": {"result": "ok"}
                }),
            )
            .await
            .unwrap();
        assert!(__for_tests_status_is(&out, FlowStatus::Finished));
        assert_eq!(out["flow"]["state"]["result"], "ok");
    }

    #[tokio::test]
    async fn fail_with_reason_records_failure() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let id = started_flow_id(&tool, &ctx).await;
        let out = tool
            .call(
                &ctx,
                json!({
                    "action": "fail",
                    "flow_id": id,
                    "reason": "downstream-error"
                }),
            )
            .await
            .unwrap();
        assert!(__for_tests_status_is(&out, FlowStatus::Failed));
        assert_eq!(
            out["flow"]["state"]["failure"]["reason"],
            "downstream-error"
        );
    }

    #[tokio::test]
    async fn fail_without_reason_errors() {
        let tool = tool().await;
        let (ctx, _) = ctx_with_session().await;
        let id = started_flow_id(&tool, &ctx).await;
        let err = tool
            .call(&ctx, json!({"action": "fail", "flow_id": id}))
            .await
            .expect_err("missing reason must fail");
        assert!(err.to_string().contains("reason"));
    }

    #[tokio::test]
    async fn wait_cross_session_rejected() {
        let tool = tool().await;
        let (ctx_a, _) = ctx_with_session().await;
        let (ctx_b, _) = ctx_with_session().await;
        let id = started_flow_id(&tool, &ctx_a).await;
        let future = (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
        let err = tool
            .call(
                &ctx_b,
                json!({
                    "action": "wait",
                    "flow_id": id,
                    "wait_condition": {"kind": "timer", "at": future}
                }),
            )
            .await
            .expect_err("cross-session wait must fail");
        assert!(err.to_string().contains("different session"));
    }
}
