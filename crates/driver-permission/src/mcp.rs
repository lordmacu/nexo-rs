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
                // Phase 73 — must match the config-key used in
                // `.nexo-mcp.json` ("nexo-driver"). Claude Code 2.1
                // namespaces tools by `mcp__<serverInfo.name>__<tool>`
                // and resolves `--permission-prompt-tool` against
                // that prefix; if `serverInfo.name` and the JSON
                // config-key disagree, Claude registers the server
                // (`status: connected`) but no tool ever lands in
                // the permission registry, surfacing as
                // "Available MCP tools: none" while every Edit /
                // Bash gets denied.
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

    /// Phase 74.2 — explicit output schema. Mirrors the Zod union
    /// Claude Code 2.1 validates internally: every successful
    /// permission_prompt call returns either `{behavior:"allow",
    /// updatedInput: object}` or `{behavior:"deny", message:
    /// string}`. Declaring this on the tool definition lets Claude
    /// type-check the response before forwarding to the model
    /// instead of silently dropping the tool when its inferred
    /// schema and ours drift apart.
    fn output_schema() -> Value {
        // Phase 75 retry — the previous strict variant declared
        // `additionalProperties: false` and a `oneOf` union, which
        // made Claude Code 2.1 silently drop the tool from its
        // permission registry while still reporting the server as
        // `connected`. The accepted shape is a permissive object
        // schema describing the discriminator (`behavior`); Claude
        // tolerates extra fields and the union-by-required-keys
        // pattern when nothing closes the schema.
        serde_json::json!({
            "type": "object",
            "required": ["behavior"],
            "properties": {
                "behavior":     { "type": "string", "enum": ["allow", "deny"] },
                "updatedInput": { "type": "object" },
                "message":      { "type": "string" }
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
            output_schema: Some(Self::output_schema()),
        }])
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpToolResult, McpError> {
        if name != "permission_prompt" {
            return Err(McpError::Protocol(format!("unknown tool {name}")));
        }
        let req: PermissionRequest = serde_json::from_value(arguments)
            .map_err(|e| McpError::Protocol(format!("invalid arguments: {e}")))?;

        // Keep a clone of the original tool input so we can echo it
        // back in the AllowOnce / AllowSession `updatedInput` field
        // — Claude 2.1's permission schema rejects the response when
        // `updatedInput` is absent or non-object (see adapter.rs).
        let original_input = req.input.clone();

        let cache_key = SessionCacheKey::from_request(&req.tool_name, &req.input);
        if let Some(cached) = self.session_cache.get(&cache_key) {
            return Ok(text_result(outcome_to_claude_value(
                cached.value(),
                &original_input,
            )));
        }

        let resp = tokio::time::timeout(self.decision_timeout, self.decider.decide(req))
            .await
            .map_err(|_| McpError::Protocol("decider timeout".into()))?
            .map_err(|e| McpError::Protocol(e.to_string()))?;

        if matches!(&resp.outcome, PermissionOutcome::AllowSession { .. }) {
            self.session_cache.insert(cache_key, resp.outcome.clone());
        }

        Ok(text_result(outcome_to_claude_value(
            &resp.outcome,
            &original_input,
        )))
    }
}

fn text_result(value: Value) -> McpToolResult {
    let is_error = matches!(value.get("behavior").and_then(Value::as_str), Some("deny"));
    // Phase 74.3 — emit BOTH the legacy text content (for clients
    // that still parse it) AND the structured form (for Claude
    // 2.1+ which validates `structuredContent` against the
    // tool's `outputSchema`). Same payload, two channels — costs
    // a clone but eliminates the "re-parse text as JSON" round-
    // trip that surfaced the Zod `updatedInput` flap in Phase 73.
    McpToolResult {
        content: vec![McpContent::Text {
            text: value.to_string(),
        }],
        is_error,
        structured_content: Some(value),
    }
}
