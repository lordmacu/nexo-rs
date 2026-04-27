//! Stdio-transport MCP server loop. Reads JSON-RPC requests line by line,
//! dispatches them against the supplied `McpServerHandler`, writes
//! responses to stdout. Exits on stdin EOF or `shutdown_token` cancel.

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::errors::McpError;
use crate::protocol::{is_supported_protocol_version, PROTOCOL_VERSION};

use super::McpServerHandler;

/// Run the server against the real process stdio. Cancel with `shutdown`
/// or close stdin on the peer side to exit.
pub async fn run_stdio_server<H: McpServerHandler>(
    handler: H,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    run_stdio_server_with_auth(handler, shutdown, None).await
}

/// Same as [`run_stdio_server`] but enforces an initialize auth token
/// when `auth_token` is `Some`.
pub async fn run_stdio_server_with_auth<H: McpServerHandler>(
    handler: H,
    shutdown: CancellationToken,
    auth_token: Option<String>,
) -> std::io::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    run_with_io_auth(handler, stdin, stdout, shutdown, auth_token).await
}

/// Transport-agnostic loop used by both the real stdio entry point and
/// the integration tests (which feed `tokio::io::duplex` halves).
pub async fn run_with_io<H, I, O>(
    handler: H,
    stdin: I,
    stdout: O,
    shutdown: CancellationToken,
) -> std::io::Result<()>
where
    H: McpServerHandler,
    I: AsyncRead + Unpin + Send,
    O: AsyncWrite + Unpin + Send,
{
    run_with_io_auth(handler, stdin, stdout, shutdown, None).await
}

/// Auth-capable variant of [`run_with_io`]. When `auth_token` is `Some`,
/// `initialize` must provide the same token via `params.auth_token` or
/// `params._meta.auth_token`.
pub async fn run_with_io_auth<H, I, O>(
    handler: H,
    stdin: I,
    mut stdout: O,
    shutdown: CancellationToken,
    auth_token: Option<String>,
) -> std::io::Result<()>
where
    H: McpServerHandler,
    I: AsyncRead + Unpin + Send,
    O: AsyncWrite + Unpin + Send,
{
    let mut lines = BufReader::new(stdin).lines();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            maybe = lines.next_line() => {
                let line = match maybe {
                    Ok(Some(l)) => l,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "stdin read error");
                        break;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                let request: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, raw = %line, "invalid jsonrpc line");
                        continue;
                    }
                };
                let method = request
                    .get("method")
                    .and_then(|m| m.as_str())
                    .unwrap_or("");
                let params = request
                    .get("params")
                    .cloned()
                    .unwrap_or(Value::Null);
                let id = request.get("id").cloned();

                tracing::debug!(
                    method,
                    id = %id.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
                    "mcp request received"
                );
                let outcome = dispatch(method, params, &handler, auth_token.as_deref()).await;
                let should_shutdown = matches!(outcome, DispatchOutcome::ReplyAndShutdown(_));

                if let Some(id_value) = id {
                    let response = match outcome {
                        DispatchOutcome::Reply(result) => serde_json::json!({
                            "jsonrpc": "2.0",
                            "result": result,
                            "id": id_value,
                        }),
                        DispatchOutcome::ReplyAndShutdown(result) => serde_json::json!({
                            "jsonrpc": "2.0",
                            "result": result,
                            "id": id_value,
                        }),
                        DispatchOutcome::Error { code, message } => serde_json::json!({
                            "jsonrpc": "2.0",
                            "error": {"code": code, "message": message},
                            "id": id_value,
                        }),
                        DispatchOutcome::Silent => continue,
                    };
                    let mut body = response.to_string();
                    body.push('\n');
                    if let Err(e) = stdout.write_all(body.as_bytes()).await {
                        tracing::warn!(error = %e, "stdout write failed");
                        break;
                    }
                    if let Err(e) = stdout.flush().await {
                        tracing::warn!(error = %e, "stdout flush failed");
                        break;
                    }
                } else {
                    // Notification — no response even on error.
                    if let DispatchOutcome::Error { code, message } = outcome {
                        tracing::debug!(method, code, %message, "notification produced error; ignored");
                    }
                }
                if should_shutdown {
                    break;
                }
            }
        }
    }
    Ok(())
}

enum DispatchOutcome {
    Reply(Value),
    ReplyAndShutdown(Value),
    Error { code: i32, message: String },
    Silent,
}

async fn dispatch<H: McpServerHandler>(
    method: &str,
    params: Value,
    handler: &H,
    expected_auth_token: Option<&str>,
) -> DispatchOutcome {
    match method {
        "initialize" => {
            if let Some(expected) = expected_auth_token {
                let provided = extract_auth_token(&params);
                if provided != Some(expected) {
                    tracing::warn!("mcp initialize rejected: invalid auth token");
                    return DispatchOutcome::Error {
                        code: -32001,
                        message: "unauthorized initialize".into(),
                    };
                }
            }
            let client_info = params.get("clientInfo");
            let client_name = client_info
                .and_then(|c| c.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>");
            let client_version = client_info
                .and_then(|c| c.get("version"))
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>");
            // Phase 73 — echo the client's requested protocol
            // version when it is recognised. Claude Code 2.1+ sends
            // `2025-11-25`; replying with our hardcoded
            // `2024-11-05` made Claude treat the server as
            // protocol-mismatched and silently drop every tool the
            // server announced via `tools/list` (the exposed
            // tools/list response was correct, but the permission
            // registry treated the server as if it had zero
            // tools — the operator saw "Available MCP tools:
            // none" while the init log line still claimed
            // `status: connected`). MCP's negotiation rule is
            // "server returns the version it supports, ideally
            // matching the client". When the client speaks a
            // version newer than ours, we still echo ours; when
            // it speaks a known version, we echo its choice so
            // the client moves forward without downgrading.
            let client_protocol_version = params.get("protocolVersion").and_then(|v| v.as_str());
            let agreed_version = match client_protocol_version {
                Some(v) if is_supported_protocol_version(v) => v,
                _ => PROTOCOL_VERSION,
            };
            tracing::info!(
                client_name,
                client_version,
                client_protocol_version = client_protocol_version.unwrap_or("<missing>"),
                agreed_protocol_version = agreed_version,
                "mcp client connected",
            );
            DispatchOutcome::Reply(serde_json::json!({
                "protocolVersion": agreed_version,
                "capabilities": handler.capabilities(),
                "serverInfo": handler.server_info(),
            }))
        }
        "notifications/initialized" | "notifications/cancelled" => {
            tracing::debug!(method, "mcp notification");
            DispatchOutcome::Silent
        }
        "ping" => DispatchOutcome::Reply(serde_json::json!({})),
        "shutdown" => DispatchOutcome::ReplyAndShutdown(Value::Null),
        "completion/complete" => {
            tracing::debug!("mcp completion/complete");
            DispatchOutcome::Reply(serde_json::json!({
                "completion": { "values": [] }
            }))
        }
        "tools/list" => match handler.list_tools().await {
            Ok(tools) => {
                tracing::debug!(count = tools.len(), "mcp tools/list");
                // Phase 73 — omit `nextCursor` when there is no
                // next page. MCP spec treats the field as
                // pagination-only; some clients (Claude Code 2.1)
                // refuse to register tools when the response
                // includes `nextCursor: null` because their
                // schema validator wants either a string token
                // or absence-of-key. Returning an empty cursor as
                // null caused the operator-visible
                // "Available MCP tools: none" while the
                // `mcp_servers.status` line still claimed
                // `connected`.
                DispatchOutcome::Reply(serde_json::json!({ "tools": tools }))
            }
            Err(e) => {
                tracing::warn!(error = %e, "mcp tools/list failed");
                map_handler_error(e)
            }
        },
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            if name.is_empty() {
                tracing::warn!("mcp tools/call missing 'name'");
                return DispatchOutcome::Error {
                    code: -32602,
                    message: "tools/call missing 'name'".into(),
                };
            }
            let started = std::time::Instant::now();
            let result = handler.call_tool(name, args).await;
            let duration_ms = started.elapsed().as_millis() as u64;
            match result {
                Ok(result) => {
                    tracing::info!(
                        tool = %name,
                        duration_ms,
                        is_error = result.is_error,
                        "mcp tool call"
                    );
                    match serde_json::to_value(result) {
                        Ok(v) => DispatchOutcome::Reply(v),
                        Err(e) => DispatchOutcome::Error {
                            code: -32603,
                            message: format!("encode result: {e}"),
                        },
                    }
                }
                Err(McpError::Protocol(msg)) => {
                    tracing::warn!(
                        tool = %name,
                        duration_ms,
                        code = -32602,
                        error = %msg,
                        "mcp tool call failed"
                    );
                    DispatchOutcome::Error {
                        code: -32602,
                        message: msg,
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        tool = %name,
                        duration_ms,
                        error = %e,
                        "mcp tool call failed"
                    );
                    map_handler_error(e)
                }
            }
        }
        "resources/list" => match handler.list_resources().await {
            Ok(resources) => {
                tracing::debug!(count = resources.len(), "mcp resources/list");
                DispatchOutcome::Reply(serde_json::json!({
                    "resources": resources,
                    "nextCursor": Value::Null,
                }))
            }
            Err(e) => {
                tracing::warn!(error = %e, "mcp resources/list failed");
                map_handler_error(e)
            }
        },
        "resources/read" => {
            let uri = params.get("uri").and_then(|n| n.as_str()).unwrap_or("");
            if uri.is_empty() {
                tracing::warn!("mcp resources/read missing 'uri'");
                return DispatchOutcome::Error {
                    code: -32602,
                    message: "resources/read missing 'uri'".into(),
                };
            }
            match handler.read_resource(uri).await {
                Ok(contents) => DispatchOutcome::Reply(serde_json::json!({
                    "contents": contents,
                })),
                Err(e) => {
                    tracing::warn!(uri, error = %e, "mcp resources/read failed");
                    map_handler_error(e)
                }
            }
        }
        "resources/templates/list" => match handler.list_resource_templates().await {
            Ok(templates) => DispatchOutcome::Reply(serde_json::json!({
                "resourceTemplates": templates,
                "nextCursor": Value::Null,
            })),
            Err(e) => {
                tracing::warn!(error = %e, "mcp resources/templates/list failed");
                map_handler_error(e)
            }
        },
        "prompts/list" => match handler.list_prompts().await {
            Ok(prompts) => DispatchOutcome::Reply(serde_json::json!({
                "prompts": prompts,
                "nextCursor": Value::Null,
            })),
            Err(e) => {
                tracing::warn!(error = %e, "mcp prompts/list failed");
                map_handler_error(e)
            }
        },
        "prompts/get" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name.is_empty() {
                tracing::warn!("mcp prompts/get missing 'name'");
                return DispatchOutcome::Error {
                    code: -32602,
                    message: "prompts/get missing 'name'".into(),
                };
            }
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            match handler.get_prompt(name, args).await {
                Ok(result) => match serde_json::to_value(result) {
                    Ok(v) => DispatchOutcome::Reply(v),
                    Err(e) => DispatchOutcome::Error {
                        code: -32603,
                        message: format!("encode result: {e}"),
                    },
                },
                Err(e) => {
                    tracing::warn!(prompt = %name, error = %e, "mcp prompts/get failed");
                    map_handler_error(e)
                }
            }
        }
        _ => {
            tracing::warn!(method, "mcp unknown method");
            DispatchOutcome::Error {
                code: -32601,
                message: format!("method not found: {method}"),
            }
        }
    }
}

fn map_handler_error(err: McpError) -> DispatchOutcome {
    match err {
        McpError::Protocol(message) => DispatchOutcome::Error {
            code: -32602,
            message,
        },
        other => DispatchOutcome::Error {
            code: -32000,
            message: other.to_string(),
        },
    }
}

fn extract_auth_token(params: &Value) -> Option<&str> {
    params
        .get("auth_token")
        .and_then(|v| v.as_str())
        .or_else(|| {
            params
                .get("_meta")
                .and_then(|m| m.get("auth_token"))
                .and_then(|v| v.as_str())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        McpContent, McpPrompt, McpPromptMessage, McpPromptResult, McpResource, McpResourceContent,
        McpServerInfo, McpTool, McpToolResult,
    };
    use async_trait::async_trait;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

    struct MockHandler {
        tools: Vec<McpTool>,
        resources: Vec<McpResource>,
        prompts: Vec<McpPrompt>,
    }

    #[async_trait]
    impl McpServerHandler for MockHandler {
        fn server_info(&self) -> McpServerInfo {
            McpServerInfo {
                name: "mock".into(),
                version: "0.1.0".into(),
            }
        }
        async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
            Ok(self.tools.clone())
        }
        async fn call_tool(&self, name: &str, _args: Value) -> Result<McpToolResult, McpError> {
            if name == "unknown" {
                Err(McpError::Protocol("tool not found".into()))
            } else {
                Ok(McpToolResult {
                    content: vec![McpContent::Text {
                        text: format!("called {name}"),
                    }],
                    is_error: false,
                    structured_content: None,
                })
            }
        }
        async fn list_resources(&self) -> Result<Vec<McpResource>, McpError> {
            Ok(self.resources.clone())
        }
        async fn read_resource(&self, uri: &str) -> Result<Vec<McpResourceContent>, McpError> {
            let found = self.resources.iter().any(|r| r.uri == uri);
            if !found {
                return Err(McpError::Protocol("resource not found".into()));
            }
            Ok(vec![McpResourceContent {
                uri: uri.to_string(),
                mime_type: Some("text/plain".into()),
                text: Some(format!("contents {uri}")),
                blob: None,
            }])
        }
        async fn list_prompts(&self) -> Result<Vec<McpPrompt>, McpError> {
            Ok(self.prompts.clone())
        }
        async fn get_prompt(
            &self,
            name: &str,
            _arguments: Value,
        ) -> Result<McpPromptResult, McpError> {
            if self.prompts.iter().all(|p| p.name != name) {
                return Err(McpError::Protocol("prompt not found".into()));
            }
            Ok(McpPromptResult {
                description: Some(format!("prompt {name}")),
                messages: vec![McpPromptMessage {
                    role: "user".into(),
                    content: McpContent::Text {
                        text: format!("run {name}"),
                    },
                }],
            })
        }
    }

    fn mock_tool(name: &str) -> McpTool {
        McpTool {
            name: name.into(),
            description: Some("x".into()),
            input_schema: serde_json::json!({"type":"object"}),
            output_schema: None,
        }
    }

    fn mock_resource(uri: &str, name: &str) -> McpResource {
        McpResource {
            uri: uri.into(),
            name: name.into(),
            description: Some("resource".into()),
            mime_type: Some("text/plain".into()),
            annotations: None,
        }
    }

    fn mock_prompt(name: &str) -> McpPrompt {
        McpPrompt {
            name: name.into(),
            description: Some("prompt".into()),
            arguments: vec![],
        }
    }

    async fn read_line<R: AsyncRead + Unpin>(r: &mut R) -> String {
        let mut buf = Vec::new();
        let mut one = [0u8; 1];
        loop {
            let n = r.read(&mut one).await.unwrap();
            if n == 0 {
                break;
            }
            buf.push(one[0]);
            if one[0] == b'\n' {
                break;
            }
        }
        String::from_utf8(buf).unwrap()
    }

    #[tokio::test]
    async fn initialize_handshake_roundtrip() {
        let (mut client_tx, server_rx) = duplex(4096);
        let (server_tx, mut client_rx) = duplex(4096);
        let shutdown = CancellationToken::new();
        let handler = MockHandler {
            tools: vec![mock_tool("ping")],
            resources: vec![],
            prompts: vec![],
        };

        let sh = shutdown.clone();
        let server = tokio::spawn(async move {
            run_with_io(handler, server_rx, server_tx, sh)
                .await
                .unwrap();
        });

        client_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"params\":{},\"id\":0}\n")
            .await
            .unwrap();
        let response_line = read_line(&mut client_rx).await;
        let response: Value = serde_json::from_str(response_line.trim()).unwrap();
        assert_eq!(response["id"], 0);
        assert_eq!(response["result"]["serverInfo"]["name"], "mock");
        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);

        shutdown.cancel();
        drop(client_tx);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn tools_list_and_call_roundtrip() {
        let (mut client_tx, server_rx) = duplex(4096);
        let (server_tx, mut client_rx) = duplex(4096);
        let shutdown = CancellationToken::new();
        let handler = MockHandler {
            tools: vec![mock_tool("ping")],
            resources: vec![],
            prompts: vec![],
        };

        let sh = shutdown.clone();
        let server = tokio::spawn(async move {
            run_with_io(handler, server_rx, server_tx, sh)
                .await
                .unwrap();
        });

        client_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"tools/list\",\"id\":1}\n")
            .await
            .unwrap();
        let r1_line = read_line(&mut client_rx).await;
        let r1: Value = serde_json::from_str(r1_line.trim()).unwrap();
        assert_eq!(r1["result"]["tools"][0]["name"], "ping");

        client_tx
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"ping\",\"arguments\":{}},\"id\":2}\n",
            )
            .await
            .unwrap();
        let r2_line = read_line(&mut client_rx).await;
        let r2: Value = serde_json::from_str(r2_line.trim()).unwrap();
        assert_eq!(r2["result"]["content"][0]["text"], "called ping");
        assert_eq!(r2["result"]["isError"], false);

        shutdown.cancel();
        drop(client_tx);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let (mut client_tx, server_rx) = duplex(4096);
        let (server_tx, mut client_rx) = duplex(4096);
        let shutdown = CancellationToken::new();
        let handler = MockHandler {
            tools: vec![],
            resources: vec![],
            prompts: vec![],
        };
        let sh = shutdown.clone();
        let server = tokio::spawn(async move {
            run_with_io(handler, server_rx, server_tx, sh)
                .await
                .unwrap();
        });

        client_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"mystery\",\"id\":9}\n")
            .await
            .unwrap();
        let line = read_line(&mut client_rx).await;
        let response: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(response["error"]["code"], -32601);

        shutdown.cancel();
        drop(client_tx);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_request_replies_and_stops_loop() {
        let (mut client_tx, server_rx) = duplex(4096);
        let (server_tx, mut client_rx) = duplex(4096);
        let shutdown = CancellationToken::new();
        let handler = MockHandler {
            tools: vec![],
            resources: vec![],
            prompts: vec![],
        };
        let sh = shutdown.clone();
        let server = tokio::spawn(async move {
            run_with_io(handler, server_rx, server_tx, sh)
                .await
                .unwrap();
        });

        client_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"shutdown\",\"id\":11}\n")
            .await
            .unwrap();
        let line = read_line(&mut client_rx).await;
        let response: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(response["id"], 11);
        assert_eq!(response["result"], Value::Null);

        // Server should terminate itself after replying to shutdown.
        tokio::time::timeout(std::time::Duration::from_secs(1), server)
            .await
            .expect("server did not stop after shutdown")
            .unwrap();
    }

    #[tokio::test]
    async fn initialize_rejected_without_auth_token_when_required() {
        let (mut client_tx, server_rx) = duplex(4096);
        let (server_tx, mut client_rx) = duplex(4096);
        let shutdown = CancellationToken::new();
        let handler = MockHandler {
            tools: vec![],
            resources: vec![],
            prompts: vec![],
        };
        let sh = shutdown.clone();
        let server = tokio::spawn(async move {
            run_with_io_auth(
                handler,
                server_rx,
                server_tx,
                sh,
                Some("secret-token".to_string()),
            )
            .await
            .unwrap();
        });

        client_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"params\":{},\"id\":12}\n")
            .await
            .unwrap();
        let line = read_line(&mut client_rx).await;
        let response: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(response["id"], 12);
        assert_eq!(response["error"]["code"], -32001);

        shutdown.cancel();
        drop(client_tx);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn initialize_accepts_auth_token_in_meta() {
        let (mut client_tx, server_rx) = duplex(4096);
        let (server_tx, mut client_rx) = duplex(4096);
        let shutdown = CancellationToken::new();
        let handler = MockHandler {
            tools: vec![mock_tool("ping")],
            resources: vec![],
            prompts: vec![],
        };
        let sh = shutdown.clone();
        let server = tokio::spawn(async move {
            run_with_io_auth(
                handler,
                server_rx,
                server_tx,
                sh,
                Some("secret-token".to_string()),
            )
            .await
            .unwrap();
        });

        client_tx
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"params\":{\"_meta\":{\"auth_token\":\"secret-token\"}},\"id\":13}\n",
            )
            .await
            .unwrap();
        let line = read_line(&mut client_rx).await;
        let response: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(response["id"], 13);
        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);

        shutdown.cancel();
        drop(client_tx);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn completion_complete_returns_empty_values() {
        let (mut client_tx, server_rx) = duplex(4096);
        let (server_tx, mut client_rx) = duplex(4096);
        let shutdown = CancellationToken::new();
        let handler = MockHandler {
            tools: vec![],
            resources: vec![],
            prompts: vec![],
        };
        let sh = shutdown.clone();
        let server = tokio::spawn(async move {
            run_with_io(handler, server_rx, server_tx, sh)
                .await
                .unwrap();
        });

        client_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"completion/complete\",\"params\":{},\"id\":14}\n")
            .await
            .unwrap();
        let line = read_line(&mut client_rx).await;
        let response: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(response["id"], 14);
        assert!(response["result"]["completion"]["values"]
            .as_array()
            .is_some());
        assert_eq!(
            response["result"]["completion"]["values"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        shutdown.cancel();
        drop(client_tx);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn resources_list_and_read_roundtrip() {
        let (mut client_tx, server_rx) = duplex(4096);
        let (server_tx, mut client_rx) = duplex(4096);
        let shutdown = CancellationToken::new();
        let handler = MockHandler {
            tools: vec![],
            resources: vec![mock_resource("agent://workspace/soul", "SOUL.md")],
            prompts: vec![],
        };
        let sh = shutdown.clone();
        let server = tokio::spawn(async move {
            run_with_io(handler, server_rx, server_tx, sh)
                .await
                .unwrap();
        });

        client_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"resources/list\",\"id\":15}\n")
            .await
            .unwrap();
        let list_line = read_line(&mut client_rx).await;
        let list_response: Value = serde_json::from_str(list_line.trim()).unwrap();
        assert_eq!(list_response["id"], 15);
        assert_eq!(
            list_response["result"]["resources"][0]["uri"],
            "agent://workspace/soul"
        );

        client_tx
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"method\":\"resources/read\",\"params\":{\"uri\":\"agent://workspace/soul\"},\"id\":16}\n",
            )
            .await
            .unwrap();
        let read_line = read_line(&mut client_rx).await;
        let read_response: Value = serde_json::from_str(read_line.trim()).unwrap();
        assert_eq!(read_response["id"], 16);
        assert_eq!(
            read_response["result"]["contents"][0]["text"],
            "contents agent://workspace/soul"
        );

        shutdown.cancel();
        drop(client_tx);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn prompts_list_and_get_roundtrip() {
        let (mut client_tx, server_rx) = duplex(4096);
        let (server_tx, mut client_rx) = duplex(4096);
        let shutdown = CancellationToken::new();
        let handler = MockHandler {
            tools: vec![],
            resources: vec![],
            prompts: vec![mock_prompt("workspace_soul")],
        };
        let sh = shutdown.clone();
        let server = tokio::spawn(async move {
            run_with_io(handler, server_rx, server_tx, sh)
                .await
                .unwrap();
        });

        client_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"prompts/list\",\"id\":17}\n")
            .await
            .unwrap();
        let list_line = read_line(&mut client_rx).await;
        let list_response: Value = serde_json::from_str(list_line.trim()).unwrap();
        assert_eq!(list_response["id"], 17);
        assert_eq!(
            list_response["result"]["prompts"][0]["name"],
            "workspace_soul"
        );

        client_tx
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"method\":\"prompts/get\",\"params\":{\"name\":\"workspace_soul\",\"arguments\":{}},\"id\":18}\n",
            )
            .await
            .unwrap();
        let get_line = read_line(&mut client_rx).await;
        let get_response: Value = serde_json::from_str(get_line.trim()).unwrap();
        assert_eq!(get_response["id"], 18);
        assert_eq!(
            get_response["result"]["messages"][0]["content"]["text"],
            "run workspace_soul"
        );

        shutdown.cancel();
        drop(client_tx);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn notification_produces_no_response() {
        let (mut client_tx, server_rx) = duplex(4096);
        let (server_tx, mut client_rx) = duplex(4096);
        let shutdown = CancellationToken::new();
        let handler = MockHandler {
            tools: vec![],
            resources: vec![],
            prompts: vec![],
        };
        let sh = shutdown.clone();
        let server = tokio::spawn(async move {
            run_with_io(handler, server_rx, server_tx, sh)
                .await
                .unwrap();
        });

        client_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        // Follow up with a request to observe the stream is alive but no response to the notification.
        client_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"tools/list\",\"id\":10}\n")
            .await
            .unwrap();
        let line = read_line(&mut client_rx).await;
        let response: Value = serde_json::from_str(line.trim()).unwrap();
        // If the server had replied to the notification, we'd see id=null first. Our
        // reader grabs whatever's next; assert it's the tools/list response.
        assert_eq!(response["id"], 10);

        shutdown.cancel();
        drop(client_tx);
        server.await.unwrap();
    }
}
