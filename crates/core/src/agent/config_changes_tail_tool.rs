//! Phase 79.10 — `config_changes_tail` tool.
//!
//! Read-only LLM tool that surfaces the latest entries in the
//! `ConfigChangesStore` audit table. Always available regardless
//! of the `config-self-edit` Cargo feature flag; reads only.
//!
//! Pattern lift: `crates/core/src/agent/agent_turns_tail_tool.rs`
//! (Phase 72) — same shape, different store.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use crate::config_changes_store::{ConfigChangeRow, ConfigChangesStore};
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use std::sync::Arc;

const DEFAULT_N: usize = 20;
const MAX_N: usize = 200;

pub struct ConfigChangesTailTool {
    store: Arc<dyn ConfigChangesStore>,
}

impl ConfigChangesTailTool {
    pub fn new(store: Arc<dyn ConfigChangesStore>) -> Self {
        Self { store }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "config_changes_tail".to_string(),
            description: format!(
                "Tail the ConfigTool audit log: lists the latest config-change events (proposed / applied / rolled_back / rejected / expired) across all agents. Read-only. Default `n` = {DEFAULT_N}, capped at {MAX_N}."
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "n": {
                        "type": "integer",
                        "minimum": 1,
                        "description": format!(
                            "How many newest rows to return. Default {DEFAULT_N}, max {MAX_N}."
                        )
                    }
                }
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for ConfigChangesTailTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let n_arg = args
            .get("n")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_N);
        let n = n_arg.clamp(1, MAX_N);
        let rows = self
            .store
            .tail(n)
            .await
            .map_err(|e| anyhow::anyhow!("config_changes_tail: {e}"))?;
        Ok(json!({
            "ok": true,
            "n": rows.len(),
            "formatted": render_markdown_table(&rows),
            "rows": rows
                .iter()
                .map(row_to_json)
                .collect::<Vec<_>>(),
        }))
    }
}

fn row_to_json(row: &ConfigChangeRow) -> Value {
    json!({
        "patch_id": row.patch_id,
        "binding_id": row.binding_id,
        "agent_id": row.agent_id,
        "op": row.op,
        "key": row.key,
        "value": row.value,
        "status": row.status,
        "error": row.error,
        "created_at": row.created_at,
        "applied_at": row.applied_at,
    })
}

fn render_markdown_table(rows: &[ConfigChangeRow]) -> String {
    if rows.is_empty() {
        return "(no config changes)".to_string();
    }
    let mut out = String::new();
    out.push_str("| created_at | patch_id | agent | binding | op | status | key | error |\n");
    out.push_str("|------------|----------|-------|---------|----|--------|-----|-------|\n");
    for r in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.created_at,
            short(&r.patch_id, 10),
            r.agent_id,
            r.binding_id,
            r.op,
            r.status,
            r.key,
            r.error.as_deref().unwrap_or("")
        ));
    }
    out.trim_end().to_string()
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_changes_store::SqliteConfigChangesStore;
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };

    fn agent_context_fixture() -> AgentContext {
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
        };
        AgentContext::new(
            "test-agent",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    fn fixture(patch_id: &str, status: &str, created_at: i64) -> ConfigChangeRow {
        ConfigChangeRow {
            patch_id: patch_id.into(),
            binding_id: "wa:default".into(),
            agent_id: "cody".into(),
            op: "propose".into(),
            key: "model.model".into(),
            value: Some("\"claude-opus-4-7\"".into()),
            status: status.into(),
            error: None,
            created_at,
            applied_at: None,
        }
    }

    #[tokio::test]
    async fn tail_default_n_returns_recent_rows() {
        let store: Arc<dyn ConfigChangesStore> =
            Arc::new(SqliteConfigChangesStore::open_in_memory().await.unwrap());
        for i in 0..30 {
            store
                .record(&fixture(&format!("p{i:03}"), "proposed", i as i64))
                .await
                .unwrap();
        }
        let tool = ConfigChangesTailTool::new(store);
        // No `n` arg → default 20.
        let res = tool
            .call(&agent_context_fixture(), json!({}))
            .await
            .unwrap();
        assert_eq!(res["n"], 20);
        assert_eq!(res["rows"].as_array().unwrap().len(), 20);
    }

    #[tokio::test]
    async fn tail_renders_markdown_table_with_header() {
        let store: Arc<dyn ConfigChangesStore> =
            Arc::new(SqliteConfigChangesStore::open_in_memory().await.unwrap());
        store
            .record(&fixture("01J7AAA", "proposed", 100))
            .await
            .unwrap();
        let tool = ConfigChangesTailTool::new(store);
        let res = tool
            .call(&agent_context_fixture(), json!({ "n": 5 }))
            .await
            .unwrap();
        let formatted = res["formatted"].as_str().unwrap();
        assert!(formatted.contains("created_at"));
        assert!(formatted.contains("patch_id"));
        assert!(formatted.contains("01J7AAA"));
        assert!(formatted.contains("model.model"));
    }

    #[tokio::test]
    async fn tail_handles_empty_store() {
        let store: Arc<dyn ConfigChangesStore> =
            Arc::new(SqliteConfigChangesStore::open_in_memory().await.unwrap());
        let tool = ConfigChangesTailTool::new(store);
        let res = tool
            .call(&agent_context_fixture(), json!({}))
            .await
            .unwrap();
        assert_eq!(res["n"], 0);
        assert_eq!(res["formatted"], "(no config changes)");
    }

    #[tokio::test]
    async fn tail_caps_n_at_max() {
        let store: Arc<dyn ConfigChangesStore> =
            Arc::new(SqliteConfigChangesStore::open_in_memory().await.unwrap());
        for i in 0..250 {
            store
                .record(&fixture(&format!("p{i:03}"), "proposed", i as i64))
                .await
                .unwrap();
        }
        let tool = ConfigChangesTailTool::new(store);
        // Caller asks 1000; capped at MAX_N (200).
        let res = tool
            .call(&agent_context_fixture(), json!({ "n": 1000 }))
            .await
            .unwrap();
        assert_eq!(res["n"], 200);
    }
}
