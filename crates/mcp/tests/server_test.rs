//! Phase 12.6 — integration: stdio server end-to-end via `tokio::io::duplex`.

use std::time::Duration;

use async_trait::async_trait;
use nexo_mcp::types::{McpContent, McpServerInfo, McpTool, McpToolResult};
use nexo_mcp::{run_with_io, McpError, McpServerHandler};
use serde_json::Value;
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

struct FixtureHandler;

#[async_trait]
impl McpServerHandler for FixtureHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "fixture".into(),
            version: "0.1.0".into(),
        }
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![McpTool {
            name: "echo".into(),
            description: Some("echoes its input back".into()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
            }),
        }])
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpToolResult, McpError> {
        if name != "echo" {
            return Err(McpError::Protocol("unknown tool".into()));
        }
        let text = arguments
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(McpToolResult {
            content: vec![McpContent::Text {
                text: format!("echo: {text}"),
            }],
            is_error: false,
        })
    }
}

async fn read_line<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> String {
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

async fn send(out: &mut tokio::io::DuplexStream, json: &str) {
    out.write_all(json.as_bytes()).await.unwrap();
    out.write_all(b"\n").await.unwrap();
    out.flush().await.unwrap();
}

#[tokio::test]
async fn end_to_end_initialize_list_call() {
    let (mut client_tx, server_rx) = duplex(4096);
    let (server_tx, mut client_rx) = duplex(4096);
    let shutdown = CancellationToken::new();
    let sh = shutdown.clone();
    let server =
        tokio::spawn(async move { run_with_io(FixtureHandler, server_rx, server_tx, sh).await });

    send(
        &mut client_tx,
        r#"{"jsonrpc":"2.0","method":"initialize","params":{},"id":0}"#,
    )
    .await;
    let init: Value = serde_json::from_str(read_line(&mut client_rx).await.trim()).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "fixture");

    send(
        &mut client_tx,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await;

    send(
        &mut client_tx,
        r#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#,
    )
    .await;
    let list: Value = serde_json::from_str(read_line(&mut client_rx).await.trim()).unwrap();
    assert_eq!(list["result"]["tools"][0]["name"], "echo");

    send(
        &mut client_tx,
        r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}},"id":2}"#,
    )
    .await;
    let call: Value = serde_json::from_str(read_line(&mut client_rx).await.trim()).unwrap();
    assert_eq!(call["result"]["content"][0]["text"], "echo: hi");
    assert_eq!(call["result"]["isError"], false);

    shutdown.cancel();
    drop(client_tx);
    let _ = tokio::time::timeout(Duration::from_millis(500), server).await;
}

#[tokio::test]
async fn call_unknown_tool_returns_jsonrpc_error() {
    let (mut client_tx, server_rx) = duplex(4096);
    let (server_tx, mut client_rx) = duplex(4096);
    let shutdown = CancellationToken::new();
    let sh = shutdown.clone();
    let server =
        tokio::spawn(async move { run_with_io(FixtureHandler, server_rx, server_tx, sh).await });

    send(
        &mut client_tx,
        r#"{"jsonrpc":"2.0","method":"initialize","params":{},"id":0}"#,
    )
    .await;
    let _ = read_line(&mut client_rx).await;

    send(
        &mut client_tx,
        r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"ghost","arguments":{}},"id":3}"#,
    )
    .await;
    let v: Value = serde_json::from_str(read_line(&mut client_rx).await.trim()).unwrap();
    assert_eq!(v["error"]["code"], -32602);

    shutdown.cancel();
    drop(client_tx);
    let _ = tokio::time::timeout(Duration::from_millis(500), server).await;
}

#[tokio::test]
async fn stdin_eof_ends_loop() {
    let (client_tx, server_rx) = duplex(4096);
    let (server_tx, _client_rx) = duplex(4096);
    let shutdown = CancellationToken::new();
    let sh = shutdown.clone();
    let server =
        tokio::spawn(async move { run_with_io(FixtureHandler, server_rx, server_tx, sh).await });

    drop(client_tx); // signal EOF
    let result = tokio::time::timeout(Duration::from_secs(2), server).await;
    assert!(result.is_ok(), "server should exit on stdin EOF");
}
