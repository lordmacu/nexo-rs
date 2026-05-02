use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nexo_llm::ToolDef;
use nexo_memory::LongTermMemory;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
pub struct HeartbeatTool {
    memory: Arc<LongTermMemory>,
}
impl HeartbeatTool {
    pub fn new(memory: Arc<LongTermMemory>) -> Self {
        Self { memory }
    }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "schedule_reminder".to_string(),
            description: "Schedule a reminder message for later in the current conversation. Use this when the user asks to be reminded at a time or after a delay.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "at": {
                        "type": "string",
                        "description": "When the reminder is due. Accepts RFC3339 timestamps like 2026-04-22T18:30:00Z or relative durations like 10m, 2h, 1d."
                    },
                    "message": {
                        "type": "string",
                        "description": "Reminder text to send when due."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional UUID override for reminder correlation. Defaults to current turn session when available; otherwise auto-generated."
                    },
                    "source_plugin": {
                        "type": "string",
                        "description": "Optional outbound channel key (e.g. 'telegram' or 'telegram.sales'). Required when no inbound context is available (common in MCP calls)."
                    },
                    "source_instance": {
                        "type": "string",
                        "description": "Optional outbound channel instance suffix (e.g. 'sales'). Used only when `source_plugin` has no instance suffix."
                    },
                    "recipient": {
                        "type": "string",
                        "description": "Optional recipient id. Required when no inbound context is available (common in MCP calls)."
                    }
                },
                "required": ["at", "message"]
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for HeartbeatTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let at = args["at"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("schedule_reminder requires `at`"))?;
        let message = args["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("schedule_reminder requires `message`"))?;
        let session_id = resolve_session_id(ctx, &args)?;
        let source_plugin = resolve_source_plugin(ctx, &args)?;
        let recipient = resolve_recipient(ctx, &args)?;
        let due_at = parse_due_at(at)?;
        let id = self
            .memory
            .schedule_reminder(
                &ctx.agent_id,
                session_id,
                &source_plugin,
                &recipient,
                message,
                due_at,
            )
            .await?;
        Ok(json!({
            "ok": true,
            "id": id.to_string(),
            "due_at": due_at.to_rfc3339(),
            "session_id": session_id.to_string(),
            "source_plugin": source_plugin,
            "recipient": recipient,
        }))
    }
}

fn optional_nonempty_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn resolve_session_id(ctx: &AgentContext, args: &Value) -> anyhow::Result<Uuid> {
    if let Some(raw) = optional_nonempty_str(args, "session_id") {
        return Uuid::parse_str(&raw).map_err(|_| anyhow::anyhow!("invalid session_id: {raw}"));
    }
    if let Some(id) = ctx.session_id {
        return Ok(id);
    }
    Ok(Uuid::new_v4())
}

fn resolve_source_plugin(ctx: &AgentContext, args: &Value) -> anyhow::Result<String> {
    if let Some(plugin) = optional_nonempty_str(args, "source_plugin") {
        let instance = optional_nonempty_str(args, "source_instance");
        return Ok(with_instance_suffix(&plugin, instance.as_deref()));
    }
    if let Some((plugin, instance, _sender)) = ctx.inbound_origin.as_ref() {
        return Ok(with_instance_suffix(plugin, Some(instance.as_str())));
    }
    Err(anyhow::anyhow!(
        "schedule_reminder requires `source_plugin` when no inbound origin is available"
    ))
}

fn resolve_recipient(ctx: &AgentContext, args: &Value) -> anyhow::Result<String> {
    if let Some(recipient) =
        optional_nonempty_str(args, "recipient").or_else(|| optional_nonempty_str(args, "to"))
    {
        return Ok(recipient);
    }
    if let Some((_plugin, _instance, sender)) = ctx.inbound_origin.as_ref() {
        let sender = sender.trim();
        if !sender.is_empty() {
            return Ok(sender.to_string());
        }
    }
    Err(anyhow::anyhow!(
        "schedule_reminder requires `recipient` when no inbound origin is available"
    ))
}

fn with_instance_suffix(plugin: &str, instance: Option<&str>) -> String {
    let plugin = plugin.trim();
    let Some(instance) = instance.map(str::trim).filter(|s| !s.is_empty()) else {
        return plugin.to_string();
    };
    if plugin.contains('.') {
        plugin.to_string()
    } else {
        format!("{plugin}.{instance}")
    }
}

fn parse_due_at(input: &str) -> anyhow::Result<DateTime<Utc>> {
    if let Ok(ts) = DateTime::parse_from_rfc3339(input) {
        return Ok(ts.with_timezone(&Utc));
    }
    let duration = humantime::parse_duration(input)
        .map_err(|e| anyhow::anyhow!("invalid reminder time `{input}`: {e}"))?;
    let due_at = chrono::Duration::from_std(duration)
        .map_err(|_| anyhow::anyhow!("reminder duration out of range: {input}"))?;
    Ok(Utc::now() + due_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::context::AgentContext;
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use std::sync::Arc;
    use std::time::Duration;

    fn test_ctx() -> AgentContext {
        let cfg = Arc::new(AgentConfig {
            id: "heartbeat-test".into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m0".into(),
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
            tenant_id: None,
            extensions_config: std::collections::BTreeMap::new(),
        });
        AgentContext::new(
            "heartbeat-test",
            cfg,
            AnyBroker::local(),
            Arc::new(SessionManager::new(Duration::from_secs(60), 4)),
        )
    }

    #[tokio::test]
    async fn falls_back_to_context_origin_and_session() {
        let memory = Arc::new(LongTermMemory::open(":memory:").await.expect("open memory"));
        let sid = Uuid::new_v4();
        let ctx = test_ctx()
            .with_session_id(sid)
            .with_inbound_origin("telegram", "sales", "u-42");
        let tool = HeartbeatTool::new(Arc::clone(&memory));

        let out = tool
            .call(
                &ctx,
                json!({
                    "at": "30m",
                    "message": "ping"
                }),
            )
            .await
            .expect("call");
        assert_eq!(out["source_plugin"], "telegram.sales");
        assert_eq!(out["recipient"], "u-42");
        assert_eq!(out["session_id"], sid.to_string());

        let due = memory
            .list_due_reminders(
                "heartbeat-test",
                Utc::now() + chrono::Duration::hours(1),
                10,
            )
            .await
            .expect("list");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].plugin, "telegram.sales");
        assert_eq!(due[0].recipient, "u-42");
        assert_eq!(due[0].session_id, sid);
    }

    #[tokio::test]
    async fn accepts_explicit_route_fields_without_context() {
        let memory = Arc::new(LongTermMemory::open(":memory:").await.expect("open memory"));
        let sid = Uuid::new_v4();
        let ctx = test_ctx();
        let tool = HeartbeatTool::new(Arc::clone(&memory));

        let out = tool
            .call(
                &ctx,
                json!({
                    "at": "10m",
                    "message": "ping",
                    "session_id": sid.to_string(),
                    "source_plugin": "whatsapp",
                    "source_instance": "ops",
                    "recipient": "+15550001"
                }),
            )
            .await
            .expect("call");
        assert_eq!(out["source_plugin"], "whatsapp.ops");
        assert_eq!(out["recipient"], "+15550001");
        assert_eq!(out["session_id"], sid.to_string());

        let due = memory
            .list_due_reminders(
                "heartbeat-test",
                Utc::now() + chrono::Duration::hours(1),
                10,
            )
            .await
            .expect("list");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].plugin, "whatsapp.ops");
        assert_eq!(due[0].recipient, "+15550001");
        assert_eq!(due[0].session_id, sid);
    }

    #[tokio::test]
    async fn missing_route_fields_error_when_context_absent() {
        let memory = Arc::new(LongTermMemory::open(":memory:").await.expect("open memory"));
        let ctx = test_ctx();
        let tool = HeartbeatTool::new(memory);

        let err = tool
            .call(
                &ctx,
                json!({
                    "at": "5m",
                    "message": "ping"
                }),
            )
            .await
            .expect_err("expected missing route args");
        let msg = err.to_string();
        assert!(
            msg.contains("source_plugin") || msg.contains("recipient"),
            "unexpected error: {msg}"
        );
    }
}
