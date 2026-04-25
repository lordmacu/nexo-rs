//! `PermissionMcpServer<D>` — `McpServerHandler` impl that exposes a
//! single `permission_prompt` tool. Caches `AllowSession` outcomes
//! in-process so a Claude turn that re-reads the same file doesn't
//! pay a decider round-trip per call.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use nexo_mcp::server::McpServerHandler;
use nexo_mcp::{McpContent, McpError, McpServerInfo, McpTool, McpToolResult};
use serde_json::Value;

use crate::adapter::outcome_to_claude_value;
use crate::cache::SessionCacheKey;
use crate::decider::PermissionDecider;
use crate::types::{PermissionOutcome, PermissionRequest};

const DEFAULT_DECISION_TIMEOUT: Duration = Duration::from_secs(30);

pub struct PermissionMcpServer<D: ?Sized + PermissionDecider = dyn PermissionDecider> {
    decider: Arc<D>,
    server_info: McpServerInfo,
    session_cache: DashMap<SessionCacheKey, PermissionOutcome>,
    decision_timeout: Duration,
}

impl<D: ?Sized + PermissionDecider> PermissionMcpServer<D> {
    pub fn new(decider: Arc<D>) -> Self {
        Self {
            decider,
            server_info: McpServerInfo {
                name: "nexo-driver-permission".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            session_cache: DashMap::new(),
            decision_timeout: DEFAULT_DECISION_TIMEOUT,
        }
    }

    pub fn server_info(mut self, info: McpServerInfo) -> Self {
        self.server_info = info;
        self
    }

    pub fn decision_timeout(mut self, t: Duration) -> Self {
        self.decision_timeout = t;
        self
    }

    fn input_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["tool_name", "input"],
            "properties": {
                "tool_name":   { "type": "string" },
                "input":       { "type": "object" },
                "tool_use_id": { "type": "string" },
                "metadata":    { "type": "object" },
                "goal_id":     { "type": "string" },
            }
        })
    }
}

#[async_trait]
impl<D: ?Sized + PermissionDecider> McpServerHandler for PermissionMcpServer<D> {
    fn server_info(&self) -> McpServerInfo {
        self.server_info.clone()
    }

    fn capabilities(&self) -> Value {
        serde_json::json!({ "tools": { "listChanged": false } })
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![McpTool {
            name: "permission_prompt".into(),
            description: Some(
                "Ask the nexo-rs driver agent whether to allow the proposed tool call.".into(),
            ),
            input_schema: Self::input_schema(),
        }])
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpToolResult, McpError> {
        if name != "permission_prompt" {
            return Err(McpError::Protocol(format!("unknown tool {name}")));
        }
        let req: PermissionRequest = serde_json::from_value(arguments)
            .map_err(|e| McpError::Protocol(format!("invalid arguments: {e}")))?;

        let cache_key = SessionCacheKey::from_request(&req.tool_name, &req.input);
        if let Some(cached) = self.session_cache.get(&cache_key) {
            return Ok(text_result(outcome_to_claude_value(cached.value())));
        }

        let resp = tokio::time::timeout(self.decision_timeout, self.decider.decide(req))
            .await
            .map_err(|_| McpError::Protocol("decider timeout".into()))?
            .map_err(|e| McpError::Protocol(e.to_string()))?;

        if matches!(&resp.outcome, PermissionOutcome::AllowSession { .. }) {
            self.session_cache.insert(cache_key, resp.outcome.clone());
        }

        Ok(text_result(outcome_to_claude_value(&resp.outcome)))
    }
}

fn text_result(value: Value) -> McpToolResult {
    let is_error = matches!(value.get("behavior").and_then(Value::as_str), Some("deny"));
    McpToolResult {
        content: vec![McpContent::Text {
            text: value.to_string(),
        }],
        is_error,
    }
}
