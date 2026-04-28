//! Phase 76.12 — load smoke test: 50 sessions × 200 requests = 10 000
//! total tool calls against a loopback HTTP server with no caps.
//!
//! Measures p99 request latency and fails if it exceeds 500 ms.
//! 500 ms is intentionally generous for CI environments; local
//! dev typically sees p99 < 20 ms on loopback.
//!
//! Marked `#[ignore]` so it does NOT run in the normal
//! `cargo test --features server-conformance` sweep. Run explicitly:
//!
//! ```bash
//! cargo test -p nexo-mcp --features server-conformance \
//!     -- --include-ignored load_smoke
//! ```
//!
//! Gated behind `--features server-conformance`.

#![cfg(feature = "server-conformance")]
#![allow(clippy::all)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nexo_mcp::server::http_config::{HttpTransportConfig, PerIpRateLimit};
use nexo_mcp::types::{McpContent, McpServerInfo, McpTool, McpToolResult};
use nexo_mcp::{start_http_server, McpError, McpServerHandler};
use reqwest::Client;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Minimal echo handler — returns immediately, no I/O
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct EchoHandler;

#[async_trait]
impl McpServerHandler for EchoHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "load-smoke".into(),
            version: "0.0.1".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![McpTool {
            name: "echo".into(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
        }])
    }
    async fn call_tool(&self, _: &str, _: Value) -> Result<McpToolResult, McpError> {
        Ok(McpToolResult {
            content: vec![McpContent::Text { text: "ok".into() }],
            is_error: false,
            structured_content: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn load_smoke_p99_under_500ms() {
    let mut cfg = HttpTransportConfig::default();
    cfg.enabled = true;
    cfg.bind = "127.0.0.1:0".parse().unwrap();
    // Disable all throttling — this test measures pure dispatch latency.
    cfg.per_ip_rate_limit = PerIpRateLimit {
        rps: 1_000_000,
        burst: 1_000_000,
    };
    cfg.max_in_flight = 100_000;
    cfg.max_sessions = 10_000;

    let shutdown = CancellationToken::new();
    let handle = start_http_server(EchoHandler, cfg, shutdown.clone())
        .await
        .expect("server boot");

    let addr = handle.bind_addr;
    let client = Arc::new(
        Client::builder()
            .timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(200)
            .build()
            .unwrap(),
    );

    const SESSIONS: usize = 50;
    const REQUESTS_PER_SESSION: usize = 200;

    // Phase 1 — initialize SESSIONS sessions in parallel.
    let mut session_tasks = Vec::with_capacity(SESSIONS);
    for _ in 0..SESSIONS {
        let client = Arc::clone(&client);
        session_tasks.push(tokio::spawn(async move {
            let url = format!("http://{addr}/mcp");
            let resp = client
                .post(&url)
                .json(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "initialize",
                    "params": {"protocolVersion": "2025-11-25"},
                    "id": 1
                }))
                .send()
                .await
                .expect("initialize send");
            assert_eq!(resp.status().as_u16(), 200, "initialize must return 200");
            resp.headers()
                .get("mcp-session-id")
                .expect("session-id header")
                .to_str()
                .unwrap()
                .to_string()
        }));
    }
    let mut sessions = Vec::with_capacity(SESSIONS);
    for t in session_tasks {
        sessions.push(t.await.expect("initialize task"));
    }
    assert_eq!(sessions.len(), SESSIONS);

    // Phase 2 — each session fires REQUESTS_PER_SESSION sequential calls.
    // Collect all durations.
    let all_durations: Arc<std::sync::Mutex<Vec<Duration>>> = Arc::new(std::sync::Mutex::new(
        Vec::with_capacity(SESSIONS * REQUESTS_PER_SESSION),
    ));

    let mut session_tasks = Vec::with_capacity(SESSIONS);
    for session_id in sessions {
        let client = Arc::clone(&client);
        let durations = Arc::clone(&all_durations);
        session_tasks.push(tokio::spawn(async move {
            let url = format!("http://{addr}/mcp");
            let mut local = Vec::with_capacity(REQUESTS_PER_SESSION);
            for i in 0..REQUESTS_PER_SESSION {
                let start = Instant::now();
                let resp = client
                    .post(&url)
                    .header("mcp-session-id", &session_id)
                    .json(&serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "tools/call",
                        "params": {"name": "echo", "arguments": {}},
                        "id": i as i64 + 2
                    }))
                    .send()
                    .await
                    .expect("tools/call send");
                let elapsed = start.elapsed();
                assert_eq!(resp.status().as_u16(), 200, "tools/call must return 200");
                let body: Value = resp.json().await.expect("json body");
                assert!(body.get("result").is_some(), "expected result, got: {body}");
                local.push(elapsed);
            }
            durations.lock().unwrap().extend(local);
        }));
    }
    for t in session_tasks {
        t.await.expect("session task");
    }

    // Phase 3 — compute p99 and assert.
    let mut durations = Arc::try_unwrap(all_durations)
        .unwrap()
        .into_inner()
        .unwrap();
    assert_eq!(
        durations.len(),
        SESSIONS * REQUESTS_PER_SESSION,
        "all requests must complete"
    );
    durations.sort_unstable();

    let p99_idx = (durations.len() as f64 * 0.99) as usize;
    let p99 = durations[p99_idx.min(durations.len() - 1)];
    let p50 = durations[durations.len() / 2];
    let max = durations[durations.len() - 1];

    println!(
        "load_smoke: {} requests — p50={:?} p99={:?} max={:?}",
        durations.len(),
        p50,
        p99,
        max
    );

    assert!(
        p99 < Duration::from_millis(500),
        "p99 latency {p99:?} exceeds 500ms threshold. \
         If running in a slow CI environment, increase the threshold."
    );

    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), handle.join).await;
}
