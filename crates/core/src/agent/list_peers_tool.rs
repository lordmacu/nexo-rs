//! Phase 80.11 — `list_peers` LLM tool.
//!
//! Returns the agent's peers (other in-process agents) with optional
//! reachability metadata when an `allowed_delegates` policy filter
//! is in effect. Pure read; no side effects. Pairs with
//! `send_to_peer` as the discovery half of the multi-agent
//! coordination flow.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};

pub const LIST_PEERS_TOOL_NAME: &str = "list_peers";

pub struct ListPeersTool;

impl ListPeersTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: LIST_PEERS_TOOL_NAME.to_string(),
            description: "List other agents in this process you can reach via `send_to_peer`. \
                Returns each peer's `agent_id` + optional description + reachability flag based \
                on the binding's `allowed_delegates` filter. Self is excluded. Use this before \
                `send_to_peer` to discover valid `to:` targets."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for ListPeersTool {
    async fn call(&self, ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        let peers = match &ctx.peers {
            Some(d) => d,
            None => {
                return Ok(json!({
                    "peers": [],
                    "note": "this agent has no PeerDirectory configured"
                }));
            }
        };

        let allowed = ctx.effective_policy().allowed_delegates.clone();
        let self_id = ctx.agent_id.clone();
        let entries: Vec<Value> = peers
            .peers()
            .iter()
            .filter(|p| p.id != self_id)
            .map(|p| {
                let reachable = allowed.is_empty()
                    || allowed.iter().any(|pat| match pat.strip_suffix('*') {
                        Some(stem) => p.id.starts_with(stem),
                        None => pat == &p.id,
                    });
                json!({
                    "agent_id": p.id,
                    "description": p.description,
                    "reachable": reachable,
                })
            })
            .collect();

        Ok(json!({
            "peers": entries,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_def_shape() {
        let def = ListPeersTool::tool_def();
        assert_eq!(def.name, "list_peers");
        assert!(def.description.contains("send_to_peer"));
        // No required params.
        let req = def.parameters.get("required");
        assert!(req.is_none() || req.unwrap().as_array().map(|a| a.is_empty()).unwrap_or(true));
    }
}
