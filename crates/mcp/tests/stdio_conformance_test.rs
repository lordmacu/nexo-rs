//! Phase 76.12 — replay of the 11 spec MCP 2025-11-25 conformance
//! cases through the stdio transport.
//!
//! Pattern from OpenClaw `research/src/mcp/channel-server.test.ts:25-35`
//! (in-memory linked pair). Rust equivalent: `tokio::io::duplex` +
//! `run_with_io(handler, server_read, server_write, shutdown)`.
//!
//! Same observable contracts as `http_conformance_test` must hold,
//! with two stdio-specific adjustments:
//!   * `shutdown` → server closes write half → client EOF (not 404)
//!   * notifications → no response line written (not 202 + empty body)
//!
//! Gated behind `--features server-conformance`.

#![cfg(feature = "server-conformance")]
#![allow(clippy::all)]

mod conformance_shared;

use std::time::Duration;

use nexo_mcp::run_with_io;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use conformance_shared::ConformanceHandler;

// ---------------------------------------------------------------------------
// Stdio test client helper
// ---------------------------------------------------------------------------

struct StdioClient {
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    reader: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
}

impl StdioClient {
    async fn send_raw(&mut self, msg: &Value) {
        let mut line = serde_json::to_string(msg).unwrap();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.unwrap();
    }

    // Send a request and read one response line. Panics on timeout.
    async fn rpc(&mut self, method: &str, params: Value, id: i64) -> Value {
        self.send_raw(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id
        }))
        .await;
        self.read_line(Duration::from_secs(5))
            .await
            .expect("expected response line, got EOF/timeout")
    }

    // Send notification (no id) — server must NOT write a response.
    async fn notify(&mut self, method: &str, params: Value) {
        self.send_raw(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .await;
    }

    // Try to read one response line with a timeout.
    // Returns None on EOF or timeout.
    async fn read_line(&mut self, timeout: Duration) -> Option<Value> {
        let mut line = String::new();
        let result = tokio::time::timeout(timeout, self.reader.read_line(&mut line)).await;
        match result {
            Ok(Ok(0)) | Err(_) => None, // EOF or timeout
            Ok(Ok(_)) => serde_json::from_str(line.trim()).ok(),
            Ok(Err(_)) => None,
        }
    }
}

async fn boot() -> (StdioClient, CancellationToken) {
    let (client_half, server_half) = tokio::io::duplex(65536);
    let (server_read, server_write) = tokio::io::split(server_half);
    let (client_read, client_write) = tokio::io::split(client_half);
    let shutdown = CancellationToken::new();
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        let _ = run_with_io(
            ConformanceHandler,
            server_read,
            server_write,
            shutdown_clone,
        )
        .await;
    });
    let client = StdioClient {
        writer: client_write,
        reader: BufReader::new(client_read),
    };
    (client, shutdown)
}

// Perform the mandatory initialize handshake and return the result body.
async fn initialize(client: &mut StdioClient) -> Value {
    client
        .rpc(
            "initialize",
            serde_json::json!({"protocolVersion": "2025-11-25"}),
            1,
        )
        .await
}

// ---------------------------------------------------------------------------
// Conformance tests (same semantics as http_conformance_test.rs)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_echoes_supported_protocol_version() {
    let (mut client, shutdown) = boot().await;
    let resp = initialize(&mut client).await;
    assert_eq!(resp["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(resp["result"]["serverInfo"]["name"], "mock");
    shutdown.cancel();
}

#[tokio::test]
async fn initialize_falls_back_when_client_version_unknown() {
    let (mut client, shutdown) = boot().await;
    let resp = client
        .rpc(
            "initialize",
            serde_json::json!({"protocolVersion": "9999-99-99"}),
            1,
        )
        .await;
    let agreed = resp["result"]["protocolVersion"].as_str().unwrap();
    assert_ne!(agreed, "9999-99-99");
    shutdown.cancel();
}

#[tokio::test]
async fn tools_list_omits_next_cursor_when_no_pagination() {
    let (mut client, shutdown) = boot().await;
    initialize(&mut client).await;
    let resp = client.rpc("tools/list", Value::Null, 2).await;
    assert!(resp["result"]["tools"].is_array());
    assert!(
        resp["result"].get("nextCursor").is_none(),
        "tools/list must not emit nextCursor; got {resp}"
    );
    shutdown.cancel();
}

#[tokio::test]
async fn tools_call_returns_content_and_is_error_false() {
    let (mut client, shutdown) = boot().await;
    initialize(&mut client).await;
    let resp = client
        .rpc(
            "tools/call",
            serde_json::json!({"name": "ping", "arguments": {}}),
            2,
        )
        .await;
    assert_eq!(resp["result"]["content"][0]["text"], "called ping");
    assert_eq!(resp["result"]["isError"], false);
    shutdown.cancel();
}

#[tokio::test]
async fn unknown_tool_returns_protocol_error() {
    let (mut client, shutdown) = boot().await;
    initialize(&mut client).await;
    let resp = client
        .rpc(
            "tools/call",
            serde_json::json!({"name": "ghost", "arguments": {}}),
            3,
        )
        .await;
    assert_eq!(resp["error"]["code"], -32602);
    shutdown.cancel();
}

#[tokio::test]
async fn unknown_method_returns_method_not_found() {
    let (mut client, shutdown) = boot().await;
    initialize(&mut client).await;
    let resp = client.rpc("mystery", Value::Null, 9).await;
    assert_eq!(resp["error"]["code"], -32601);
    shutdown.cancel();
}

#[tokio::test]
async fn shutdown_closes_server_write_half() {
    // HTTP: server returns null result then 404 on subsequent requests.
    // Stdio: server sends null result then closes write half (EOF).
    let (mut client, _shutdown) = boot().await;
    initialize(&mut client).await;
    let resp = client.rpc("shutdown", Value::Null, 11).await;
    assert_eq!(resp["id"], 11);
    assert_eq!(resp["result"], Value::Null);

    // After shutdown the server closes its write half — reading again
    // returns EOF (None) rather than a valid frame.
    let after = client.read_line(Duration::from_millis(500)).await;
    assert!(
        after.is_none(),
        "expected EOF after shutdown, got: {after:?}"
    );
}

#[tokio::test]
async fn completion_complete_returns_empty_values() {
    let (mut client, shutdown) = boot().await;
    initialize(&mut client).await;
    let resp = client
        .rpc("completion/complete", serde_json::json!({}), 14)
        .await;
    assert_eq!(
        resp["result"]["completion"]["values"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(99),
        0
    );
    shutdown.cancel();
}

#[tokio::test]
async fn resources_list_and_read_roundtrip() {
    let (mut client, shutdown) = boot().await;
    initialize(&mut client).await;
    let list = client.rpc("resources/list", Value::Null, 15).await;
    assert_eq!(
        list["result"]["resources"][0]["uri"],
        "agent://workspace/soul"
    );

    let read = client
        .rpc(
            "resources/read",
            serde_json::json!({"uri": "agent://workspace/soul"}),
            16,
        )
        .await;
    assert_eq!(
        read["result"]["contents"][0]["text"],
        "contents agent://workspace/soul"
    );
    shutdown.cancel();
}

#[tokio::test]
async fn prompts_list_and_get_roundtrip() {
    let (mut client, shutdown) = boot().await;
    initialize(&mut client).await;
    let list = client.rpc("prompts/list", Value::Null, 17).await;
    assert_eq!(list["result"]["prompts"][0]["name"], "workspace_soul");

    let get = client
        .rpc(
            "prompts/get",
            serde_json::json!({"name": "workspace_soul", "arguments": {}}),
            18,
        )
        .await;
    assert_eq!(
        get["result"]["messages"][0]["content"]["text"],
        "run workspace_soul"
    );
    shutdown.cancel();
}

#[tokio::test]
async fn notification_produces_no_response_line() {
    // HTTP: server returns 202 with empty body.
    // Stdio: server must NOT write any line for a notification.
    let (mut client, shutdown) = boot().await;
    initialize(&mut client).await;
    client
        .notify("notifications/initialized", Value::Null)
        .await;
    // Give the server a moment to process, then verify no line arrives.
    let response = client.read_line(Duration::from_millis(200)).await;
    assert!(
        response.is_none(),
        "notifications must not produce a response line; got: {response:?}"
    );
    shutdown.cancel();
}
