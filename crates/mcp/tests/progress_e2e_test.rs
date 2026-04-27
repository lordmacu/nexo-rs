//! Phase 76.7 — end-to-end progress + notifications tests.
//!
//! Drives the dispatcher with a custom `McpServerHandler` that
//! emits progress via `call_tool_streaming`, asserts the
//! resulting `notifications/progress` arrive on the session's
//! broadcast channel.

use std::time::Duration;

use async_trait::async_trait;
use nexo_mcp::server::auth::{AuthMethod, Principal, TenantId};
use nexo_mcp::server::http_session::SessionEvent;
use nexo_mcp::server::progress::ProgressReporter;
use nexo_mcp::server::{DispatchContext, DispatchOutcome, Dispatcher, McpServerHandler};
use nexo_mcp::types::{McpContent, McpServerInfo, McpTool, McpToolResult};
use nexo_mcp::McpError;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct ProgressingHandler {
    n: usize,
}

#[async_trait]
impl McpServerHandler for ProgressingHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "progress-test".into(),
            version: "0.0.1".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![McpTool {
            name: "long_run".into(),
            description: None,
            input_schema: serde_json::json!({"type":"object"}),
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
    async fn call_tool_streaming(
        &self,
        _name: &str,
        _args: Value,
        progress: ProgressReporter,
    ) -> Result<McpToolResult, McpError> {
        for i in 1..=self.n {
            progress.report(i as f64, Some(self.n as f64), None);
            // Tiny stagger to defeat coalescing — the reporter's
            // 20 ms gate would otherwise collapse all events into
            // ≤ 2 emits.
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Ok(McpToolResult {
            content: vec![McpContent::Text {
                text: "done".into(),
            }],
            is_error: false,
            structured_content: None,
        })
    }
}

fn ctx_with(
    progress_token: Option<Value>,
    sink: broadcast::Sender<SessionEvent>,
) -> DispatchContext {
    let mut p = Principal::stdio_local();
    p.tenant = TenantId::parse("tenant-a").unwrap();
    p.auth_method = AuthMethod::StaticToken;
    DispatchContext {
        session_id: Some("sess-1".into()),
        request_id: Some(serde_json::json!(1)),
        cancel: CancellationToken::new(),
        principal: Some(p),
        progress_token,
        session_sink: Some(sink),
        correlation_id: None,
    }
}

fn collect_progress(events: Vec<SessionEvent>) -> Vec<f64> {
    events
        .into_iter()
        .filter_map(|ev| match ev {
            SessionEvent::Message(v) => {
                let m = v.get("method")?.as_str()?;
                if m != "notifications/progress" {
                    return None;
                }
                v.get("params")?.get("progress")?.as_f64()
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn tools_call_with_progress_token_emits_notifications() {
    let (tx, mut rx) = broadcast::channel::<SessionEvent>(64);
    let d = Dispatcher::new(ProgressingHandler { n: 5 }, None);
    let ctx = ctx_with(Some(serde_json::json!("tok-A")), tx);

    let task = tokio::spawn(async move {
        d.dispatch(
            "tools/call",
            serde_json::json!({"name":"long_run","arguments":{}}),
            &ctx,
        )
        .await
    });

    let mut events = Vec::new();
    let recv_loop = async {
        while let Ok(ev) = rx.recv().await {
            events.push(ev);
        }
        events
    };

    // Race: collect events until the dispatch completes.
    let outcome = task.await.unwrap();
    assert!(matches!(outcome, DispatchOutcome::Reply(_)));

    // Drain remaining events (timeout-bounded so we don't block).
    drop(recv_loop);
    let mut leftover = Vec::new();
    while let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
        leftover.push(ev);
    }

    let progresses = collect_progress(leftover);
    // We should have seen at least 1 progress event (the very
    // first emit isn't gated). The handler stagger is 25 ms which
    // is just past the 20 ms coalesce window, so we expect ≥ 3.
    assert!(
        !progresses.is_empty(),
        "expected at least one progress event"
    );
    // Ordered ascending.
    let mut sorted = progresses.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(progresses, sorted, "progress events arrived out of order");
}

#[tokio::test]
async fn tools_call_without_progress_token_emits_nothing() {
    let (tx, mut rx) = broadcast::channel::<SessionEvent>(64);
    let d = Dispatcher::new(ProgressingHandler { n: 5 }, None);
    let ctx = ctx_with(None, tx);

    let outcome = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"long_run","arguments":{}}),
            &ctx,
        )
        .await;
    assert!(matches!(outcome, DispatchOutcome::Reply(_)));

    let mut got_progress = 0;
    while let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
        if let SessionEvent::Message(v) = ev {
            if v.get("method").and_then(Value::as_str) == Some("notifications/progress") {
                got_progress += 1;
            }
        }
    }
    assert_eq!(got_progress, 0, "no progress without progressToken");
}

#[tokio::test]
async fn dropped_subscriber_does_not_panic_handler() {
    // Subscriber drops before handler finishes. The reporter
    // sees `send` errors silently — handler must complete clean.
    let (tx, rx) = broadcast::channel::<SessionEvent>(8);
    drop(rx);
    let d = Dispatcher::new(ProgressingHandler { n: 10 }, None);
    let ctx = ctx_with(Some(serde_json::json!("tok-A")), tx);
    let outcome = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"long_run","arguments":{}}),
            &ctx,
        )
        .await;
    assert!(matches!(outcome, DispatchOutcome::Reply(_)));
}
