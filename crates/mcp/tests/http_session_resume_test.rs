#![allow(clippy::all)]

//! Phase 76.8 — end-to-end resume tests for the HTTP transport.
//! Boots a real server with a configured `SessionEventStore`,
//! initializes a session, fires `notifications/tools/list_changed`
//! to seed the durable trail, then asserts the SSE consumer's
//! `Last-Event-ID` reconnect surfaces the right gap.
//!
//! Wire contract under test (matches the leak):
//!   * `claude-code-leak/src/cli/transports/SSETransport.ts:159-266`
//!     — SSE frame `id: <seq>` + `Last-Event-ID: <seq>` reconnect.
//!   * `claude-code-leak/src/services/mcp/client.ts:189-206` —
//!     unknown session → HTTP 404 + JSON-RPC `-32001`.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use nexo_mcp::server::event_store::SessionEventStoreConfig;
use nexo_mcp::server::http_config::HttpTransportConfig;
use nexo_mcp::types::{McpServerInfo, McpTool, McpToolResult};
use nexo_mcp::{start_http_server, HttpServerHandle, McpError, McpServerHandler};
use reqwest::Client;
use serde_json::Value;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct NoopHandler;

#[async_trait]
impl McpServerHandler for NoopHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "resume-test".into(),
            version: "0.0.1".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![])
    }
    async fn call_tool(&self, _: &str, _: Value) -> Result<McpToolResult, McpError> {
        Ok(McpToolResult {
            content: vec![],
            is_error: false,
            structured_content: None,
        })
    }
}

fn cfg_with_store(db_path: std::path::PathBuf, max_replay_batch: usize) -> HttpTransportConfig {
    let mut c = HttpTransportConfig::default();
    c.enabled = true;
    c.bind = "127.0.0.1:0".parse().unwrap();
    c.session_event_store = Some(SessionEventStoreConfig {
        enabled: true,
        db_path,
        max_events_per_session: 10_000,
        max_replay_batch,
        purge_interval_secs: 60,
    });
    c
}

async fn boot(cfg: HttpTransportConfig) -> (HttpServerHandle, Client, CancellationToken) {
    let shutdown = CancellationToken::new();
    let handle = start_http_server(NoopHandler, cfg, shutdown.clone())
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

/// Drain `n` `event: message` frames from the SSE stream — skips
/// non-message frames (lagged, end, shutdown). Returns each frame's
/// `id` field plus its raw `data` body.
async fn collect_messages(resp: reqwest::Response, n: usize) -> Vec<(String, String)> {
    let mut events = eventsource_stream::EventStream::new(resp.bytes_stream());
    let mut out = Vec::new();
    while out.len() < n {
        let next = tokio::time::timeout(Duration::from_secs(3), events.next()).await;
        let Ok(Some(Ok(evt))) = next else {
            break;
        };
        if evt.event == "message" {
            out.push((evt.id.clone(), evt.data.clone()));
        }
    }
    out
}

#[tokio::test]
async fn unknown_session_returns_404_with_jsonrpc_error_minus_32001() {
    let dir = tempdir().unwrap();
    let cfg = cfg_with_store(dir.path().join("sessions.db"), 1_000);
    let (handle, client, shut) = boot(cfg).await;
    let url = format!("http://{}/mcp", handle.bind_addr);
    let resp = client
        .get(&url)
        .header("Mcp-Session-Id", "deadbeef-not-a-real-session")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["error"]["code"], -32001);
    assert_eq!(body["error"]["message"], "Session not found");
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn last_event_id_zero_streams_live_with_seq_labels() {
    let dir = tempdir().unwrap();
    let cfg = cfg_with_store(dir.path().join("sessions.db"), 1_000);
    let (handle, client, shut) = boot(cfg).await;
    let session_id = initialize(&client, handle.bind_addr).await;

    // Open SSE consumer first so the broadcast is visible.
    let url = format!("http://{}/mcp", handle.bind_addr);
    let resp = client
        .get(&url)
        .header("Mcp-Session-Id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    // Tiny pause so the SSE handler has subscribed to the broadcast.
    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.notify_tools_list_changed();
    handle.notify_tools_list_changed();

    let frames = collect_messages(resp, 2).await;
    assert_eq!(frames.len(), 2, "expected 2 indexed frames");
    assert_eq!(frames[0].0, "1", "first frame must carry seq id 1");
    assert_eq!(frames[1].0, "2", "second frame must carry seq id 2");
    assert!(frames[0].1.contains("tools/list_changed"));
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn last_event_id_n_replays_only_after_n() {
    let dir = tempdir().unwrap();
    let cfg = cfg_with_store(dir.path().join("sessions.db"), 1_000);
    let (handle, client, shut) = boot(cfg).await;
    let session_id = initialize(&client, handle.bind_addr).await;

    // Seed 3 events while no SSE consumer is attached. Persist
    // is fire-and-forget — give the writer a beat to land them.
    handle.notify_tools_list_changed();
    handle.notify_tools_list_changed();
    handle.notify_tools_list_changed();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Reconnect with `Last-Event-ID: 1` — expect frames 2 and 3.
    let url = format!("http://{}/mcp", handle.bind_addr);
    let resp = client
        .get(&url)
        .header("Mcp-Session-Id", &session_id)
        .header("Last-Event-ID", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let frames = collect_messages(resp, 2).await;
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].0, "2");
    assert_eq!(frames[1].0, "3");
    shutdown(handle, shut).await;
}

#[tokio::test]
async fn last_event_id_replay_respects_max_replay_batch() {
    let dir = tempdir().unwrap();
    let cfg = cfg_with_store(dir.path().join("sessions.db"), 2);
    let (handle, client, shut) = boot(cfg).await;
    let session_id = initialize(&client, handle.bind_addr).await;

    // Seed 5 events, reconnect from 0 — replay path should yield
    // exactly 2 (`max_replay_batch`), then transition to the live
    // loop.
    for _ in 0..5 {
        handle.notify_tools_list_changed();
    }
    tokio::time::sleep(Duration::from_millis(150)).await;

    let url = format!("http://{}/mcp", handle.bind_addr);
    let resp = client
        .get(&url)
        .header("Mcp-Session-Id", &session_id)
        .header("Last-Event-ID", "0")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let mut events = eventsource_stream::EventStream::new(resp.bytes_stream());
    let mut replay_seqs: Vec<String> = Vec::new();
    // Read up to ~250ms — anything after that is live, not replay.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let next = tokio::time::timeout(remaining, events.next()).await;
        let Ok(Some(Ok(evt))) = next else {
            break;
        };
        if evt.event == "message" {
            replay_seqs.push(evt.id);
        }
    }
    assert_eq!(
        replay_seqs.len(),
        2,
        "max_replay_batch=2 should cap the initial drain at 2 frames, got {replay_seqs:?}"
    );
    assert_eq!(replay_seqs[0], "1");
    assert_eq!(replay_seqs[1], "2");

    // Discard handle/shutdown — eventsource borrows resp.
    drop(events);
    shutdown(handle, shut).await;
}
