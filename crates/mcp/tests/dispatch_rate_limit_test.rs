#![allow(clippy::all)] // Phase 76 scaffolding — re-enable when 76.x fully shipped

//! Phase 76.5 — `Dispatcher` integration tests with the
//! per-principal rate-limiter wired in.
//!
//! These exercise the dispatch path end-to-end (no HTTP transport),
//! so the assertions cover:
//!   * `tools/call` bursts past the bucket → `-32099` with
//!     `data.retry_after_ms`
//!   * `tools/list`, `initialize`, `shutdown` ignore the limiter
//!   * Cross-tenant fairness via different `Principal` shapes
//!   * Per-tool override applied
//!   * Stdio principals bypass entirely

use std::sync::Arc;

use async_trait::async_trait;
use nexo_mcp::server::auth::{AuthMethod, Principal, TenantId};
use nexo_mcp::server::per_principal_rate_limit::{
    PerPrincipalRateLimiter, PerPrincipalRateLimiterConfig, PerToolLimit,
};
use nexo_mcp::server::{DispatchContext, DispatchOutcome, Dispatcher, McpServerHandler};
use nexo_mcp::types::{McpContent, McpServerInfo, McpTool, McpToolResult};
use nexo_mcp::McpError;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct EchoHandler;

#[async_trait]
impl McpServerHandler for EchoHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "rl-test".into(),
            version: "0.0.1".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![McpTool {
            name: "echo".into(),
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
}

fn principal_for(tenant: &str, method: AuthMethod) -> Principal {
    let mut p = Principal::stdio_local();
    p.tenant = TenantId::parse(tenant).unwrap();
    p.auth_method = method;
    p
}

fn ctx(principal: Principal) -> DispatchContext {
    DispatchContext {
        session_id: None,
        request_id: Some(serde_json::json!(1)),
        cancel: CancellationToken::new(),
        principal: Some(principal),
        progress_token: None,
        session_sink: None,
        correlation_id: None,
    }
}

fn cfg(rps: f64, burst: f64) -> PerPrincipalRateLimiterConfig {
    let mut c = PerPrincipalRateLimiterConfig::default();
    c.default = PerToolLimit { rps, burst };
    c.warn_threshold = 0.0; // suppress warn-log during tests
    c
}

async fn build_dispatcher(cfg: PerPrincipalRateLimiterConfig) -> Dispatcher<EchoHandler> {
    let lim = PerPrincipalRateLimiter::new_without_sweeper(cfg).unwrap();
    Dispatcher::with_rate_limiter(EchoHandler, None, lim)
}

#[tokio::test]
async fn tools_call_rejected_on_burst_overflow() {
    let d = build_dispatcher(cfg(1.0, 2.0)).await;
    let p = principal_for("tenant-a", AuthMethod::StaticToken);
    let mut admitted = 0u32;
    let mut rejected = 0u32;
    let mut last_retry: Option<u64> = None;
    for _ in 0..5 {
        let outcome = d
            .dispatch(
                "tools/call",
                serde_json::json!({"name":"echo","arguments":{}}),
                &ctx(p.clone()),
            )
            .await;
        match outcome {
            DispatchOutcome::Reply(_) => admitted += 1,
            DispatchOutcome::Error { code, ref data, .. } => {
                assert_eq!(code, -32099);
                rejected += 1;
                last_retry = data
                    .as_ref()
                    .and_then(|d| d.get("retry_after_ms"))
                    .and_then(|v| v.as_u64());
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
    assert_eq!(admitted, 2, "burst should admit exactly 2");
    assert_eq!(rejected, 3, "remaining 3 must be rejected");
    let r = last_retry.expect("data.retry_after_ms must be set on hit");
    assert!(r >= 1, "retry_after_ms must be ≥ 1, got {r}");
}

#[tokio::test]
async fn tools_list_not_rate_limited() {
    let d = build_dispatcher(cfg(0.001, 1.0)).await; // would exhaust on 2nd call_tool
    let p = principal_for("tenant-a", AuthMethod::StaticToken);
    for _ in 0..50 {
        let outcome = d.dispatch("tools/list", Value::Null, &ctx(p.clone())).await;
        assert!(
            matches!(outcome, DispatchOutcome::Reply(_)),
            "tools/list must bypass rate-limit, got {outcome:?}"
        );
    }
}

#[tokio::test]
async fn initialize_not_rate_limited() {
    let d = build_dispatcher(cfg(0.001, 1.0)).await;
    let p = principal_for("tenant-a", AuthMethod::StaticToken);
    for _ in 0..10 {
        let outcome = d
            .dispatch("initialize", serde_json::json!({}), &ctx(p.clone()))
            .await;
        assert!(
            matches!(outcome, DispatchOutcome::Reply(_)),
            "initialize must bypass rate-limit, got {outcome:?}"
        );
    }
}

#[tokio::test]
async fn cross_tenant_fairness_via_dispatcher() {
    let d = build_dispatcher(cfg(1.0, 1.0)).await;
    let a = principal_for("tenant-a", AuthMethod::StaticToken);
    let b = principal_for("tenant-b", AuthMethod::StaticToken);
    // Drain A.
    let _ = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"echo","arguments":{}}),
            &ctx(a.clone()),
        )
        .await;
    let bad = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"echo","arguments":{}}),
            &ctx(a.clone()),
        )
        .await;
    assert!(
        matches!(bad, DispatchOutcome::Error { code: -32099, .. }),
        "A's 2nd call must be rejected"
    );
    // B unaffected.
    let ok = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"echo","arguments":{}}),
            &ctx(b.clone()),
        )
        .await;
    assert!(
        matches!(ok, DispatchOutcome::Reply(_)),
        "B's call must pass"
    );
}

#[tokio::test]
async fn cross_tool_isolation_via_dispatcher() {
    let d = build_dispatcher(cfg(1.0, 1.0)).await;
    let p = principal_for("tenant-a", AuthMethod::StaticToken);
    // Drain tool_a.
    let _ = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"tool_a","arguments":{}}),
            &ctx(p.clone()),
        )
        .await;
    let bad = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"tool_a","arguments":{}}),
            &ctx(p.clone()),
        )
        .await;
    assert!(matches!(bad, DispatchOutcome::Error { code: -32099, .. }));
    // Same tenant, different tool — different bucket.
    let ok = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"tool_b","arguments":{}}),
            &ctx(p.clone()),
        )
        .await;
    assert!(matches!(ok, DispatchOutcome::Reply(_)));
}

#[tokio::test]
async fn per_tool_override_applied() {
    let mut c = cfg(100.0, 100.0);
    c.per_tool.insert(
        "heavy".into(),
        PerToolLimit {
            rps: 1.0,
            burst: 1.0,
        },
    );
    let d = build_dispatcher(c).await;
    let p = principal_for("tenant-a", AuthMethod::StaticToken);
    let mut admitted = 0u32;
    let mut rejected = 0u32;
    for _ in 0..5 {
        let outcome = d
            .dispatch(
                "tools/call",
                serde_json::json!({"name":"heavy","arguments":{}}),
                &ctx(p.clone()),
            )
            .await;
        match outcome {
            DispatchOutcome::Reply(_) => admitted += 1,
            DispatchOutcome::Error { code: -32099, .. } => rejected += 1,
            other => panic!("unexpected: {other:?}"),
        }
    }
    assert_eq!(admitted, 1);
    assert_eq!(rejected, 4);
}

#[tokio::test]
async fn stdio_principal_bypasses_limiter() {
    let lim = PerPrincipalRateLimiter::new_without_sweeper(cfg(0.001, 1.0)).unwrap();
    let lim_for_assert = Arc::clone(&lim);
    let d = Dispatcher::with_rate_limiter(EchoHandler, None, lim);
    let p = principal_for("tenant-a", AuthMethod::Stdio);
    // Hammer 100 calls — every one admits because stdio bypasses.
    for _ in 0..100 {
        let outcome = d
            .dispatch(
                "tools/call",
                serde_json::json!({"name":"echo","arguments":{}}),
                &ctx(p.clone()),
            )
            .await;
        assert!(matches!(outcome, DispatchOutcome::Reply(_)));
    }
    // Limiter must not have created any bucket — stdio path
    // skipped the check entirely.
    assert_eq!(
        lim_for_assert.bucket_count(),
        0,
        "stdio principals must not allocate a bucket"
    );
}
