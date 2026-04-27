//! Phase 76.2 — adversarial tests for the new `Dispatcher`. Verify
//! the three robustness invariants that 76.1 (HTTP transport) will
//! depend on:
//!
//!   1. Handler panics are caught and converted to JSON-RPC -32603;
//!      the server keeps processing subsequent requests.
//!   2. Cancellation pre-dispatch yields `Cancelled` immediately.
//!   3. Cancellation mid-dispatch aborts the in-flight handler
//!      future without leaking spawned tasks.
//!
//! These tests drive `Dispatcher` directly (no transport) so the
//! same fixtures parametrize over stdio + HTTP once 76.1 lands.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nexo_mcp::server::{DispatchContext, DispatchOutcome, Dispatcher, McpServerHandler};
use nexo_mcp::types::{McpContent, McpServerInfo, McpTool, McpToolResult};
use nexo_mcp::McpError;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

struct PanickingHandler {
    panic_on_first_call: Arc<AtomicUsize>,
}

#[async_trait]
impl McpServerHandler for PanickingHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "panicker".into(),
            version: "0.1.0".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![McpTool {
            name: "boom".into(),
            description: Some("panics on first call".into()),
            input_schema: serde_json::json!({"type":"object"}),
            output_schema: None,
        }])
    }
    async fn call_tool(&self, _name: &str, _args: Value) -> Result<McpToolResult, McpError> {
        let prev = self.panic_on_first_call.fetch_add(1, Ordering::SeqCst);
        if prev == 0 {
            panic!("intentional handler panic for robustness test");
        }
        Ok(McpToolResult {
            content: vec![McpContent::Text {
                text: "second call survived".into(),
            }],
            is_error: false,
            structured_content: None,
        })
    }
}

#[tokio::test]
async fn handler_panic_is_caught_and_server_recovers() {
    let counter = Arc::new(AtomicUsize::new(0));
    let dispatcher = Dispatcher::new(
        PanickingHandler {
            panic_on_first_call: counter.clone(),
        },
        None,
    );
    let ctx = DispatchContext::empty();

    // First call panics inside the handler.
    let outcome = dispatcher
        .dispatch(
            "tools/call",
            serde_json::json!({"name": "boom", "arguments": {}}),
            &ctx,
        )
        .await;
    match outcome {
        DispatchOutcome::Error {
            code, ref message, ..
        } => {
            assert_eq!(code, -32603);
            assert!(
                message.contains("handler panicked"),
                "expected panic message, got: {message}"
            );
        }
        other => panic!("expected -32603 Error after panic, got {other:?}"),
    }

    // Second call (same dispatcher, same handler) succeeds — the
    // panic did not poison the dispatcher.
    let outcome = dispatcher
        .dispatch(
            "tools/call",
            serde_json::json!({"name": "boom", "arguments": {}}),
            &ctx,
        )
        .await;
    match outcome {
        DispatchOutcome::Reply(value) => {
            assert_eq!(value["content"][0]["text"], "second call survived");
        }
        other => panic!("expected Reply after recovery, got {other:?}"),
    }

    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

struct EchoHandler;

#[async_trait]
impl McpServerHandler for EchoHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "echo".into(),
            version: "0.1.0".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![])
    }
    async fn call_tool(&self, _name: &str, _args: Value) -> Result<McpToolResult, McpError> {
        Ok(McpToolResult {
            content: vec![McpContent::Text { text: "ok".into() }],
            is_error: false,
            structured_content: None,
        })
    }
}

#[tokio::test]
async fn cancel_before_dispatch_yields_cancelled() {
    let dispatcher = Dispatcher::new(EchoHandler, None);
    let cancel = CancellationToken::new();
    cancel.cancel(); // already cancelled before we call dispatch
    let ctx = DispatchContext {
        session_id: None,
        request_id: Some(serde_json::json!(7)),
        cancel,
        principal: None,
        progress_token: None,
        session_sink: None,
        correlation_id: None,
    };
    let outcome = dispatcher.dispatch("tools/list", Value::Null, &ctx).await;
    assert!(matches!(outcome, DispatchOutcome::Cancelled));
}

struct SlowHandler {
    started: Arc<AtomicUsize>,
}

#[async_trait]
impl McpServerHandler for SlowHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "slow".into(),
            version: "0.1.0".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        // Sleep 10s. The cancellation test should resolve well
        // before this completes.
        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok(vec![])
    }
    async fn call_tool(&self, _name: &str, _args: Value) -> Result<McpToolResult, McpError> {
        unreachable!("not exercised");
    }
}

#[tokio::test]
async fn cancel_mid_dispatch_aborts_handler() {
    let started = Arc::new(AtomicUsize::new(0));
    let dispatcher = Dispatcher::new(
        SlowHandler {
            started: started.clone(),
        },
        None,
    );
    let cancel = CancellationToken::new();
    let ctx = DispatchContext {
        session_id: None,
        request_id: Some(serde_json::json!(1)),
        cancel: cancel.clone(),
        principal: None,
        progress_token: None,
        session_sink: None,
        correlation_id: None,
    };

    let dispatcher_clone = dispatcher.clone();
    let dispatch_task = tokio::spawn(async move {
        dispatcher_clone
            .dispatch("tools/list", Value::Null, &ctx)
            .await
    });

    // Give the handler a chance to actually start sleeping.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mark = Instant::now();
    cancel.cancel();

    let outcome = tokio::time::timeout(Duration::from_secs(1), dispatch_task)
        .await
        .expect("dispatch should resolve within 1s of cancel")
        .expect("dispatch task itself should not panic");

    let elapsed = mark.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "expected cancel to abort within 500ms, took {elapsed:?}"
    );
    assert!(matches!(outcome, DispatchOutcome::Cancelled));
    assert_eq!(
        started.load(Ordering::SeqCst),
        1,
        "handler should have been entered before cancel fired"
    );
}

struct AuthOnlyHandler;

#[async_trait]
impl McpServerHandler for AuthOnlyHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "auth".into(),
            version: "0.1.0".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![])
    }
    async fn call_tool(&self, _name: &str, _args: Value) -> Result<McpToolResult, McpError> {
        Ok(McpToolResult {
            content: vec![],
            is_error: false,
            structured_content: None,
        })
    }
}

#[tokio::test]
async fn auth_token_mismatch_uses_constant_path() {
    // Same-length token, wrong content. Verifies the
    // length-mismatch shortcut isn't being taken (would otherwise
    // mask the consteq path).
    let dispatcher = Dispatcher::new(AuthOnlyHandler, Some("secret-token-12345".to_string()));
    let ctx = DispatchContext::empty();

    let outcome = dispatcher
        .dispatch(
            "initialize",
            serde_json::json!({"auth_token": "secret-token-XXXXX"}),
            &ctx,
        )
        .await;
    match outcome {
        DispatchOutcome::Error { code, .. } => assert_eq!(code, -32001),
        other => panic!("expected -32001 unauthorized, got {other:?}"),
    }
}

#[tokio::test]
async fn auth_token_correct_passes() {
    let dispatcher = Dispatcher::new(AuthOnlyHandler, Some("secret-token-12345".to_string()));
    let ctx = DispatchContext::empty();
    let outcome = dispatcher
        .dispatch(
            "initialize",
            serde_json::json!({"auth_token": "secret-token-12345"}),
            &ctx,
        )
        .await;
    match outcome {
        DispatchOutcome::Reply(v) => {
            assert!(v.get("serverInfo").is_some());
            assert!(v.get("protocolVersion").is_some());
        }
        other => panic!("expected Reply on valid token, got {other:?}"),
    }
}

#[tokio::test]
async fn notification_with_id_zero_is_treated_as_request() {
    // Regression guard: `id: 0` is a legitimate JSON-RPC request id
    // (only `id: null` / absence makes it a notification). The
    // server loop relies on `id.is_some()` to discriminate; this
    // test pins that behaviour at the dispatcher level by checking
    // that an id=0 initialize still produces a Reply.
    let dispatcher = Dispatcher::new(EchoHandler, None);
    let ctx = DispatchContext {
        session_id: None,
        request_id: Some(serde_json::json!(0)),
        cancel: CancellationToken::new(),
        principal: None,
        progress_token: None,
        session_sink: None,
        correlation_id: None,
    };
    let outcome = dispatcher
        .dispatch("initialize", serde_json::json!({}), &ctx)
        .await;
    assert!(
        matches!(outcome, DispatchOutcome::Reply(_)),
        "id=0 must be treated like any other request"
    );
}
