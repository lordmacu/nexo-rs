//! Test harness (feature `test-harness`).
//!
//! `MicroappTestHarness::new(app)` wraps a [`crate::Microapp`]
//! and exposes synchronous-shaped helpers (`call_tool`,
//! `call_tool_with_binding`, `fire_hook`) that drive the dispatch
//! loop in-process without spawning the binary or touching
//! stdio.

use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::BufReader;
use tokio::sync::Mutex;

use crate::builder::Microapp;
use crate::hook::HookOutcome;
use nexo_tool_meta::{BindingContext, InboundMessageMeta};

/// Test-harness errors.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum MicroappTestError {
    /// JSON-RPC frame parse failed (test input was malformed).
    #[error("json parse: {0}")]
    Parse(String),
    /// The dispatch loop returned an error frame; carry the
    /// raw `error` value for assertions.
    #[error("rpc error: {0}")]
    RpcError(Value),
    /// Internal harness wiring failure.
    #[error("harness internal: {0}")]
    Internal(String),
}

/// In-process driver for unit tests.
pub struct MicroappTestHarness {
    app: Mutex<Option<Microapp>>,
}

impl MicroappTestHarness {
    /// Build a new harness wrapping `app`.
    pub fn new(app: Microapp) -> Self {
        Self {
            app: Mutex::new(Some(app)),
        }
    }

    /// Invoke a tool by name with `arguments`. Returns the JSON
    /// `result` on success or `MicroappTestError::RpcError(error)`
    /// on JSON-RPC error.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Value, MicroappTestError> {
        self.call_tool_inner(tool_name, arguments, None, None).await
    }

    /// Invoke a tool with a pre-built `BindingContext` injected
    /// into `_meta.nexo.binding`.
    pub async fn call_tool_with_binding(
        &self,
        tool_name: &str,
        arguments: Value,
        binding: BindingContext,
    ) -> Result<Value, MicroappTestError> {
        self.call_tool_inner(tool_name, arguments, Some(binding), None)
            .await
    }

    /// Invoke a tool with a pre-built `InboundMessageMeta`
    /// injected into `_meta.nexo.inbound` (no binding context).
    pub async fn call_tool_with_inbound(
        &self,
        tool_name: &str,
        arguments: Value,
        inbound: InboundMessageMeta,
    ) -> Result<Value, MicroappTestError> {
        self.call_tool_inner(tool_name, arguments, None, Some(inbound))
            .await
    }

    /// Invoke a tool with both a `BindingContext` and an
    /// `InboundMessageMeta` injected into `_meta.nexo.*`.
    pub async fn call_tool_with_binding_and_inbound(
        &self,
        tool_name: &str,
        arguments: Value,
        binding: BindingContext,
        inbound: InboundMessageMeta,
    ) -> Result<Value, MicroappTestError> {
        self.call_tool_inner(tool_name, arguments, Some(binding), Some(inbound))
            .await
    }

    async fn call_tool_inner(
        &self,
        tool_name: &str,
        mut arguments: Value,
        binding: Option<BindingContext>,
        inbound: Option<InboundMessageMeta>,
    ) -> Result<Value, MicroappTestError> {
        if binding.is_some() || inbound.is_some() {
            // Determine agent_id/session_id from binding when available.
            let (agent_id, session_id) = binding
                .as_ref()
                .map(|b| (b.agent_id.clone(), b.session_id))
                .unwrap_or_else(|| ("test".into(), None));
            let meta = nexo_tool_meta::build_meta_value(
                &agent_id,
                session_id,
                binding.as_ref(),
                inbound.as_ref(),
            );
            if let Some(obj) = arguments.as_object_mut() {
                obj.insert("_meta".into(), meta);
            } else {
                arguments = json!({"_meta": meta});
            }
        }
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool_name, "arguments": arguments }
        });
        let resp = self.run_one_request(req).await?;
        match (resp.get("result"), resp.get("error")) {
            (Some(r), _) => Ok(r.clone()),
            (None, Some(e)) => Err(MicroappTestError::RpcError(e.clone())),
            _ => Err(MicroappTestError::Internal(
                "neither result nor error".into(),
            )),
        }
    }

    /// Fire a hook by name; returns the parsed [`HookOutcome`].
    pub async fn fire_hook(
        &self,
        hook_name: &str,
        args: Value,
    ) -> Result<HookOutcome, MicroappTestError> {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": format!("hooks/{hook_name}"),
            "params": args
        });
        let resp = self.run_one_request(req).await?;
        let result = resp.get("result").cloned().ok_or_else(|| {
            MicroappTestError::RpcError(resp.get("error").cloned().unwrap_or(Value::Null))
        })?;
        let abort = result.get("abort").and_then(Value::as_bool).unwrap_or(false);
        if abort {
            let reason = result
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(HookOutcome::Abort { reason })
        } else {
            Ok(HookOutcome::Continue)
        }
    }

    async fn run_one_request(&self, req: Value) -> Result<Value, MicroappTestError> {
        let app = self
            .app
            .lock()
            .await
            .take()
            .ok_or_else(|| MicroappTestError::Internal("harness already consumed".into()))?;
        let mut input = serde_json::to_string(&req)
            .map_err(|e| MicroappTestError::Internal(e.to_string()))?;
        input.push('\n');
        let reader = BufReader::new(std::io::Cursor::new(input.into_bytes()));
        let writer: Vec<u8> = Vec::new();
        // Run the loop — it processes one request and then sees
        // EOF on the in-memory cursor, terminating naturally.
        let writer_arc = std::sync::Arc::new(Mutex::new(writer));
        let writer_for_run = std::sync::Arc::clone(&writer_arc);
        let handlers = app.into_handlers();
        crate::runtime::dispatch_loop(reader, writer_for_run, handlers)
            .await
            .map_err(|e| MicroappTestError::Internal(e.to_string()))?;
        let bytes = std::sync::Arc::try_unwrap(writer_arc)
            .map_err(|_| MicroappTestError::Internal("writer arc still shared".into()))?
            .into_inner();
        let line = String::from_utf8(bytes)
            .map_err(|e| MicroappTestError::Internal(e.to_string()))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Err(MicroappTestError::Internal("no response".into()));
        }
        // Take the first line — `run_one_request` only sends one
        // request, so there is exactly one response frame.
        let first_line = trimmed.lines().next().ok_or_else(|| {
            MicroappTestError::Internal("response had no lines".into())
        })?;
        serde_json::from_str(first_line).map_err(|e| MicroappTestError::Parse(e.to_string()))
    }
}

// ── Phase 83.15 — MockBindingContext builder ─────────────────────

/// Phase 83.15 — fluent builder for [`BindingContext`] in tests.
///
/// `BindingContext` ships with `agent_only(...)` for the no-binding
/// case. Multi-binding microapp tests need a richer builder so each
/// test reads at the call site like the YAML it mirrors:
///
/// ```ignore
/// let ctx = MockBindingContext::new()
///     .with_agent("ana")
///     .with_channel("whatsapp")
///     .with_account("acme")
///     .build();
/// ```
///
/// Sets `binding_id` automatically when both `channel` and one of
/// `account_id` / no-account are specified — matches the canonical
/// `<channel>:<account_id|"default">` render the daemon produces
/// at boot.
#[derive(Debug, Clone, Default)]
pub struct MockBindingContext {
    agent_id: Option<String>,
    channel: Option<String>,
    account_id: Option<String>,
    session_id: Option<uuid::Uuid>,
    mcp_channel_source: Option<String>,
}

impl MockBindingContext {
    /// Fresh builder with every field unset.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the agent id (required — `build()` panics if unset).
    pub fn with_agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    /// Set the channel name (`"whatsapp"`, `"telegram"`, `"web"`,
    /// …). Drives `binding_id` rendering.
    pub fn with_channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    /// Set the account / instance discriminator. Goes into
    /// `binding_id` as the second segment; `None` (default)
    /// renders as `"default"`.
    pub fn with_account(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    /// Pin a specific session UUID. Default leaves `None` (no
    /// active LLM turn).
    pub fn with_session(mut self, session_id: uuid::Uuid) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Layer the MCP channel-source label on top.
    pub fn with_mcp_channel_source(
        mut self,
        source: impl Into<String>,
    ) -> Self {
        self.mcp_channel_source = Some(source.into());
        self
    }

    /// Materialise the [`BindingContext`]. Panics when `agent_id`
    /// is unset — every binding has an agent owner.
    pub fn build(self) -> BindingContext {
        let agent_id = self.agent_id.expect(
            "MockBindingContext: with_agent(...) is required before build()",
        );
        let mut ctx = BindingContext::agent_only(agent_id);
        ctx.session_id = self.session_id;
        ctx.channel = self.channel.clone();
        ctx.account_id = self.account_id.clone();
        ctx.binding_id = self.channel.as_deref().map(|ch| {
            nexo_tool_meta::binding_id_render(ch, self.account_id.as_deref())
        });
        if let Some(s) = self.mcp_channel_source {
            ctx = ctx.with_mcp_channel_source(s);
        }
        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::ToolCtx;
    use crate::errors::ToolError;
    use crate::reply::ToolReply;

    async fn echo(args: Value, _ctx: ToolCtx) -> Result<ToolReply, ToolError> {
        Ok(ToolReply::ok_json(args))
    }

    #[tokio::test]
    async fn call_tool_happy_path() {
        let app = Microapp::new("t", "0.0.0").with_tool("echo", echo);
        let h = MicroappTestHarness::new(app);
        let out = h.call_tool("echo", json!({"x": 1})).await.unwrap();
        assert_eq!(out["x"], 1);
    }

    #[tokio::test]
    async fn call_tool_with_binding_injects_meta() {
        async fn read_binding(
            args: Value,
            ctx: ToolCtx,
        ) -> Result<ToolReply, ToolError> {
            let _ = args;
            let channel = ctx
                .binding
                .as_ref()
                .and_then(|b| b.channel.clone())
                .unwrap_or_default();
            Ok(ToolReply::ok_json(json!({"channel": channel})))
        }
        let app = Microapp::new("t", "0.0.0").with_tool("read", read_binding);
        let h = MicroappTestHarness::new(app);
        let mut binding = BindingContext::agent_only("ana");
        binding.channel = Some("whatsapp".into());
        binding.account_id = Some("personal".into());
        let out = h
            .call_tool_with_binding("read", json!({}), binding)
            .await
            .unwrap();
        assert_eq!(out["channel"], "whatsapp");
    }

    #[tokio::test]
    async fn call_tool_with_inbound_injects_inbound_meta() {
        async fn read_inbound(args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
            let _ = args;
            let sender = ctx
                .inbound
                .as_ref()
                .and_then(|i| i.sender_id.clone())
                .unwrap_or_default();
            Ok(ToolReply::ok_json(json!({"sender": sender})))
        }
        let app = Microapp::new("t", "0.0.0").with_tool("read", read_inbound);
        let h = MicroappTestHarness::new(app);
        let inbound = InboundMessageMeta::external_user("+5491100", "wa.X");
        let out = h
            .call_tool_with_inbound("read", json!({}), inbound)
            .await
            .unwrap();
        assert_eq!(out["sender"], "+5491100");
    }

    #[tokio::test]
    async fn call_tool_with_binding_and_inbound_carries_both() {
        async fn read_both(args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
            let _ = args;
            let channel = ctx
                .binding
                .as_ref()
                .and_then(|b| b.channel.clone())
                .unwrap_or_default();
            let sender = ctx
                .inbound
                .as_ref()
                .and_then(|i| i.sender_id.clone())
                .unwrap_or_default();
            Ok(ToolReply::ok_json(json!({
                "channel": channel,
                "sender": sender,
            })))
        }
        let app = Microapp::new("t", "0.0.0").with_tool("read", read_both);
        let h = MicroappTestHarness::new(app);
        let mut binding = BindingContext::agent_only("ana");
        binding.channel = Some("whatsapp".into());
        binding.account_id = Some("personal".into());
        let inbound = InboundMessageMeta::external_user("+5491100", "wa.X");
        let out = h
            .call_tool_with_binding_and_inbound("read", json!({}), binding, inbound)
            .await
            .unwrap();
        assert_eq!(out["channel"], "whatsapp");
        assert_eq!(out["sender"], "+5491100");
    }

    // ── Phase 83.15 — MockBindingContext tests ───────────────

    #[test]
    fn mock_binding_minimal_agent_only() {
        let ctx = MockBindingContext::new().with_agent("ana").build();
        assert_eq!(ctx.agent_id, "ana");
        assert!(ctx.channel.is_none());
        assert!(ctx.binding_id.is_none());
    }

    #[test]
    fn mock_binding_renders_binding_id_with_account() {
        let ctx = MockBindingContext::new()
            .with_agent("ana")
            .with_channel("whatsapp")
            .with_account("acme")
            .build();
        assert_eq!(ctx.channel.as_deref(), Some("whatsapp"));
        assert_eq!(ctx.account_id.as_deref(), Some("acme"));
        assert_eq!(ctx.binding_id.as_deref(), Some("whatsapp:acme"));
    }

    #[test]
    fn mock_binding_renders_default_segment_when_no_account() {
        let ctx = MockBindingContext::new()
            .with_agent("ana")
            .with_channel("telegram")
            .build();
        // Canonical render is `<channel>:<account|"default">`.
        assert_eq!(ctx.binding_id.as_deref(), Some("telegram:default"));
    }

    #[test]
    fn mock_binding_carries_session_uuid() {
        let id = uuid::Uuid::new_v4();
        let ctx = MockBindingContext::new()
            .with_agent("ana")
            .with_session(id)
            .build();
        assert_eq!(ctx.session_id, Some(id));
    }

    #[test]
    fn mock_binding_layers_mcp_channel_source() {
        let ctx = MockBindingContext::new()
            .with_agent("ana")
            .with_channel("telegram")
            .with_mcp_channel_source("slack")
            .build();
        assert_eq!(ctx.mcp_channel_source.as_deref(), Some("slack"));
    }

    #[test]
    #[should_panic(expected = "with_agent")]
    fn mock_binding_panics_when_agent_unset() {
        let _ = MockBindingContext::new().build();
    }

    #[test]
    fn mock_binding_chains_call_tool_with_harness() {
        // Smoke test: the builder output plugs cleanly into
        // MicroappTestHarness::call_tool_with_binding without
        // any glue.
        async fn read(_args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
            let ag = ctx
                .binding()
                .map(|b| b.agent_id.clone())
                .unwrap_or_default();
            Ok(ToolReply::ok_json(json!({ "agent": ag })))
        }
        let app = Microapp::new("t", "0.0.0").with_tool("read", read);
        let h = MicroappTestHarness::new(app);
        let ctx = MockBindingContext::new()
            .with_agent("ana")
            .with_channel("whatsapp")
            .build();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt
            .block_on(h.call_tool_with_binding("read", json!({}), ctx))
            .unwrap();
        assert_eq!(out["agent"], "ana");
    }
}
