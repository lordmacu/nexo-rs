#![allow(clippy::all)] // Phase 76 scaffolding — re-enable when 76.x fully shipped

//! Phase 76.1 — replay of the Phase 12.6 / 73 / 74 conformance
//! cases against the HTTP transport. Stdio path keeps its inline
//! tests in `crates/mcp/src/server/stdio.rs::tests`; the same
//! observable contracts must hold over HTTP.
//!
//! Phase 76.12 — `ConformanceHandler` moved to `conformance_shared`
//! so `stdio_conformance_test` can reuse it without duplication.

mod conformance_shared;

use std::time::Duration;

use nexo_mcp::server::http_config::HttpTransportConfig;
use nexo_mcp::{start_http_server, HttpServerHandle};
use reqwest::Client;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use conformance_shared::ConformanceHandler;

fn loopback_cfg() -> HttpTransportConfig {
    let mut c = HttpTransportConfig::default();
    c.enabled = true;
    c.bind = "127.0.0.1:0".parse().unwrap();
    c
}

async fn boot() -> (HttpServerHandle, Client, CancellationToken) {
    let shutdown = CancellationToken::new();
    let handle = start_http_server(ConformanceHandler, loopback_cfg(), shutdown.clone())
        .await
        .unwrap();
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    (handle, client, shutdown)
}

async fn shut(handle: HttpServerHandle, t: CancellationToken) {
    t.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), handle.join).await;
}

async fn initialize(client: &Client, addr: std::net::SocketAddr) -> (String, Value) {
    let url = format!("http://{}/mcp", addr);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"2025-11-25"},"id":1
        }))
        .send()
        .await
        .unwrap();
    let session = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    let body: Value = resp.json().await.unwrap();
    (session, body)
}

async fn rpc(
    client: &Client,
    addr: std::net::SocketAddr,
    session: &str,
    method: &str,
    params: Value,
    id: i64,
) -> Value {
    let url = format!("http://{}/mcp", addr);
    let resp = client
        .post(&url)
        .header("mcp-session-id", session)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":method,"params":params,"id":id
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    resp.json().await.unwrap()
}

#[tokio::test]
async fn initialize_echoes_supported_protocol_version() {
    let (handle, client, t) = boot().await;
    let (_, init) = initialize(&client, handle.bind_addr).await;
    // Phase 73.4 — server echoes the client's requested version
    // when supported.
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(init["result"]["serverInfo"]["name"], "mock");
    shut(handle, t).await;
}

#[tokio::test]
async fn initialize_falls_back_when_client_version_unknown() {
    let (handle, client, t) = boot().await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"initialize",
            "params":{"protocolVersion":"9999-99-99"},"id":1
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let agreed = body["result"]["protocolVersion"].as_str().expect("string");
    // Falls back to a known (server-supported) version, NOT the
    // unknown client one.
    assert_ne!(agreed, "9999-99-99");
    shut(handle, t).await;
}

#[tokio::test]
async fn tools_list_omits_next_cursor_when_no_pagination() {
    // Phase 73.5 — `nextCursor` MUST NOT appear when there is no
    // next page (Claude Code 2.1 schema validator rejects null).
    let (handle, client, t) = boot().await;
    let (session, _) = initialize(&client, handle.bind_addr).await;
    let body = rpc(
        &client,
        handle.bind_addr,
        &session,
        "tools/list",
        Value::Null,
        2,
    )
    .await;
    assert!(body["result"]["tools"].is_array());
    assert!(
        body["result"].get("nextCursor").is_none(),
        "tools/list must not emit nextCursor when there is no next page; got {body}"
    );
    shut(handle, t).await;
}

#[tokio::test]
async fn tools_call_returns_content_and_is_error_false() {
    let (handle, client, t) = boot().await;
    let (session, _) = initialize(&client, handle.bind_addr).await;
    let body = rpc(
        &client,
        handle.bind_addr,
        &session,
        "tools/call",
        serde_json::json!({"name":"ping","arguments":{}}),
        2,
    )
    .await;
    assert_eq!(body["result"]["content"][0]["text"], "called ping");
    assert_eq!(body["result"]["isError"], false);
    shut(handle, t).await;
}

#[tokio::test]
async fn unknown_tool_returns_protocol_error() {
    let (handle, client, t) = boot().await;
    let (session, _) = initialize(&client, handle.bind_addr).await;
    let body = rpc(
        &client,
        handle.bind_addr,
        &session,
        "tools/call",
        serde_json::json!({"name":"ghost","arguments":{}}),
        3,
    )
    .await;
    assert_eq!(body["error"]["code"], -32602);
    shut(handle, t).await;
}

#[tokio::test]
async fn unknown_method_returns_method_not_found() {
    let (handle, client, t) = boot().await;
    let (session, _) = initialize(&client, handle.bind_addr).await;
    let body = rpc(
        &client,
        handle.bind_addr,
        &session,
        "mystery",
        Value::Null,
        9,
    )
    .await;
    assert_eq!(body["error"]["code"], -32601);
    shut(handle, t).await;
}

#[tokio::test]
async fn shutdown_replies_then_closes_session() {
    let (handle, client, t) = boot().await;
    let (session, _) = initialize(&client, handle.bind_addr).await;
    let body = rpc(
        &client,
        handle.bind_addr,
        &session,
        "shutdown",
        Value::Null,
        11,
    )
    .await;
    assert_eq!(body["id"], 11);
    assert_eq!(body["result"], Value::Null);
    // Session is closed after shutdown reply.
    let url = format!("http://{}/mcp", handle.bind_addr);
    let after = client
        .post(&url)
        .header("mcp-session-id", &session)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"tools/list","id":12
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(after.status().as_u16(), 404);
    shut(handle, t).await;
}

#[tokio::test]
async fn completion_complete_returns_empty_values() {
    let (handle, client, t) = boot().await;
    let (session, _) = initialize(&client, handle.bind_addr).await;
    let body = rpc(
        &client,
        handle.bind_addr,
        &session,
        "completion/complete",
        serde_json::json!({}),
        14,
    )
    .await;
    assert_eq!(
        body["result"]["completion"]["values"]
            .as_array()
            .map(|a| a.len())
            .unwrap(),
        0
    );
    shut(handle, t).await;
}

#[tokio::test]
async fn resources_list_and_read_roundtrip() {
    let (handle, client, t) = boot().await;
    let (session, _) = initialize(&client, handle.bind_addr).await;
    let list = rpc(
        &client,
        handle.bind_addr,
        &session,
        "resources/list",
        Value::Null,
        15,
    )
    .await;
    assert_eq!(
        list["result"]["resources"][0]["uri"],
        "agent://workspace/soul"
    );
    let read = rpc(
        &client,
        handle.bind_addr,
        &session,
        "resources/read",
        serde_json::json!({"uri":"agent://workspace/soul"}),
        16,
    )
    .await;
    assert_eq!(
        read["result"]["contents"][0]["text"],
        "contents agent://workspace/soul"
    );
    shut(handle, t).await;
}

#[tokio::test]
async fn prompts_list_and_get_roundtrip() {
    let (handle, client, t) = boot().await;
    let (session, _) = initialize(&client, handle.bind_addr).await;
    let list = rpc(
        &client,
        handle.bind_addr,
        &session,
        "prompts/list",
        Value::Null,
        17,
    )
    .await;
    assert_eq!(list["result"]["prompts"][0]["name"], "workspace_soul");
    let get = rpc(
        &client,
        handle.bind_addr,
        &session,
        "prompts/get",
        serde_json::json!({"name":"workspace_soul","arguments":{}}),
        18,
    )
    .await;
    assert_eq!(
        get["result"]["messages"][0]["content"]["text"],
        "run workspace_soul"
    );
    shut(handle, t).await;
}

#[tokio::test]
async fn notification_returns_202_no_body() {
    let (handle, client, t) = boot().await;
    let (session, _) = initialize(&client, handle.bind_addr).await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    let resp = client
        .post(&url)
        .header("mcp-session-id", &session)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"notifications/initialized"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 202);
    let body = resp.text().await.unwrap();
    assert!(body.is_empty(), "notification reply must have no body");
    shut(handle, t).await;
}
