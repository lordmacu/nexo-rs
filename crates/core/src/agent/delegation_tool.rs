use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use nexo_llm::ToolDef;
use async_trait::async_trait;
use serde_json::{json, Value};
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub struct DelegationTool;
impl DelegationTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "delegate".to_string(),
            description: "Delegate a task to another agent and wait for the result.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "Target agent id."
                    },
                    "task": {
                        "type": "string",
                        "description": "Task to delegate."
                    },
                    "context": {
                        "type": "object",
                        "description": "Optional structured context for the target agent."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Optional timeout in milliseconds (default 30000)."
                    }
                },
                "required": ["agent_id", "task"]
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for DelegationTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let target = args["agent_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("delegate requires `agent_id`"))?;
        let task = args["task"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("delegate requires `task`"))?;
        // Peer allowlist — empty = no restriction (back-compat).
        // Populated = target must match at least one pattern. Glob
        // uses the same trailing-`*` convention as `allowed_tools`.
        // Read from the effective policy so a narrow binding can block
        // delegation even if the agent-level list is permissive, or a
        // trusted channel can broaden it to `["*"]`.
        let effective = ctx.effective_policy();
        let allowlist = &effective.allowed_delegates;
        if !allowlist.is_empty() && !delegate_matches(allowlist, target) {
            anyhow::bail!(
                "agent `{}` is not allowed to delegate to `{}` (allowed_delegates: {:?})",
                ctx.agent_id,
                target,
                allowlist,
            );
        }
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(DEFAULT_TIMEOUT_MS);
        let delegate_context = args["context"].clone();
        let router = ctx
            .router
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("delegate router is unavailable in this runtime"))?;
        let output = router
            .delegate(
                &ctx.broker,
                &ctx.agent_id,
                target,
                task,
                delegate_context,
                timeout_ms,
            )
            .await?;
        Ok(json!({
            "ok": true,
            "agent_id": target,
            "output": output,
        }))
    }
}

fn delegate_matches(patterns: &[String], target: &str) -> bool {
    patterns.iter().any(|p| match p.strip_suffix('*') {
        Some(stem) => target.starts_with(stem),
        None => p == target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_wildcard_patterns() {
        let list = vec!["ventas".to_string(), "soporte_*".to_string()];
        assert!(delegate_matches(&list, "ventas"));
        assert!(delegate_matches(&list, "soporte_nivel1"));
        assert!(delegate_matches(&list, "soporte_"));
        assert!(!delegate_matches(&list, "boss"));
        assert!(!delegate_matches(&list, "venta"));
    }

    #[test]
    fn empty_list_matches_nothing_caller_must_short_circuit() {
        // delegate_matches returns false on empty — the tool handler
        // short-circuits before calling this (empty = no restriction),
        // so we never actually hit this with an empty list in prod.
        assert!(!delegate_matches(&[], "ventas"));
    }
}
