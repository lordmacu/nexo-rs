//! Stdio-transport MCP server loop. Reads JSON-RPC requests line by line,
//! dispatches them against the supplied `McpServerHandler`, writes
//! responses to stdout. Exits on stdin EOF or `shutdown_token` cancel.

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::errors::McpError;
use crate::protocol::PROTOCOL_VERSION;

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
            tracing::info!(client_name, client_version, "mcp client connected");
            DispatchOutcome::Reply(serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
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
                DispatchOutcome::Reply(serde_json::json!({
                    "tools": tools,
                    "nextCursor": Value::Null,
                }))
            }
            Err(e) => {
                tracing::warn!(error = %e, "mcp tools/list failed");
                DispatchOutcome::Error {
                    code: -32000,
                    message: e.to_string(),
                }
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
                    DispatchOutcome::Error {
                        code: -32000,
                        message: e.to_string(),
                    }
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

// Serialize McpTool/McpToolResult: the types derive Deserialize only; we
// need Serialize to put them back on the wire. Keep it local to avoid
// polluting the public types.
mod _impl_serialize {
    use crate::types::{McpContent, McpResourceRef, McpTool, McpToolResult};
    use serde::ser::SerializeMap;
    use serde::Serialize;

    impl Serialize for McpTool {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut map = s.serialize_map(Some(3))?;
            map.serialize_entry("name", &self.name)?;
            if let Some(d) = &self.description {
                map.serialize_entry("description", d)?;
            }
            map.serialize_entry("inputSchema", &self.input_schema)?;
            map.end()
        }
    }

    impl Serialize for McpContent {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut map = s.serialize_map(Some(3))?;
            match self {
                McpContent::Text { text } => {
                    map.serialize_entry("type", "text")?;
                    map.serialize_entry("text", text)?;
                }
                McpContent::Image { data, mime_type } => {
                    map.serialize_entry("type", "image")?;
                    map.serialize_entry("data", data)?;
                    map.serialize_entry("mimeType", mime_type)?;
                }
                McpContent::Resource { resource } => {
                    map.serialize_entry("type", "resource")?;
                    map.serialize_entry("resource", resource)?;
                }
            }
            map.end()
        }
    }

    impl Serialize for McpResourceRef {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut map = s.serialize_map(Some(3))?;
            map.serialize_entry("uri", &self.uri)?;
            if let Some(t) = &self.text {
                map.serialize_entry("text", t)?;
            }
            if let Some(m) = &self.mime_type {
                map.serialize_entry("mimeType", m)?;
            }
            map.end()
        }
    }

    impl Serialize for McpToolResult {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut map = s.serialize_map(Some(2))?;
            map.serialize_entry("content", &self.content)?;
            map.serialize_entry("isError", &self.is_error)?;
            map.end()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{McpContent, McpServerInfo, McpTool, McpToolResult};
    use async_trait::async_trait;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

    struct MockHandler {
        tools: Vec<McpTool>,
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
                })
            }
        }
    }

    fn mock_tool(name: &str) -> McpTool {
        McpTool {
            name: name.into(),
            description: Some("x".into()),
            input_schema: serde_json::json!({"type":"object"}),
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
        let handler = MockHandler { tools: vec![] };
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
        let handler = MockHandler { tools: vec![] };
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
        let handler = MockHandler { tools: vec![] };
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
        let handler = MockHandler { tools: vec![] };
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
    async fn notification_produces_no_response() {
        let (mut client_tx, server_rx) = duplex(4096);
        let (server_tx, mut client_rx) = duplex(4096);
        let shutdown = CancellationToken::new();
        let handler = MockHandler { tools: vec![] };
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
