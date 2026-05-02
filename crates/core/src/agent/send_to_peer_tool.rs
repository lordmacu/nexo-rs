//! Phase 80.11 — `send_to_peer` LLM tool.
//!
//! Fire-and-forget peer-to-peer messaging via NATS subject
//! `agent.inbox.<goal_id>`. Resolves the `to:` argument to a peer
//! agent_id, looks up the peer's running goals (caller-provided
//! lookup fn), and publishes the message to each goal's inbox
//! subject. Returns the list of `delivered_to` goals plus
//! `unreachable_reasons` for goals or peers that couldn't accept.
//!
//! Receive side (subscriber + buffer + next-turn injection) is the
//! deferred 80.11.b follow-up. Today's MVP ships the publisher half.
//!
//! # Provider-agnostic
//!
//! Pure JSON tool surface + NATS subject contract. Works under any
//! LLM provider — the tool's behaviour is below the LLM round-trip.

use super::context::AgentContext;
use super::inbox::{inbox_subject, InboxMessage, MAX_BODY_BYTES, MIN_BODY_CHARS};
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use chrono::Utc;
use nexo_broker::types::Event;
use nexo_broker::BrokerHandle;
use nexo_driver_types::GoalId;
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use std::sync::Arc;

pub const SEND_TO_PEER_TOOL_NAME: &str = "send_to_peer";

/// Lookup function: given an agent_id, return the live goal_ids of
/// that peer at call time. Empty `Vec` means the peer has no live
/// goals (the receiver is offline / not yet spawned). Caller wires
/// this to `nexo_agent_registry::AgentRegistry::list` filtered by
/// `agent_id` + Running status — kept as a closure so the tool
/// stays free of an agent-registry dep.
pub type PeerGoalLookup =
    Arc<dyn Fn(&str) -> Vec<GoalId> + Send + Sync + 'static>;

pub struct SendToPeerTool {
    lookup: PeerGoalLookup,
}

impl SendToPeerTool {
    pub fn new(lookup: PeerGoalLookup) -> Self {
        Self { lookup }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: SEND_TO_PEER_TOOL_NAME.to_string(),
            description: "Send a fire-and-forget message to another agent's inbox. \
                The receiver processes the message at the start of its next turn. \
                Use `list_peers` first to discover valid `to:` targets. Plain text \
                output of yours is NOT visible to other agents — `send_to_peer` is \
                the ONLY way to communicate with them. Self-sends are rejected. Empty \
                bodies are rejected. Bodies > 64 KB are rejected."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "Peer agent id (matches an entry in `list_peers`)."
                    },
                    "message": {
                        "type": "string",
                        "description": "Plain text message body (1-65536 chars)."
                    },
                    "correlation_id": {
                        "type": "string",
                        "description": "Optional UUID for request/response correlation."
                    }
                },
                "required": ["to", "message"],
                "additionalProperties": false
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for SendToPeerTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let to = args
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("send_to_peer: `to` is required"))?
            .trim();
        if to.is_empty() {
            anyhow::bail!("send_to_peer: `to` cannot be empty");
        }
        if to == ctx.agent_id {
            anyhow::bail!("send_to_peer: cannot send to self (`{to}`)");
        }
        let body = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("send_to_peer: `message` is required"))?;
        if body.chars().count() < MIN_BODY_CHARS {
            anyhow::bail!("send_to_peer: `message` is empty");
        }
        if body.len() > MAX_BODY_BYTES {
            anyhow::bail!(
                "send_to_peer: `message` is {} bytes, max {}",
                body.len(),
                MAX_BODY_BYTES
            );
        }
        let correlation_id = args
            .get("correlation_id")
            .and_then(|v| v.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok());

        // Verify peer is in the directory at all (fast-path
        // unreachable if peer name doesn't match anything).
        if let Some(dir) = &ctx.peers {
            let known = dir.peers().iter().any(|p| p.id == to);
            if !known {
                return Ok(json!({
                    "delivered_to": [],
                    "unreachable_reasons": [format!("unknown agent_id `{}`", to)]
                }));
            }
        }

        // Caller-provided lookup → goals_to_deliver.
        let goals = (self.lookup)(to);
        if goals.is_empty() {
            return Ok(json!({
                "delivered_to": [],
                "unreachable_reasons": [format!("no live goals for agent_id `{}`", to)]
            }));
        }

        // Sender's own goal_id — best-effort from session id when
        // present. None when the agent is mid-turn without a session
        // (e.g. heartbeat path); we still record provenance via
        // `from_agent_id`.
        let from_goal = ctx.session_id.map(GoalId).unwrap_or_else(GoalId::new);
        let now = Utc::now();

        let mut delivered = Vec::with_capacity(goals.len());
        let mut unreachable: Vec<String> = Vec::new();

        for goal in goals {
            let msg = InboxMessage {
                from_agent_id: ctx.agent_id.clone(),
                from_goal_id: from_goal,
                to_agent_id: to.to_string(),
                body: body.to_string(),
                sent_at: now,
                correlation_id,
            };
            let payload = match serde_json::to_value(&msg) {
                Ok(v) => v,
                Err(e) => {
                    unreachable.push(format!(
                        "serialise failed for goal {}: {}",
                        goal.0, e
                    ));
                    continue;
                }
            };
            let event = Event::new(inbox_subject(goal), &ctx.agent_id, payload);
            let topic = inbox_subject(goal);
            match ctx.broker.publish(&topic, event).await {
                Ok(_) => delivered.push(goal.0.to_string()),
                Err(e) => unreachable
                    .push(format!("broker publish failed for goal {}: {}", goal.0, e)),
            }
        }

        Ok(json!({
            "delivered_to": delivered,
            "unreachable_reasons": unreachable,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::peer_directory::{PeerDirectory, PeerSummary};
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig,
    };
    use std::time::Duration;

    fn lookup_returns_one_goal(_id: &str) -> Vec<GoalId> {
        vec![GoalId(uuid::Uuid::new_v4())]
    }
    fn lookup_returns_empty(_id: &str) -> Vec<GoalId> {
        Vec::new()
    }

    fn mk_lookup(f: fn(&str) -> Vec<GoalId>) -> PeerGoalLookup {
        Arc::new(f)
    }

    fn mk_peers() -> Arc<PeerDirectory> {
        PeerDirectory::new(vec![
            PeerSummary {
                id: "researcher".into(),
                description: "research agent".into(),
            },
            PeerSummary {
                id: "writer".into(),
                description: "writer agent".into(),
            },
        ])
    }

    fn mk_ctx() -> AgentContext {
        let cfg = Arc::new(AgentConfig {
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
            empresa_id: None,
            extensions_config: std::collections::BTreeMap::new(),
        });
        let broker = AnyBroker::local();
        let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
        let mut ctx = AgentContext::new("kate", cfg, broker, sessions);
        ctx.peers = Some(mk_peers());
        ctx.session_id = Some(uuid::Uuid::new_v4());
        ctx
    }

    #[test]
    fn tool_def_shape() {
        let def = SendToPeerTool::tool_def();
        assert_eq!(def.name, "send_to_peer");
        let req = def.parameters["required"].as_array().unwrap();
        assert!(req.iter().any(|v| v == "to"));
        assert!(req.iter().any(|v| v == "message"));
    }

    #[tokio::test]
    async fn empty_to_errors() {
        let tool = SendToPeerTool::new(mk_lookup(lookup_returns_one_goal));
        let ctx = mk_ctx();
        let err = tool
            .call(&ctx, json!({"to": "", "message": "x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn missing_to_errors() {
        let tool = SendToPeerTool::new(mk_lookup(lookup_returns_one_goal));
        let ctx = mk_ctx();
        let err = tool
            .call(&ctx, json!({"message": "x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("`to` is required"));
    }

    #[tokio::test]
    async fn missing_message_errors() {
        let tool = SendToPeerTool::new(mk_lookup(lookup_returns_one_goal));
        let ctx = mk_ctx();
        let err = tool
            .call(&ctx, json!({"to": "researcher"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("`message` is required"));
    }

    #[tokio::test]
    async fn empty_message_errors() {
        let tool = SendToPeerTool::new(mk_lookup(lookup_returns_one_goal));
        let ctx = mk_ctx();
        let err = tool
            .call(&ctx, json!({"to": "researcher", "message": ""}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[tokio::test]
    async fn self_send_rejected() {
        let tool = SendToPeerTool::new(mk_lookup(lookup_returns_one_goal));
        let ctx = mk_ctx();
        let err = tool
            .call(&ctx, json!({"to": "kate", "message": "hello me"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("send to self"));
    }

    #[tokio::test]
    async fn unknown_agent_id_returns_unreachable() {
        let tool = SendToPeerTool::new(mk_lookup(lookup_returns_one_goal));
        let ctx = mk_ctx();
        let v = tool
            .call(
                &ctx,
                json!({"to": "ghost", "message": "anyone there"}),
            )
            .await
            .unwrap();
        assert_eq!(v["delivered_to"].as_array().unwrap().len(), 0);
        let reasons = v["unreachable_reasons"].as_array().unwrap();
        assert!(reasons[0].as_str().unwrap().contains("unknown agent_id"));
    }

    #[tokio::test]
    async fn no_live_goals_returns_unreachable() {
        let tool = SendToPeerTool::new(mk_lookup(lookup_returns_empty));
        let ctx = mk_ctx();
        let v = tool
            .call(&ctx, json!({"to": "researcher", "message": "ping"}))
            .await
            .unwrap();
        assert_eq!(v["delivered_to"].as_array().unwrap().len(), 0);
        let reasons = v["unreachable_reasons"].as_array().unwrap();
        assert!(reasons[0].as_str().unwrap().contains("no live goals"));
    }

    #[tokio::test]
    async fn live_peer_publishes_and_returns_delivered() {
        let tool = SendToPeerTool::new(mk_lookup(lookup_returns_one_goal));
        let ctx = mk_ctx();
        let v = tool
            .call(&ctx, json!({"to": "researcher", "message": "task #1"}))
            .await
            .unwrap();
        assert_eq!(v["delivered_to"].as_array().unwrap().len(), 1);
        assert_eq!(v["unreachable_reasons"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn oversize_message_rejected() {
        let tool = SendToPeerTool::new(mk_lookup(lookup_returns_one_goal));
        let ctx = mk_ctx();
        let huge = "x".repeat(MAX_BODY_BYTES + 1);
        let err = tool
            .call(&ctx, json!({"to": "researcher", "message": huge}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("max"));
    }

    #[tokio::test]
    async fn correlation_id_round_trips() {
        let captured_corr: std::sync::Mutex<Option<uuid::Uuid>> =
            std::sync::Mutex::new(None);
        let captured = Arc::new(captured_corr);

        // Subscribe to the inbox subject so we can assert the wire
        // payload carries the correlation_id we passed.
        let ctx = mk_ctx();
        let goal = GoalId(uuid::Uuid::new_v4());
        let lookup: PeerGoalLookup = {
            let g = goal;
            Arc::new(move |_id: &str| vec![g])
        };
        let topic = inbox_subject(goal);
        let mut sub = ctx.broker.subscribe(&topic).await.unwrap();
        let cap = Arc::clone(&captured);
        let handle = tokio::spawn(async move {
            if let Ok(Some(ev)) = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                sub.next(),
            )
            .await
            {
                let msg: InboxMessage = serde_json::from_value(ev.payload).unwrap();
                *cap.lock().unwrap() = msg.correlation_id;
            }
        });

        let corr = uuid::Uuid::new_v4();
        let tool = SendToPeerTool::new(lookup);
        let _ = tool
            .call(
                &ctx,
                json!({
                    "to": "researcher",
                    "message": "ping",
                    "correlation_id": corr.to_string(),
                }),
            )
            .await
            .unwrap();
        // Give the subscriber a moment.
        let _ = handle.await;
        let captured_value = *captured.lock().unwrap();
        assert_eq!(captured_value, Some(corr));
    }
}
