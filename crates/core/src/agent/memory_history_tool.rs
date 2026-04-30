//! Phase 10.9 — `memory_history` native tool. Gives the LLM cheap access
//! to its own recent git log + optional unified diff (DiffMem pattern).
use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use super::workspace_git::MemoryGitRepo;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use std::sync::Arc;
pub struct MemoryHistoryTool {
    git: Arc<MemoryGitRepo>,
}
impl MemoryHistoryTool {
    pub fn new(git: Arc<MemoryGitRepo>) -> Self {
        Self { git }
    }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "memory_history".into(),
            description:
                "Inspect the agent's own workspace git log. Returns the last N commits as an \
                 array of {oid, subject, body, author, timestamp}. Pass include_diff=true to \
                 also receive a unified patch from the oldest returned commit up to HEAD."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "description": "Max commits (default 10).", "minimum": 1, "maximum": 100 },
                    "include_diff": { "type": "boolean", "description": "Include unified patch text." }
                },
                "additionalProperties": false
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for MemoryHistoryTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .clamp(1, 100) as usize;
        let include_diff = args
            .get("include_diff")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let git = self.git.clone();
        let commits = tokio::task::spawn_blocking(move || git.log(limit))
            .await
            .map_err(|e| anyhow::anyhow!("history task join failed: {e}"))??;
        let diff = if include_diff && commits.len() > 1 {
            // oldest oid in the returned slice
            let oldest_hex = commits.last().unwrap().oid.clone();
            let git = self.git.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
                let oid = git2::Oid::from_str(&oldest_hex).ok();
                git.diff_since(oid)
            })
            .await
            .map_err(|e| anyhow::anyhow!("diff task join failed: {e}"))??
        } else {
            String::new()
        };
        let mut out = json!({ "commits": commits });
        if !diff.is_empty() {
            out["diff"] = Value::String(diff);
        }
        Ok(out)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig,
    };
    use tempfile::TempDir;
    fn ctx() -> AgentContext {
        let cfg = Arc::new(AgentConfig {
            id: "test".into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m".into(),
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
        });
        let broker = AnyBroker::local();
        let sessions = Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 20));
        AgentContext::new("test", cfg, broker, sessions)
    }
    #[tokio::test]
    async fn history_lists_bootstrap_commit() {
        let td = TempDir::new().unwrap();
        let git = Arc::new(MemoryGitRepo::open_or_init(td.path(), "kate", "k@t").unwrap());
        let tool = MemoryHistoryTool::new(git);
        let out = tool.call(&ctx(), json!({"limit": 5})).await.unwrap();
        let commits = out["commits"].as_array().unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0]["subject"], "workspace init");
        assert!(out.get("diff").is_none());
    }
    #[tokio::test]
    async fn history_with_diff_returns_patch() {
        let td = TempDir::new().unwrap();
        let git = Arc::new(MemoryGitRepo::open_or_init(td.path(), "kate", "k@t").unwrap());
        std::fs::write(td.path().join("MEMORY.md"), "one\n").unwrap();
        git.commit_all("first", "").unwrap();
        std::fs::write(td.path().join("MEMORY.md"), "two\n").unwrap();
        git.commit_all("second", "").unwrap();
        let tool = MemoryHistoryTool::new(git);
        let out = tool
            .call(&ctx(), json!({"limit": 5, "include_diff": true}))
            .await
            .unwrap();
        let diff = out.get("diff").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            diff.contains("+two") || diff.contains("+one"),
            "expected patch text: {diff}"
        );
    }
}
