//! Phase 79.7 — `cron_create` / `cron_list` / `cron_delete` LLM
//! tools that read/write the [`CronStore`].
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/ScheduleCronTool/CronCreateTool.ts:1-157`
//!     (5-field cron schema, recurring + durable flags, 50-entry
//!     cap).
//!   * `claude-code-leak/src/tools/ScheduleCronTool/CronListTool.ts`
//!     and `CronDeleteTool.ts` (sibling tools — same store).
//!
//! Reference (secondary):
//!   * OpenClaw `research/src/cron/schedule.ts` —
//!     `croner` + cache. Cron expression semantics are
//!     compatible.
//!
//! MVP caveat: these tools only mutate the SQLite table. The
//! tokio runtime that polls `due_at` and fires LLM turns is
//! deferred — it lands as a follow-up that wires into the
//! Phase 20 `agent_turn` poller.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use crate::cron_schedule::{
    build_new_entry, next_fire_after, CronStore, CronStoreError,
    MAX_CRON_ENTRIES_PER_BINDING,
};
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use std::sync::Arc;

/// `cron_create` — schedule a recurring (or one-shot) prompt.
pub struct CronCreateTool {
    store: Arc<dyn CronStore>,
}

impl CronCreateTool {
    pub fn new(store: Arc<dyn CronStore>) -> Self {
        Self { store }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "cron_create".to_string(),
            description: format!(
                "Schedule a recurring or one-shot prompt to fire on a cron schedule. The runtime persists the entry to SQLite and survives daemon restarts. Cap: {} entries per binding. Minimum interval: 60 seconds.\n\nMVP caveat: entries are persisted but the runtime task that fires due entries is not yet wired (lands as a follow-up). Useful today for testing schedule shapes + populating the durable table.",
                MAX_CRON_ENTRIES_PER_BINDING
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "cron": {
                        "type": "string",
                        "description": "Standard 5-field cron expression in UTC: \"M H DoM Mon DoW\". Examples: \"*/5 * * * *\" (every 5 minutes), \"0 9 * * *\" (daily 9am UTC), \"30 14 28 2 *\" (Feb 28 14:30 UTC, runs once if recurring=false)."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Prompt to enqueue at each fire time."
                    },
                    "channel": {
                        "type": "string",
                        "description": "Optional channel hint (e.g. 'whatsapp:default'). Inherits the binding's primary channel when omitted."
                    },
                    "recurring": {
                        "type": "boolean",
                        "description": "true (default) = fire on every cron match until deleted. false = fire once at the next match, then auto-delete (use for 'remind me at X')."
                    }
                },
                "required": ["cron", "prompt"]
            }),
        }
    }
}

fn binding_id_from_ctx(ctx: &AgentContext) -> String {
    ctx.inbound_origin
        .as_ref()
        .map(|(plugin, instance, _sender)| format!("{plugin}:{instance}"))
        .unwrap_or_else(|| ctx.agent_id.clone())
}

#[async_trait]
impl ToolHandler for CronCreateTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let cron = args
            .get("cron")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("cron_create requires `cron` (string)"))?
            .trim()
            .to_string();
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("cron_create requires `prompt` (string)"))?
            .to_string();
        let channel = args
            .get("channel")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let recurring = args
            .get("recurring")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let binding_id = binding_id_from_ctx(ctx);
        let entry = build_new_entry(
            &self.store,
            &binding_id,
            &cron,
            &prompt,
            channel.as_deref(),
            recurring,
        )
        .await
        .map_err(map_err)?;
        let id = entry.id.clone();
        let next_fire_at = entry.next_fire_at;
        self.store.insert(&entry).await.map_err(map_err)?;
        Ok(json!({
            "ok": true,
            "id": id,
            "binding_id": binding_id,
            "cron": cron,
            "recurring": recurring,
            "next_fire_at": next_fire_at,
            "instructions": "Entry persisted. The runtime fires it on schedule (firing wired in a follow-up). Use cron_list to inspect, cron_delete to cancel."
        }))
    }
}

/// `cron_list` — return the binding's scheduled entries.
pub struct CronListTool {
    store: Arc<dyn CronStore>,
}

impl CronListTool {
    pub fn new(store: Arc<dyn CronStore>) -> Self {
        Self { store }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "cron_list".to_string(),
            description: "List the scheduled cron entries for the current binding. Read-only.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for CronListTool {
    async fn call(&self, ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        let binding_id = binding_id_from_ctx(ctx);
        let entries = self
            .store
            .list_by_binding(&binding_id)
            .await
            .map_err(map_err)?;
        Ok(json!({
            "binding_id": binding_id,
            "count": entries.len(),
            "entries": entries,
        }))
    }
}

/// `cron_delete` — drop a scheduled entry by id.
pub struct CronDeleteTool {
    store: Arc<dyn CronStore>,
}

impl CronDeleteTool {
    pub fn new(store: Arc<dyn CronStore>) -> Self {
        Self { store }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "cron_delete".to_string(),
            description: "Delete a scheduled cron entry by id. Use cron_list first to find the id.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Entry id from cron_create response or cron_list output."
                    }
                },
                "required": ["id"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for CronDeleteTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let id = args
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("cron_delete requires `id` (string)"))?
            .to_string();
        self.store.delete(&id).await.map_err(map_err)?;
        Ok(json!({"ok": true, "id": id}))
    }
}

fn map_err(e: CronStoreError) -> anyhow::Error {
    match e {
        CronStoreError::InvalidCron(expr, reason) => {
            anyhow::anyhow!("invalid cron expression `{expr}`: {reason}")
        }
        CronStoreError::IntervalTooShort(expr, _) => {
            anyhow::anyhow!(
                "cron expression `{expr}` schedules fires more often than the 60-second minimum"
            )
        }
        CronStoreError::BindingFull(binding, count, max) => {
            anyhow::anyhow!(
                "binding `{binding}` already has {count} cron entries (max {max}) — delete one first via cron_delete"
            )
        }
        CronStoreError::NotFound(id) => {
            anyhow::anyhow!("cron entry `{id}` not found")
        }
        CronStoreError::Sql(s) => anyhow::anyhow!("cron store sqlx error: {s}"),
    }
}

/// Helper exposed for tests + future runtime wiring: compute the
/// next-fire timestamp for a given expression. Re-exports the
/// `cron_schedule` helper through the agent module so callers
/// (poller follow-up, doctor commands) have one entry point.
pub fn next_fire_for(cron_expr: &str, from_unix: i64) -> Result<i64, CronStoreError> {
    next_fire_after(cron_expr, from_unix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron_schedule::SqliteCronStore;
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };

    async fn ctx_with_origin() -> (AgentContext, Arc<dyn CronStore>) {
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
        let ctx = AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
        .with_inbound_origin("whatsapp", "default", "+1234");
        let store: Arc<dyn CronStore> =
            Arc::new(SqliteCronStore::open_memory().await.unwrap());
        (ctx, store)
    }

    #[tokio::test]
    async fn create_persists_entry_with_binding_namespace() {
        let (ctx, store) = ctx_with_origin().await;
        let tool = CronCreateTool::new(store.clone());
        let res = tool
            .call(
                &ctx,
                json!({
                    "cron": "*/5 * * * *",
                    "prompt": "ping ops"
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["binding_id"], "whatsapp:default");
        assert!(res["next_fire_at"].as_i64().unwrap() > 0);
        assert_eq!(
            store.count_by_binding("whatsapp:default").await.unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn create_rejects_invalid_cron() {
        let (ctx, store) = ctx_with_origin().await;
        let tool = CronCreateTool::new(store);
        let err = tool
            .call(
                &ctx,
                json!({"cron": "not a cron", "prompt": "x"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid cron"), "got: {err}");
    }

    #[tokio::test]
    async fn create_rejects_sub_minute() {
        let (ctx, store) = ctx_with_origin().await;
        let tool = CronCreateTool::new(store);
        let err = tool
            .call(
                &ctx,
                json!({"cron": "* * * * * *", "prompt": "x"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("60-second"), "got: {err}");
    }

    #[tokio::test]
    async fn list_returns_only_current_binding_entries() {
        let (ctx, store) = ctx_with_origin().await;
        let create = CronCreateTool::new(store.clone());
        // Insert in current binding.
        create
            .call(&ctx, json!({"cron": "*/5 * * * *", "prompt": "a"}))
            .await
            .unwrap();
        create
            .call(&ctx, json!({"cron": "0 9 * * *", "prompt": "b"}))
            .await
            .unwrap();
        // Insert one in a different binding bypassing the tool —
        // simulates another goal's data.
        let other = build_new_entry(
            &store,
            "telegram:bot",
            "0 */2 * * *",
            "c",
            None,
            true,
        )
        .await
        .unwrap();
        store.insert(&other).await.unwrap();

        let list = CronListTool::new(store);
        let res = list.call(&ctx, json!({})).await.unwrap();
        assert_eq!(res["binding_id"], "whatsapp:default");
        assert_eq!(res["count"], 2);
    }

    #[tokio::test]
    async fn delete_removes_entry() {
        let (ctx, store) = ctx_with_origin().await;
        let create = CronCreateTool::new(store.clone());
        let res = create
            .call(&ctx, json!({"cron": "*/5 * * * *", "prompt": "x"}))
            .await
            .unwrap();
        let id = res["id"].as_str().unwrap().to_string();
        let del = CronDeleteTool::new(store.clone());
        let res2 = del.call(&ctx, json!({"id": id.clone()})).await.unwrap();
        assert_eq!(res2["ok"], true);
        assert_eq!(
            store.count_by_binding("whatsapp:default").await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn delete_unknown_id_errors() {
        let (ctx, store) = ctx_with_origin().await;
        let del = CronDeleteTool::new(store);
        let err = del
            .call(&ctx, json!({"id": "nope"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "got: {err}");
    }

    #[tokio::test]
    async fn create_missing_required_args() {
        let (ctx, store) = ctx_with_origin().await;
        let tool = CronCreateTool::new(store);
        let err1 = tool
            .call(&ctx, json!({"prompt": "x"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err1.contains("requires `cron`"));
        let err2 = tool
            .call(&ctx, json!({"cron": "*/5 * * * *"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err2.contains("requires `prompt`"));
    }

    #[tokio::test]
    async fn fallback_binding_id_uses_agent_id_without_inbound_origin() {
        let cfg = AgentConfig {
            id: "agent-z".into(),
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
        let ctx = AgentContext::new(
            "agent-z",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        );
        let store: Arc<dyn CronStore> =
            Arc::new(SqliteCronStore::open_memory().await.unwrap());
        let tool = CronCreateTool::new(store.clone());
        let res = tool
            .call(&ctx, json!({"cron": "*/5 * * * *", "prompt": "x"}))
            .await
            .unwrap();
        assert_eq!(res["binding_id"], "agent-z");
    }
}
