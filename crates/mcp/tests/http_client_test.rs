//! Phase 12.2 — integration tests against a minimal axum-based MCP server.
//!
//! Scope: cover streamable-http happy path (initialize, list_tools,
//! call_tool), JSON-RPC error mapping, and SSE legacy bootstrap.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use nexo_mcp::{HttpMcpClient, HttpMcpOptions, HttpTransportMode, McpError};
use reqwest::header::HeaderMap;
use tokio::net::TcpListener;
use url::Url;

#[derive(Clone, Default)]
struct ServerState {
    fail_call: bool,
}

async fn streamable_handler(
    State(state): State<ServerState>,
    body: String,
) -> axum::response::Response {
    let req: serde_json::Value = match serde_json::from_str(body.trim()) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid json").into_response(),
    };
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned();

    // Notifications have no id — return 200 empty.
    let Some(id_value) = id else {
        return (StatusCode::OK, Body::empty()).into_response();
    };

    let result = match method {
        "initialize" => serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name":"axum-mock","version":"0.1.0"}
        }),
        "tools/list" => serde_json::json!({
            "tools": [
                {"name":"echo","inputSchema":{"type":"object"}},
                {"name":"ping","inputSchema":{"type":"object"}}
            ]
        }),
        "tools/call" => {
            if state.fail_call {
                let err = serde_json::json!({
                    "jsonrpc":"2.0",
                    "error": {"code": -32002, "message": "denied"},
                    "id": id_value
                });
                let mut response = (StatusCode::OK, Json(err)).into_response();
                response.headers_mut().insert(
                    reqwest::header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                return response;
            }
            serde_json::json!({
                "content":[{"type":"text","text":"ok"}],
                "isError": false
            })
        }
        _ => {
            let err = serde_json::json!({
                "jsonrpc":"2.0",
                "error": {"code": -32601, "message": "method not found"},
                "id": id_value
            });
            return (StatusCode::OK, Json(err)).into_response();
        }
    };
    let response_json = serde_json::json!({
        "jsonrpc":"2.0",
        "result": result,
        "id": id_value,
    });
    let mut response = (StatusCode::OK, Json(response_json)).into_response();
    response.headers_mut().insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
        .headers_mut()
        .insert("mcp-session-id", HeaderValue::from_static("sess-1"));
    response
}

async fn spawn_streamable_server(state: ServerState) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/", post(streamable_handler))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, handle)
}

#[tokio::test]
async fn streamable_connect_and_list_tools() {
    let (addr, server) = spawn_streamable_server(ServerState::default()).await;
    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let client = HttpMcpClient::connect(
        "axum-http".into(),
        url,
        HttpTransportMode::StreamableHttp,
        HeaderMap::new(),
        HttpMcpOptions {
            initialize_timeout: Duration::from_secs(2),
            call_timeout: Duration::from_secs(2),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    assert_eq!(client.server_info().name, "axum-mock");
    let tools = client.list_tools().await.expect("list");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"ping"));

    let result = client
        .call_tool("echo", serde_json::json!({"x":1}))
        .await
        .expect("call");
    assert!(!result.is_error);

    client.shutdown().await;
    server.abort();
}

#[derive(Clone, Default)]
struct SlowState {
    cancelled: Arc<tokio::sync::Mutex<Vec<u64>>>,
}

async fn slow_streamable_handler(
    State(state): State<SlowState>,
    body: String,
) -> axum::response::Response {
    let req: serde_json::Value = match serde_json::from_str(body.trim()) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "bad json").into_response(),
    };
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned();

    if id.is_none() {
        if method == "notifications/cancelled" {
            if let Some(rid) = req
                .get("params")
                .and_then(|p| p.get("requestId"))
                .and_then(|v| v.as_u64())
            {
                state.cancelled.lock().await.push(rid);
            }
        }
        return (StatusCode::OK, Body::empty()).into_response();
    }
    let id_value = id.unwrap();

    let result = match method {
        "initialize" => serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name":"slow-mock","version":"0.1.0"}
        }),
        "tools/call" => {
            tokio::time::sleep(Duration::from_millis(500)).await;
            serde_json::json!({
                "content":[{"type":"text","text":"slow ok"}],
                "isError": false
            })
        }
        _ => {
            let err = serde_json::json!({
                "jsonrpc":"2.0",
                "error": {"code": -32601, "message": "method not found"},
                "id": id_value
            });
            return (StatusCode::OK, Json(err)).into_response();
        }
    };
    let response_json = serde_json::json!({
        "jsonrpc":"2.0",
        "result": result,
        "id": id_value
    });
    let mut response = (StatusCode::OK, Json(response_json)).into_response();
    response.headers_mut().insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

#[tokio::test]
async fn shutdown_sends_cancelled_streamable() {
    let state = SlowState::default();
    let app = Router::new()
        .route("/", post(slow_streamable_handler))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let client = Arc::new(
        HttpMcpClient::connect(
            "slow-http".into(),
            url,
            HttpTransportMode::StreamableHttp,
            HeaderMap::new(),
            HttpMcpOptions {
                initialize_timeout: Duration::from_secs(2),
                call_timeout: Duration::from_secs(2),
                ..Default::default()
            },
        )
        .await
        .expect("connect"),
    );

    // Fire a tools/call that will block on the server for 500ms.
    let call = {
        let client = client.clone();
        tokio::spawn(async move {
            let _ = client.call_tool("anything", serde_json::json!({})).await;
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    client.shutdown().await;
    let _ = call.await;

    // Give the server a moment to log the cancelled notif.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let recorded = state.cancelled.lock().await.clone();
    assert!(!recorded.is_empty(), "no cancelled recorded");
    assert!(
        recorded.iter().any(|&id| id > 0),
        "recorded ids: {recorded:?}"
    );
    server.abort();
}

#[tokio::test]
async fn streamable_server_error_maps_to_server_error() {
    let (addr, server) = spawn_streamable_server(ServerState { fail_call: true }).await;
    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let client = HttpMcpClient::connect(
        "axum-err".into(),
        url,
        HttpTransportMode::StreamableHttp,
        HeaderMap::new(),
        HttpMcpOptions {
            initialize_timeout: Duration::from_secs(2),
            call_timeout: Duration::from_secs(2),
            ..Default::default()
        },
    )
    .await
    .expect("connect");
    let err = client
        .call_tool("echo", serde_json::json!({}))
        .await
        .unwrap_err();
    match err {
        McpError::ServerError { code, message } => {
            assert_eq!(code, -32002);
            assert!(message.contains("denied"));
        }
        other => panic!("unexpected: {other:?}"),
    }
    client.shutdown().await;
    server.abort();
}

// ─── SSE legacy ──────────────────────────────────────────────────────────────

async fn sse_get_handler(
    State(state): State<Arc<tokio::sync::Mutex<Option<String>>>>,
) -> axum::response::Response {
    use axum::response::sse::{Event, Sse};
    use futures_util::stream;

    // Publish a single `endpoint` event; the post handler will dispatch responses
    // via a shared mpsc since SSE can't write back on its own trivially.
    // For test simplicity we include the initialize response inline after
    // a 50ms delay, matching what the client POSTs.
    let post_url = "/sse/message".to_string();
    let messages: Vec<Result<Event, std::convert::Infallible>> = vec![Ok(Event::default()
        .event("endpoint")
        .data(post_url.clone()))];
    *state.lock().await = Some(post_url);
    let stream = stream::iter(messages);
    Sse::new(stream).into_response()
}

#[tokio::test]
#[ignore] // SSE legacy test kept for future reference; current mock omits live response stream correlation.
async fn sse_connect_receives_endpoint_event() {
    let state: Arc<tokio::sync::Mutex<Option<String>>> = Arc::new(tokio::sync::Mutex::new(None));
    let app = Router::new()
        .route("/sse", get(sse_get_handler))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Because this test server lacks the response-push plumbing, only
    // verify that the endpoint event is received. A full SSE round-trip is
    // validated by the unit-level `sse::tests::*` suite.
    let state_after = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            if state.lock().await.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    drop(state_after);

    // Actually attempt the handshake; expect timeout because no response stream.
    let url = Url::parse(&format!("http://{addr}/sse")).unwrap();
    let result = HttpMcpClient::connect(
        "sse".into(),
        url,
        HttpTransportMode::Sse,
        HeaderMap::new(),
        HttpMcpOptions {
            initialize_timeout: Duration::from_millis(200),
            ..Default::default()
        },
    )
    .await;
    assert!(
        matches!(result, Err(McpError::InitializeTimeout(_))),
        "expected initialize timeout because mock lacks response plumbing"
    );
    server.abort();
}

// ──────────────────────────────────────────────────────────────────────
// Phase 12.2 follow-up — Mcp-Session-Id mid-call invalidation (404 → retry).
// ──────────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct SessionRotatingState {
    // Count of tools/call requests. First one carries stale session and
    // must 404; second one must arrive without Mcp-Session-Id.
    tool_calls_with_stale: Arc<std::sync::atomic::AtomicU32>,
    tool_calls_without_session: Arc<std::sync::atomic::AtomicU32>,
    retried_ok: Arc<std::sync::atomic::AtomicBool>,
}

async fn session_rotating_handler(
    State(state): State<SessionRotatingState>,
    headers: axum::http::HeaderMap,
    body: String,
) -> axum::response::Response {
    let req: serde_json::Value = match serde_json::from_str(body.trim()) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "bad json").into_response(),
    };
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let Some(id_value) = req.get("id").cloned() else {
        return (StatusCode::OK, Body::empty()).into_response();
    };

    let has_stale_session = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s == "sess-stale")
        .unwrap_or(false);

    if method == "tools/call" && has_stale_session {
        state
            .tool_calls_with_stale
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }

    if method == "tools/call" && !has_stale_session {
        state
            .tool_calls_without_session
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        state
            .retried_ok
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    let result = match method {
        "initialize" => serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name":"rotating-mock","version":"0.1.0"}
        }),
        "tools/list" => serde_json::json!({ "tools": [] }),
        "tools/call" => serde_json::json!({
            "content":[{"type":"text","text":"ok-after-retry"}],
            "isError": false
        }),
        _ => {
            let err = serde_json::json!({
                "jsonrpc":"2.0",
                "error": {"code": -32601, "message": "method not found"},
                "id": id_value
            });
            return (StatusCode::OK, Json(err)).into_response();
        }
    };
    let response_json = serde_json::json!({
        "jsonrpc":"2.0",
        "result": result,
        "id": id_value,
    });
    let mut response = (StatusCode::OK, Json(response_json)).into_response();
    response.headers_mut().insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    // `initialize` seeds the "stale" session id; subsequent retry-without
    // gets a fresh one. We always emit the header so the client stores it.
    let next_sid = if method == "initialize" {
        "sess-stale"
    } else {
        "sess-fresh"
    };
    response
        .headers_mut()
        .insert("mcp-session-id", HeaderValue::from_static(next_sid));
    response
}

#[tokio::test]
async fn streamable_retries_without_session_on_404() {
    let state = SessionRotatingState::default();
    let app = Router::new()
        .route("/", post(session_rotating_handler))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let client = HttpMcpClient::connect(
        "rot-http".into(),
        url,
        HttpTransportMode::StreamableHttp,
        HeaderMap::new(),
        HttpMcpOptions {
            initialize_timeout: Duration::from_secs(2),
            call_timeout: Duration::from_secs(2),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    // First tools/call: client sends stale session → 404 → client drops
    // session and retries once → second attempt succeeds.
    let result = client
        .call_tool("x", serde_json::json!({}))
        .await
        .expect("call recovers");
    assert!(!result.is_error);
    match &result.content[0] {
        nexo_mcp::McpContent::Text { text } => assert_eq!(text, "ok-after-retry"),
        _ => panic!("expected text content"),
    }
    assert_eq!(
        state
            .tool_calls_with_stale
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "first tools/call must carry the stale session"
    );
    assert_eq!(
        state
            .tool_calls_without_session
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "retry must arrive without Mcp-Session-Id"
    );
    assert!(state.retried_ok.load(std::sync::atomic::Ordering::SeqCst));

    client.shutdown().await;
    server.abort();
}

#[tokio::test]
async fn streamable_does_not_retry_404_when_no_session() {
    // No-session 404 must surface as HttpStatus, not silently retry.
    let app = Router::new().route(
        "/",
        post(|body: String| async move {
            // Always 404 — also on initialize → connect should fail.
            let _ = body;
            (StatusCode::NOT_FOUND, "nope").into_response()
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let err = match HttpMcpClient::connect(
        "nose-http".into(),
        url,
        HttpTransportMode::StreamableHttp,
        HeaderMap::new(),
        HttpMcpOptions {
            initialize_timeout: Duration::from_secs(1),
            call_timeout: Duration::from_secs(1),
            ..Default::default()
        },
    )
    .await
    {
        Err(e) => e,
        Ok(_) => panic!("initialize must fail"),
    };
    // Accept either HttpStatus directly or a wrapping InitializeFailed.
    let s = format!("{err:?}");
    assert!(
        s.contains("HttpStatus") || s.contains("404") || s.contains("Initialize"),
        "expected http 404 surface, got {s}"
    );
    server.abort();
}

// ──────────────────────────────────────────────────────────────────────
// Phase 12.2 follow-up — `transport: auto` fallback from streamable → sse
// ──────────────────────────────────────────────────────────────────────

async fn auto_fallback_streamable_handler(_body: String) -> axum::response::Response {
    // Mimic an SSE-only server rejecting the streamable POST.
    (StatusCode::METHOD_NOT_ALLOWED, "streamable not supported").into_response()
}

async fn auto_fallback_sse_get_handler() -> axum::response::Response {
    use axum::response::sse::{Event, Sse};
    use futures_util::stream;

    let events = vec![
        Ok::<_, std::convert::Infallible>(Event::default().event("endpoint").data("/sse/messages")),
        Ok::<_, std::convert::Infallible>(
            Event::default().event("message").data(
                serde_json::json!({
                    "jsonrpc":"2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion":"2024-11-05",
                        "capabilities": {"tools":{}},
                        "serverInfo": {"name":"auto-sse-mock","version":"0.1.0"}
                    }
                })
                .to_string(),
            ),
        ),
    ];
    Sse::new(stream::iter(events))
        .keep_alive(axum::response::sse::KeepAlive::new())
        .into_response()
}

async fn auto_fallback_sse_post_handler(_body: String) -> axum::response::Response {
    (StatusCode::ACCEPTED, Body::empty()).into_response()
}

#[tokio::test]
#[ignore = "SSE axum mock plumbing incomplete — see ignored sse_connect_receives_endpoint_event for the shared limitation"]
async fn auto_falls_back_to_sse_on_405() {
    // Root accepts GET (SSE) and POST (streamable returns 405).
    let app = Router::new()
        .route(
            "/",
            get(auto_fallback_sse_get_handler).post(auto_fallback_streamable_handler),
        )
        .route("/sse/messages", post(auto_fallback_sse_post_handler));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let client = HttpMcpClient::connect(
        "auto-http".into(),
        url,
        HttpTransportMode::Auto,
        HeaderMap::new(),
        HttpMcpOptions {
            initialize_timeout: Duration::from_secs(2),
            call_timeout: Duration::from_secs(2),
            ..Default::default()
        },
    )
    .await
    .expect("auto fallback must succeed over sse");

    assert_eq!(client.transport(), HttpTransportMode::Sse);
    assert_eq!(client.server_info().name, "auto-sse-mock");
    client.shutdown().await;
    server.abort();
}

#[tokio::test]
async fn auto_picks_streamable_when_available() {
    // Same handler as the happy path — accepts streamable POST.
    let (addr, server) = spawn_streamable_server(ServerState::default()).await;
    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let client = HttpMcpClient::connect(
        "auto-stream".into(),
        url,
        HttpTransportMode::Auto,
        HeaderMap::new(),
        HttpMcpOptions {
            initialize_timeout: Duration::from_secs(2),
            call_timeout: Duration::from_secs(2),
            ..Default::default()
        },
    )
    .await
    .expect("auto must succeed on streamable");

    assert_eq!(client.transport(), HttpTransportMode::StreamableHttp);
    assert_eq!(client.server_info().name, "axum-mock");
    client.shutdown().await;
    server.abort();
}

#[tokio::test]
async fn auto_surfaces_non_fallback_errors() {
    // Always returns 500 — must NOT trigger fallback (not in 404/405/415 set).
    let app =
        Router::new().route(
            "/",
            post(|_body: String| async move {
                (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let err = match HttpMcpClient::connect(
        "auto-500".into(),
        url,
        HttpTransportMode::Auto,
        HeaderMap::new(),
        HttpMcpOptions {
            initialize_timeout: Duration::from_secs(1),
            call_timeout: Duration::from_secs(1),
            ..Default::default()
        },
    )
    .await
    {
        Err(e) => e,
        Ok(_) => panic!("must not connect"),
    };
    let s = format!("{err:?}");
    assert!(s.contains("500") || s.contains("HttpStatus"), "got {s}");
    server.abort();
}
