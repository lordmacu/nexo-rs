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
use nexo_tool_meta::BindingContext;

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
        self.call_tool_inner(tool_name, arguments, None).await
    }

    /// Invoke a tool with a pre-built `BindingContext` injected
    /// into `_meta.nexo.binding`.
    pub async fn call_tool_with_binding(
        &self,
        tool_name: &str,
        arguments: Value,
        binding: BindingContext,
    ) -> Result<Value, MicroappTestError> {
        self.call_tool_inner(tool_name, arguments, Some(binding))
            .await
    }

    async fn call_tool_inner(
        &self,
        tool_name: &str,
        mut arguments: Value,
        binding: Option<BindingContext>,
    ) -> Result<Value, MicroappTestError> {
        if let Some(b) = binding {
            // Inject _meta.nexo.binding into arguments.
            let meta = nexo_tool_meta::build_meta_value(&b.agent_id, b.session_id, Some(&b), None);
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
}
