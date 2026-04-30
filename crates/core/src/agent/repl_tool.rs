use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use nexo_config::types::repl::ReplConfig;
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
pub struct ReplTool {
    registry: Arc<ReplRegistry>,
    #[allow(dead_code)]
    config: ReplConfig,
}

impl ReplTool {
    pub fn new(registry: Arc<ReplRegistry>, config: ReplConfig) -> Self {
        Self { registry, config }
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
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        let action = args["action"]
            .as_str()
            .ok_or_else(|| anyhow!("missing 'action' field"))?;

        match action {
            "spawn" => {
                let runtime = args["runtime"]
                    .as_str()
                    .ok_or_else(|| anyhow!("'runtime' required for spawn"))?;
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
