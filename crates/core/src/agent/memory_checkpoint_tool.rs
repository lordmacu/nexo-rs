//! Phase 10.9 — `forge_memory_checkpoint` native tool. Lets the LLM mark a
//! moment in the workspace git history (end of project, significant fact
//! learned, etc.) with an arbitrary note.
use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use super::workspace_git::MemoryGitRepo;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use std::sync::Arc;
pub struct MemoryCheckpointTool {
    git: Arc<MemoryGitRepo>,
}
impl MemoryCheckpointTool {
    pub fn new(git: Arc<MemoryGitRepo>) -> Self {
        Self { git }
    }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "forge_memory_checkpoint".into(),
            description:
                "Snapshot the current state of the agent workspace as a git commit. Useful before \
                 a risky memory update, at the end of a project phase, or to mark a significant \
                 fact. Returns the new short oid, or skipped=true when nothing changed."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "note": { "type": "string", "description": "Short description of why this checkpoint exists." }
                },
                "additionalProperties": false
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for MemoryCheckpointTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let note = args
            .get("note")
            .and_then(|v| v.as_str())
            .unwrap_or("manual");
        let subject = format!("checkpoint: {note}");
        let body = format!("manual checkpoint at {}", chrono::Utc::now().to_rfc3339());
        let git = self.git.clone();
        let outcome = tokio::task::spawn_blocking(move || git.commit_all(&subject, &body))
            .await
            .map_err(|e| anyhow::anyhow!("checkpoint task join failed: {e}"))??;
        match outcome {
            Some(oid) => Ok(json!({
                "ok": true,
                "oid": short_oid(&oid),
                "subject": format!("checkpoint: {note}"),
                "skipped": false,
            })),
            None => Ok(json!({
                "ok": true,
                "skipped": true,
                "reason": "workspace tree is clean; no commit created"
            })),
        }
    }
}
fn short_oid(oid: &git2::Oid) -> String {
    format!("{oid}").chars().take(7).collect()
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
    async fn checkpoint_creates_commit_when_tree_dirty() {
        let td = TempDir::new().unwrap();
        let git = Arc::new(MemoryGitRepo::open_or_init(td.path(), "kate", "k@t").unwrap());
        std::fs::write(td.path().join("MEMORY.md"), "hi").unwrap();
        let tool = MemoryCheckpointTool::new(git);
        let out = tool
            .call(&ctx(), json!({"note": "fresh write"}))
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["skipped"], false);
        assert!(out["oid"].as_str().unwrap().len() == 7);
    }
    #[tokio::test]
    async fn checkpoint_skips_when_tree_clean() {
        let td = TempDir::new().unwrap();
        let git = Arc::new(MemoryGitRepo::open_or_init(td.path(), "kate", "k@t").unwrap());
        let tool = MemoryCheckpointTool::new(git);
        let out = tool.call(&ctx(), json!({"note": "nop"})).await.unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["skipped"], true);
    }
}
