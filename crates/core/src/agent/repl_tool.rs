use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use nexo_llm::ToolDef;

use super::repl_registry::ReplRegistry;
use crate::agent::context::AgentContext;
use crate::agent::tool_registry::ToolHandler;

/// Phase 79.12 — stateful REPL tool. Spawns persistent Python, Node.js,
/// or bash subprocesses that survive across LLM turns.
///
/// Feature-gated behind `repl-tool`. Registered per-agent when
/// `repl.enabled: true` in binding config.
///
/// C2 — `ReplConfig` is no longer captured at construction. The
/// `spawn` handler enforces the per-call `allowed_runtimes` allowlist
/// from `ctx.effective_policy().repl` so a binding-specific override
/// (e.g. agent-level `[python, node, bash]` narrowed to `[python]`
/// for a specific Telegram channel) is observed without restart. The
/// underlying [`ReplRegistry`] still owns timeout / max_output_bytes
/// / max_sessions caps that are subsystem-actor-level (boot-frozen
/// per spec scope).
pub struct ReplTool {
    registry: Arc<ReplRegistry>,
}

impl ReplTool {
    pub fn new(registry: Arc<ReplRegistry>) -> Self {
        Self { registry }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "Repl".into(),
            description: "Run code in a stateful Python, Node.js, or bash REPL session. \
                Sessions persist across turns — spawn once, exec many times. \
                Use for multi-step data analysis, prototyping, or iterative debugging."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["spawn", "exec", "read", "kill", "list"],
                        "description": "Action to perform. spawn: launch a new REPL session. \
                            exec: run code in a session. read: read output without running code. \
                            kill: terminate a session. list: show all active sessions."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session ID returned by spawn. Required for exec, read, kill."
                    },
                    "runtime": {
                        "type": "string",
                        "enum": ["python", "node", "bash"],
                        "description": "REPL runtime to spawn. Required for spawn."
                    },
                    "code": {
                        "type": "string",
                        "description": "Code to execute. Required for exec. \
                            For Python/Node: a complete statement or block. For bash: a shell command."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory for the session. Defaults to agent workspace. \
                            Only valid on spawn."
                    }
                },
                "required": ["action"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for ReplTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> Result<Value> {
        let action = args["action"]
            .as_str()
            .ok_or_else(|| anyhow!("missing 'action' field"))?;

        match action {
            "spawn" => {
                let runtime = args["runtime"]
                    .as_str()
                    .ok_or_else(|| anyhow!("'runtime' required for spawn"))?;
                // C2 — pre-check per-call allowlist from the effective
                // policy. Defense-in-depth: the registry runs its own
                // check against the boot-time `ReplConfig`, but a
                // per-binding override that narrows the agent default
                // is enforced here so it takes effect without daemon
                // restart. An empty list inherits (registry decides).
                let effective = ctx.effective_policy();
                let binding_allow = &effective.repl.allowed_runtimes;
                if !binding_allow.is_empty()
                    && !binding_allow.iter().any(|r| r == runtime)
                {
                    return Err(anyhow!(
                        "runtime '{}' not in this binding's allowed_runtimes ({:?})",
                        runtime,
                        binding_allow
                    ));
                }
                let cwd = args["cwd"].as_str();
                let session_id = self.registry.spawn(runtime, cwd).await?;
                Ok(json!({
                    "session_id": session_id,
                    "runtime": runtime,
                    "message": format!("REPL session started ({})", runtime)
                }))
            }
            "exec" => {
                let session_id = args["session_id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("'session_id' required for exec"))?;
                let code = args["code"]
                    .as_str()
                    .ok_or_else(|| anyhow!("'code' required for exec"))?;
                let output = self.registry.exec(session_id, code).await?;
                Ok(json!({
                    "stdout": output.stdout,
                    "stderr": output.stderr,
                    "timed_out": output.timed_out,
                    "exit_code": output.exit_code
                }))
            }
            "read" => {
                let session_id = args["session_id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("'session_id' required for read"))?;
                let output = self.registry.read(session_id).await?;
                Ok(json!({
                    "stdout": output.stdout,
                    "stderr": output.stderr
                }))
            }
            "kill" => {
                let session_id = args["session_id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("'session_id' required for kill"))?;
                self.registry.kill(session_id).await?;
                Ok(json!({
                    "message": format!("session {} terminated", session_id)
                }))
            }
            "list" => {
                let sessions = self.registry.list();
                let list: Vec<Value> = sessions
                    .iter()
                    .map(|s| {
                        json!({
                            "session_id": s.id,
                            "runtime": s.runtime,
                            "cwd": s.cwd,
                            "spawned_at": s.spawned_at.to_rfc3339(),
                            "output_len": s.output_len,
                            "exit_code": s.exit_code
                        })
                    })
                    .collect();
                Ok(json!({
                    "sessions": list,
                    "count": list.len()
                }))
            }
            other => Err(anyhow!(
                "unknown action '{}' — valid: spawn, exec, read, kill, list",
                other
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::effective::EffectiveBindingPolicy;
    use nexo_broker::AnyBroker;
    use nexo_config::types::repl::ReplConfig;
    use nexo_config::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };

    fn make_agent_with_repl(allowed: Vec<String>) -> Arc<AgentConfig> {
        Arc::new(AgentConfig {
            id: "a".into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m".into(),
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
            lsp: Default::default(),
            config_tool: Default::default(),
            team: Default::default(),
            proactive: Default::default(),
            repl: ReplConfig {
                enabled: true,
                allowed_runtimes: allowed,
                max_sessions: 1,
                timeout_secs: 1,
                max_output_bytes: 1024,
            },
        })
    }

    fn make_ctx(agent: Arc<AgentConfig>) -> AgentContext {
        let effective = Arc::new(EffectiveBindingPolicy::from_agent_defaults(&agent));
        AgentContext::new(
            "a",
            agent,
            AnyBroker::local(),
            Arc::new(crate::session::SessionManager::new(
                std::time::Duration::from_secs(60),
                8,
            )),
        )
        .with_effective(effective)
    }

    fn make_tool() -> ReplTool {
        let registry = Arc::new(ReplRegistry::new(
            ReplConfig {
                enabled: true,
                allowed_runtimes: vec!["python".into(), "node".into(), "bash".into()],
                max_sessions: 1,
                timeout_secs: 1,
                max_output_bytes: 1024,
            },
            "/tmp".into(),
        ));
        ReplTool::new(registry)
    }

    /// C2 — per-binding allowlist override (here narrower than the
    /// agent-wide registry config) refuses spawn before reaching the
    /// registry. Reload-pickup proxy: this same code path is hit on
    /// the next intake event after a snapshot swap.
    #[tokio::test]
    async fn spawn_refused_when_runtime_not_in_per_binding_allowlist() {
        let agent = make_agent_with_repl(vec!["python".into()]); // no node
        let ctx = make_ctx(agent);
        let tool = make_tool();
        let res = tool
            .call(&ctx, json!({ "action": "spawn", "runtime": "node" }))
            .await;
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("not in this binding's allowed_runtimes"),
            "expected per-binding refusal, got: {err}"
        );
    }

    /// C2 — empty per-binding allowlist inherits from the registry
    /// (which holds the agent-level boot config). Inheritance keeps
    /// existing YAML behaviour.
    #[tokio::test]
    async fn spawn_inherits_when_per_binding_allowlist_empty() {
        let agent = make_agent_with_repl(Vec::new()); // empty = inherit
        let ctx = make_ctx(agent);
        let tool = make_tool();
        // The registry will accept any of [python, node, bash]; we
        // only assert that the per-call gate did not refuse. Because
        // spawning a real node process in unit tests is flaky, we
        // pick `bash` which is always available and assert success
        // (the spawn returns a session id).
        let res = tool
            .call(&ctx, json!({ "action": "spawn", "runtime": "bash" }))
            .await;
        let v = res.expect("spawn should succeed when allowlist is empty");
        assert!(
            v.get("session_id").and_then(|s| s.as_str()).is_some(),
            "spawn ok but no session_id in {v}"
        );
    }
}
