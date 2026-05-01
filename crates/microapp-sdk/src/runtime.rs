//! JSON-RPC dispatch loop.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::ctx::{HookCtx, ToolCtx};
use crate::errors::{Error, Result as SdkResult, ToolError};
use crate::hook::HookHandler;
use crate::reply::ToolReply;

/// Async tool handler the [`crate::Microapp`] builder accepts.
///
/// Plain async fns matching the signature
/// `async fn(args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError>`
/// implement this trait via blanket impl.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Invoke the tool with the daemon-supplied `args` and a
    /// pre-parsed [`ToolCtx`].
    async fn call(&self, args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError>;
}

#[async_trait]
impl<F, Fut> ToolHandler for F
where
    F: Fn(Value, ToolCtx) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<ToolReply, ToolError>> + Send,
{
    async fn call(&self, args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
        (self)(args, ctx).await
    }
}

/// Handler registry — populated by the [`crate::Microapp`]
/// builder, consumed by the dispatch loop. Public so the test
/// harness (feature `test-harness`) can build one directly.
#[doc(hidden)]
pub struct Handlers {
    /// Microapp identity name (returned in `initialize.server_info.name`).
    pub name: String,
    /// Microapp version (returned in `initialize.server_info.version`).
    pub version: String,
    /// Registered tool handlers keyed by tool name.
    pub tools: BTreeMap<String, Arc<dyn ToolHandler>>,
    /// Registered hook handlers keyed by hook name (no `hooks/`
    /// prefix).
    pub hooks: BTreeMap<String, Arc<dyn HookHandler>>,
}

impl Handlers {
    pub(crate) fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }

    pub(crate) fn hook_names(&self) -> Vec<&str> {
        self.hooks.keys().map(String::as_str).collect()
    }
}

/// Run the JSON-RPC dispatch loop until EOF or `shutdown`.
///
/// `writer` is pre-wrapped in `Arc<Mutex<...>>` so callers (e.g.
/// the test harness) can recover the underlying buffer after
/// the loop returns.
pub(crate) async fn dispatch_loop<R, W>(
    reader: R,
    writer: Arc<Mutex<W>>,
    handlers: Handlers,
) -> SdkResult<()>
where
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                write_error(&writer, None, -32700, &format!("parse error: {e}"), None).await?;
                continue;
            }
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        let stop = handle_one(&handlers, &writer, id, method, params).await?;
        if stop {
            break;
        }
    }
    Ok(())
}

/// Returns `true` when the loop should stop (only `shutdown`
/// triggers this today).
async fn handle_one<W>(
    handlers: &Handlers,
    writer: &Arc<Mutex<W>>,
    id: Option<Value>,
    method: &str,
    params: Value,
) -> SdkResult<bool>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    match method {
        "initialize" => {
            let result = json!({
                "server_info": {
                    "name": handlers.name,
                    "version": handlers.version,
                },
                "tools": handlers.tool_names(),
                "hooks": handlers.hook_names(),
            });
            write_result(writer, id, result).await?;
            Ok(false)
        }
        "tools/list" => {
            let result = json!({ "tools": handlers.tool_names() });
            write_result(writer, id, result).await?;
            Ok(false)
        }
        "tools/call" => {
            let tool_name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            let ctx = build_tool_ctx(&arguments);
            // Strip `_meta` from arguments before passing to the
            // handler so the handler sees only its own fields.
            let stripped_args = strip_meta(arguments.clone());
            match handlers.tools.get(&tool_name) {
                Some(handler) => match handler.call(stripped_args, ctx).await {
                    Ok(reply) => write_result(writer, id, reply.into_value()).await?,
                    Err(e) => {
                        write_error(writer, id, e.code(), &e.to_string(), Some(e.symbolic())).await?
                    }
                },
                None => {
                    write_error(
                        writer,
                        id,
                        -32601,
                        &format!("tool '{tool_name}' not registered"),
                        Some("not_implemented"),
                    )
                    .await?
                }
            }
            Ok(false)
        }
        m if m.starts_with("hooks/") => {
            let hook_name = m.trim_start_matches("hooks/").to_string();
            let ctx = build_hook_ctx(&params);
            match handlers.hooks.get(&hook_name) {
                Some(handler) => match handler.call(params, ctx).await {
                    Ok(outcome) => {
                        let v = serde_json::to_value(&outcome).unwrap_or(json!({"abort": false}));
                        write_result(writer, id, v).await?;
                    }
                    Err(e) => {
                        write_error(writer, id, e.code(), &e.to_string(), Some(e.symbolic())).await?
                    }
                },
                None => {
                    // Unknown hook: default Continue. Don't error
                    // out — daemon may probe hook surface
                    // speculatively.
                    write_result(writer, id, json!({ "abort": false })).await?;
                }
            }
            Ok(false)
        }
        "shutdown" => {
            write_result(writer, id, json!({ "ok": true })).await?;
            Ok(true)
        }
        other => {
            write_error(
                writer,
                id,
                -32601,
                &format!("method not found: {other}"),
                Some("not_implemented"),
            )
            .await?;
            Ok(false)
        }
    }
}

fn build_tool_ctx(arguments: &Value) -> ToolCtx {
    let meta = arguments.get("_meta").cloned().unwrap_or(Value::Null);
    let agent_id = meta
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let session_id = meta
        .get("session_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let binding = nexo_tool_meta::parse_binding_from_meta(&meta);
    let inbound = nexo_tool_meta::parse_inbound_from_meta(&meta);
    ToolCtx {
        agent_id,
        session_id,
        binding,
        inbound,
        #[cfg(not(feature = "outbound"))]
        _outbound_marker: std::marker::PhantomData,
        #[cfg(feature = "outbound")]
        outbound: Arc::new(crate::outbound::OutboundDispatcher::new_stub()),
    }
}

fn build_hook_ctx(params: &Value) -> HookCtx {
    let meta = params.get("_meta").cloned().unwrap_or(Value::Null);
    let agent_id = meta
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let binding = nexo_tool_meta::parse_binding_from_meta(&meta);
    let inbound = nexo_tool_meta::parse_inbound_from_meta(&meta);
    HookCtx {
        agent_id,
        binding,
        inbound,
    }
}

fn strip_meta(mut value: Value) -> Value {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("_meta");
    }
    value
}

async fn write_result<W>(writer: &Arc<Mutex<W>>, id: Option<Value>, result: Value) -> SdkResult<()>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result,
    });
    write_line(writer, &frame).await
}

async fn write_error<W>(
    writer: &Arc<Mutex<W>>,
    id: Option<Value>,
    code: i32,
    message: &str,
    symbolic: Option<&str>,
) -> SdkResult<()>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut error = json!({ "code": code, "message": message });
    if let Some(sym) = symbolic {
        error["data"] = json!({ "code": sym });
    }
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": error,
    });
    write_line(writer, &frame).await
}

async fn write_line<W>(writer: &Arc<Mutex<W>>, value: &Value) -> SdkResult<()>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut line = serde_json::to_string(value).map_err(|e| Error::Internal(e.to_string()))?;
    line.push('\n');
    let mut guard = writer.lock().await;
    guard.write_all(line.as_bytes()).await?;
    guard.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reply::ToolReply;
    use std::io::Cursor;
    use tokio::io::BufReader;

    async fn run_with_lines(handlers: Handlers, input: &str) -> Vec<Value> {
        let cursor = Cursor::new(input.as_bytes().to_vec());
        let reader = BufReader::new(cursor);
        let writer = Vec::new();
        // Dispatch loop writes to a Vec<u8>; we then parse.
        let writer_arc = Arc::new(Mutex::new(writer));
        // Scope to avoid moving handlers into the spawn.
        let writer_for_dispatch = Arc::clone(&writer_arc);
        // Run dispatch directly (not via spawn) so we can recover
        // the writer afterwards.
        let _ = run_with_writer_arc(reader, writer_for_dispatch, handlers).await;
        let bytes = Arc::try_unwrap(writer_arc).unwrap().into_inner();
        bytes_to_lines(bytes)
    }

    async fn run_with_writer_arc(
        reader: BufReader<Cursor<Vec<u8>>>,
        writer: Arc<Mutex<Vec<u8>>>,
        handlers: Handlers,
    ) -> SdkResult<()> {
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let req: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    write_error(&writer, None, -32700, &format!("parse error: {e}"), None).await?;
                    continue;
                }
            };
            let id = req.get("id").cloned();
            let method = req.get("method").and_then(Value::as_str).unwrap_or("");
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            let stop = handle_one(&handlers, &writer, id, method, params).await?;
            if stop {
                break;
            }
        }
        Ok(())
    }

    fn bytes_to_lines(bytes: Vec<u8>) -> Vec<Value> {
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn empty_handlers() -> Handlers {
        Handlers {
            name: "test".into(),
            version: "0.0.0".into(),
            tools: BTreeMap::new(),
            hooks: BTreeMap::new(),
        }
    }

    fn handlers_with_echo() -> Handlers {
        let echo: Arc<dyn ToolHandler> = {
            async fn h(args: Value, _ctx: ToolCtx) -> Result<ToolReply, ToolError> {
                Ok(ToolReply::ok_json(json!({ "echoed": args })))
            }
            Arc::new(h)
        };
        let mut tools: BTreeMap<String, Arc<dyn ToolHandler>> = BTreeMap::new();
        tools.insert("echo".into(), echo);
        Handlers {
            name: "test".into(),
            version: "0.0.0".into(),
            tools,
            hooks: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn initialize_returns_server_info_and_tools() {
        let h = handlers_with_echo();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let lines = run_with_lines(h, &format!("{req}\n")).await;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["result"]["server_info"]["name"], "test");
        assert_eq!(lines[0]["result"]["tools"][0], "echo");
    }

    #[tokio::test]
    async fn tools_list_echos_registered() {
        let h = handlers_with_echo();
        let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let lines = run_with_lines(h, &format!("{req}\n")).await;
        assert_eq!(lines[0]["result"]["tools"][0], "echo");
    }

    #[tokio::test]
    async fn tools_call_happy_path() {
        let h = handlers_with_echo();
        let req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"x":1}}}"#;
        let lines = run_with_lines(h, &format!("{req}\n")).await;
        assert_eq!(lines[0]["result"]["echoed"]["x"], 1);
    }

    #[tokio::test]
    async fn tools_call_unknown_returns_minus_32601() {
        let h = handlers_with_echo();
        let req = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#;
        let lines = run_with_lines(h, &format!("{req}\n")).await;
        assert_eq!(lines[0]["error"]["code"], -32601);
        assert_eq!(lines[0]["error"]["data"]["code"], "not_implemented");
    }

    #[tokio::test]
    async fn unknown_method_returns_minus_32601() {
        let h = empty_handlers();
        let req = r#"{"jsonrpc":"2.0","id":5,"method":"nexo/admin/list"}"#;
        let lines = run_with_lines(h, &format!("{req}\n")).await;
        assert_eq!(lines[0]["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn shutdown_acks_and_stops_loop() {
        let h = empty_handlers();
        let req = r#"{"jsonrpc":"2.0","id":6,"method":"shutdown"}"#;
        // Append a follow-up request to verify it's NOT processed
        // after shutdown.
        let follow = r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#;
        let lines = run_with_lines(h, &format!("{req}\n{follow}\n")).await;
        assert_eq!(lines.len(), 1, "only shutdown processed");
        assert_eq!(lines[0]["result"]["ok"], true);
    }

    #[tokio::test]
    async fn parse_error_continues_loop() {
        let h = empty_handlers();
        let req = "not json\n{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"tools/list\"}";
        let lines = run_with_lines(h, &format!("{req}\n")).await;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["error"]["code"], -32700);
        assert!(lines[1]["result"]["tools"].is_array());
    }

    #[tokio::test]
    async fn hook_unknown_returns_continue_default() {
        let h = empty_handlers();
        let req = r#"{"jsonrpc":"2.0","id":9,"method":"hooks/before_message","params":{}}"#;
        let lines = run_with_lines(h, &format!("{req}\n")).await;
        assert_eq!(lines[0]["result"]["abort"], false);
    }

    #[tokio::test]
    async fn binding_context_parsed_into_tool_ctx() {
        let with_binding: Arc<dyn ToolHandler> = {
            async fn h(_args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
                Ok(ToolReply::ok_json(json!({
                    "agent_id": ctx.agent_id,
                    "channel": ctx.binding.as_ref()
                        .and_then(|b| b.channel.clone())
                        .unwrap_or_default(),
                })))
            }
            Arc::new(h)
        };
        let mut tools = BTreeMap::new();
        tools.insert("introspect".into(), with_binding);
        let h = Handlers {
            name: "t".into(),
            version: "0.0.0".into(),
            tools,
            hooks: BTreeMap::new(),
        };
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "introspect",
                "arguments": {
                    "_meta": {
                        "agent_id": "ana",
                        "session_id": null,
                        "nexo": {
                            "binding": {
                                "agent_id": "ana",
                                "channel": "whatsapp",
                                "account_id": "personal",
                                "binding_id": "whatsapp:personal"
                            }
                        }
                    }
                }
            }
        });
        let line = format!("{req}\n");
        let lines = run_with_lines(h, &line).await;
        assert_eq!(lines[0]["result"]["agent_id"], "ana");
        assert_eq!(lines[0]["result"]["channel"], "whatsapp");
    }

    #[tokio::test]
    async fn inbound_meta_parsed_into_tool_ctx() {
        let with_inbound: Arc<dyn ToolHandler> = {
            async fn h(_args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
                let sender = ctx
                    .inbound
                    .as_ref()
                    .and_then(|i| i.sender_id.clone())
                    .unwrap_or_default();
                let msg = ctx
                    .inbound
                    .as_ref()
                    .and_then(|i| i.msg_id.clone())
                    .unwrap_or_default();
                Ok(ToolReply::ok_json(json!({
                    "sender": sender,
                    "msg": msg,
                })))
            }
            Arc::new(h)
        };
        let mut tools = BTreeMap::new();
        tools.insert("introspect".into(), with_inbound);
        let h = Handlers {
            name: "t".into(),
            version: "0.0.0".into(),
            tools,
            hooks: BTreeMap::new(),
        };
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "introspect",
                "arguments": {
                    "_meta": {
                        "agent_id": "ana",
                        "session_id": null,
                        "nexo": {
                            "inbound": {
                                "kind": "external_user",
                                "sender_id": "+5491100",
                                "msg_id": "wa.ABCD"
                            }
                        }
                    }
                }
            }
        });
        let line = format!("{req}\n");
        let lines = run_with_lines(h, &line).await;
        assert_eq!(lines[0]["result"]["sender"], "+5491100");
        assert_eq!(lines[0]["result"]["msg"], "wa.ABCD");
    }

    #[tokio::test]
    async fn tool_ctx_inbound_returns_none_for_legacy_meta() {
        let probe: Arc<dyn ToolHandler> = {
            async fn h(_args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
                Ok(ToolReply::ok_json(json!({
                    "has_inbound": ctx.inbound.is_some(),
                })))
            }
            Arc::new(h)
        };
        let mut tools = BTreeMap::new();
        tools.insert("probe".into(), probe);
        let h = Handlers {
            name: "t".into(),
            version: "0.0.0".into(),
            tools,
            hooks: BTreeMap::new(),
        };
        // Legacy meta — only `binding`, no `inbound` bucket.
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": {
                "name": "probe",
                "arguments": {
                    "_meta": {
                        "agent_id": "ana",
                        "nexo": {
                            "binding": { "agent_id": "ana", "channel": "whatsapp" }
                        }
                    }
                }
            }
        });
        let line = format!("{req}\n");
        let lines = run_with_lines(h, &line).await;
        assert_eq!(lines[0]["result"]["has_inbound"], false);
    }
}
