//! Phase 76.1 — adversarial integration tests for the HTTP+SSE
//! transport. Each test boots a real server on a kernel-assigned
//! port and drives it with reqwest.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use nexo_mcp::server::http_config::{HttpTransportConfig, PerIpRateLimit};
use nexo_mcp::types::{McpContent, McpServerInfo, McpTool, McpToolResult};
use nexo_mcp::{start_http_server, HttpServerHandle, McpError, McpServerHandler};
use reqwest::Client;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct TestHandler {
    panic_first: Arc<AtomicUsize>,
}

#[async_trait]
impl McpServerHandler for TestHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "test".into(),
            version: "0.0.1".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![McpTool {
            name: "boom".into(),
            description: None,
            input_schema: serde_json::json!({"type":"object"}),
            output_schema: None,
        }])
    }
    async fn call_tool(&self, name: &str, _: Value) -> Result<McpToolResult, McpError> {
        if name == "boom" {
            let prev = self.panic_first.fetch_add(1, Ordering::SeqCst);
            if prev == 0 {
                panic!("intentional panic for robustness test");
            }
        }
        Ok(McpToolResult {
            content: vec![McpContent::Text { text: "ok".into() }],
            is_error: false,
            structured_content: None,
        })
    }
}

fn loopback_cfg() -> HttpTransportConfig {
    let mut c = HttpTransportConfig::default();
    c.enabled = true;
    c.bind = "127.0.0.1:0".parse().unwrap();
    c
}

async fn boot(cfg: HttpTransportConfig) -> (HttpServerHandle, Client, CancellationToken) {
    let handler = TestHandler {
        panic_first: Arc::new(AtomicUsize::new(0)),
    };
    let shutdown = CancellationToken::new();
    let handle = start_http_server(handler, cfg, shutdown.clone())
        .await
        .expect("server boot");
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    (handle, client, shutdown)
}

async fn shutdown(handle: HttpServerHandle, token: CancellationToken) {
    token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), handle.join).await;
}

async fn initialize(client: &Client, addr: std::net::SocketAddr) -> String {
    let url = format!("http://{}/mcp", addr);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"initialize","params":{},"id":1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    resp.headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn body_above_limit_returns_413_no_panic() {
    let mut cfg = loopback_cfg();
    cfg.body_max_bytes = 1024; // 1 KiB cap
    let (handle, client, shut) = boot(cfg).await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    let oversized = "x".repeat(10 * 1024); // 10 KiB
    let body = format!(
        r#"{{"jsonrpc":"2.0","method":"x","params":"{}","id":1}}"#,
        oversized
    );
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 413);
    // Server still alive — health check still works.
    let h = client
        .get(format!("http://{}/healthz", handle.bind_addr))
        .send()
        .await
        .unwrap();
    assert_eq!(h.status().as_u16(), 200);
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn nested_json_above_depth_64_returns_400() {
    let (handle, client, shut) = boot(loopback_cfg()).await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    let mut payload = String::new();
    for _ in 0..200 {
        payload.push('{');
        payload.push_str("\"a\":");
    }
    payload.push_str("null");
    for _ in 0..200 {
        payload.push('}');
    }
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .body(payload)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"]["message"].as_str().unwrap().contains("depth"));
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn batch_request_returns_invalid_envelope() {
    let (handle, client, shut) = boot(loopback_cfg()).await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    let resp = client
        .post(&url)
        .json(&serde_json::json!([{
            "jsonrpc":"2.0","method":"x","id":1
        }]))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"]["message"].as_str().unwrap().contains("batch"));
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn missing_session_id_on_post_returns_400() {
    let (handle, client, shut) = boot(loopback_cfg()).await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    // Non-initialize without session id.
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"tools/list","id":2
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn unknown_session_returns_404() {
    let (handle, client, shut) = boot(loopback_cfg()).await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    let resp = client
        .post(&url)
        .header("mcp-session-id", "00000000-0000-0000-0000-000000000000")
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"tools/list","id":2
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn cross_session_isolation_rejects_other_id() {
    let (handle, client, shut) = boot(loopback_cfg()).await;
    let _id_a = initialize(&client, handle.bind_addr).await;
    let _id_b = initialize(&client, handle.bind_addr).await;
    // Bogus session id is rejected even with two real ones registered.
    let url = format!("http://{}/mcp", handle.bind_addr);
    let resp = client
        .post(&url)
        .header("mcp-session-id", "11111111-1111-1111-1111-111111111111")
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"tools/list","id":3
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn handler_panic_does_not_kill_server() {
    let (handle, client, shut) = boot(loopback_cfg()).await;
    let session = initialize(&client, handle.bind_addr).await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    // First call panics → 200 with -32603 in body.
    let resp = client
        .post(&url)
        .header("mcp-session-id", &session)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"tools/call",
            "params":{"name":"boom","arguments":{}},"id":2
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32603);
    // Second call succeeds — server didn't die.
    let resp2 = client
        .post(&url)
        .header("mcp-session-id", &session)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"tools/call",
            "params":{"name":"boom","arguments":{}},"id":3
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 200);
    let body2: Value = resp2.json().await.unwrap();
    assert_eq!(body2["result"]["content"][0]["text"], "ok");
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn unauth_request_returns_401_when_token_required() {
    let mut cfg = loopback_cfg();
    cfg.auth_token = Some("secret".into());
    let (handle, client, shut) = boot(cfg).await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"initialize","params":{},"id":1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    // With token → 200.
    let resp_ok = client
        .post(&url)
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"initialize","params":{},"id":1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp_ok.status().as_u16(), 200);
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn non_loopback_bind_without_token_refuses_to_start() {
    let mut cfg = HttpTransportConfig::default();
    cfg.enabled = true;
    cfg.bind = "0.0.0.0:0".parse().unwrap();
    cfg.auth_token = None;
    let res = start_http_server(
        TestHandler {
            panic_first: Arc::new(AtomicUsize::new(0)),
        },
        cfg,
        CancellationToken::new(),
    )
    .await;
    assert!(res.is_err());
    assert_eq!(res.err().unwrap().kind(), std::io::ErrorKind::InvalidInput);
}

#[tokio::test]
async fn legacy_sse_alias_handshake_emits_endpoint() {
    let mut cfg = loopback_cfg();
    cfg.enable_legacy_sse = true;
    let (handle, client, shut) = boot(cfg).await;
    let url = format!("http://{}/sse", handle.bind_addr);
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let mut events = eventsource_stream::EventStream::new(resp.bytes_stream());
    let next = tokio::time::timeout(Duration::from_secs(2), events.next()).await;
    let evt = next.unwrap().expect("stream").expect("recv");
    // First event is the legacy `endpoint` announcement carrying
    // the absolute `/messages?sessionId=...` URL the client must
    // POST to.
    assert_eq!(evt.event, "endpoint");
    assert!(
        evt.data.contains("/messages?sessionId="),
        "endpoint data did not include the messages URL: {}",
        evt.data
    );
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn rate_limit_blocks_burst() {
    let mut cfg = loopback_cfg();
    cfg.per_ip_rate_limit = PerIpRateLimit { rps: 1, burst: 2 };
    let (handle, client, shut) = boot(cfg).await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    let mut hit_429 = false;
    for _ in 0..6 {
        let resp = client
            .post(&url)
            .json(&serde_json::json!({
                "jsonrpc":"2.0","method":"initialize","params":{},"id":1
            }))
            .send()
            .await
            .unwrap();
        if resp.status().as_u16() == 429 {
            hit_429 = true;
            break;
        }
    }
    assert!(
        hit_429,
        "rate limiter did not engage after 6 rapid requests"
    );
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn concurrent_50_sessions_round_trip_no_panic() {
    let (handle, client, shut) = boot(loopback_cfg()).await;
    let mut handles = Vec::new();
    for _ in 0..50 {
        let c = client.clone();
        let addr = handle.bind_addr;
        handles.push(tokio::spawn(async move {
            let session = initialize(&c, addr).await;
            let url = format!("http://{}/mcp", addr);
            let r = c
                .post(&url)
                .header("mcp-session-id", &session)
                .json(&serde_json::json!({
                    "jsonrpc":"2.0","method":"tools/list","id":2
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status().as_u16(), 200);
            let _ = c
                .delete(&url)
                .header("mcp-session-id", session)
                .send()
                .await;
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    // Health still 200.
    let h = client
        .get(format!("http://{}/healthz", handle.bind_addr))
        .send()
        .await
        .unwrap();
    assert_eq!(h.status().as_u16(), 200);
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn delete_idempotent_second_returns_404() {
    let (handle, client, shut) = boot(loopback_cfg()).await;
    let session = initialize(&client, handle.bind_addr).await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    let r1 = client
        .delete(&url)
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status().as_u16(), 204);
    let r2 = client
        .delete(&url)
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status().as_u16(), 404);
    shutdown(handle, shut).await;
}
