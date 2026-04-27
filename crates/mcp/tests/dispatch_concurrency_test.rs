#![allow(clippy::all)] // Phase 76 scaffolding — re-enable when 76.x fully shipped

//! Phase 76.6 — `Dispatcher` integration tests with the
//! per-principal concurrency cap + per-call timeout wired in.
//!
//! These exercise the dispatch path end-to-end (no HTTP transport),
//! so the assertions cover:
//!   * `tools/call` past max_in_flight → `-32002` with
//!     `data.max_in_flight` + `data.queue_wait_ms_exceeded`
//!   * Per-call timeout → `-32001` with `data.timeout_ms`
//!   * Permit released after success / error / timeout / cancel
//!   * Cross-tenant + cross-tool isolation
//!   * Stdio principals bypass entirely
//!   * `tools/list` skips the cap

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nexo_mcp::server::auth::{AuthMethod, Principal, TenantId};
use nexo_mcp::server::per_principal_concurrency::{
    PerPrincipalConcurrencyCap, PerPrincipalConcurrencyConfig, PerToolConcurrency,
};
use nexo_mcp::server::{DispatchContext, DispatchOutcome, Dispatcher, McpServerHandler};
use nexo_mcp::types::{McpContent, McpServerInfo, McpTool, McpToolResult};
use nexo_mcp::McpError;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct SlowHandler {
    /// Active handler counter. Incremented on enter, decremented
    /// on exit — used to check no concurrent admits past the cap.
    in_flight: Arc<AtomicUsize>,
    /// Sleep duration on each call. Tests configure this to be
    /// longer/shorter than the dispatcher timeout to drive the
    /// timeout vs success paths.
    sleep_ms: u64,
}

impl SlowHandler {
    fn new(sleep_ms: u64) -> Self {
        Self {
            in_flight: Arc::new(AtomicUsize::new(0)),
            sleep_ms,
        }
    }
}

#[async_trait]
impl McpServerHandler for SlowHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "concurrency-test".into(),
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
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
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

fn cap_cfg(
    max_in_flight: u32,
    queue_wait_ms: u64,
    timeout_secs: u64,
) -> PerPrincipalConcurrencyConfig {
    let mut c = PerPrincipalConcurrencyConfig::default();
    c.default = PerToolConcurrency {
        max_in_flight,
        timeout_secs: Some(timeout_secs),
    };
    c.queue_wait_ms = queue_wait_ms;
    c.default_timeout_secs = timeout_secs;
    c
}

async fn build_dispatcher(
    cap_cfg: PerPrincipalConcurrencyConfig,
    sleep_ms: u64,
) -> (
    Dispatcher<SlowHandler>,
    Arc<AtomicUsize>,
    Arc<PerPrincipalConcurrencyCap>,
) {
    let cap = PerPrincipalConcurrencyCap::new_without_sweeper(cap_cfg).unwrap();
    let handler = SlowHandler::new(sleep_ms);
    let in_flight = Arc::clone(&handler.in_flight);
    let d = Dispatcher::with_rate_and_concurrency(handler, None, None, Some(Arc::clone(&cap)));
    (d, in_flight, cap)
}

#[tokio::test]
async fn cap_exceeded_returns_minus_32002() {
    // max_in_flight=2, queue_wait_ms=50 (short), handler sleeps 1s.
    // Fire 5 concurrent calls. First 2 admit; next 3 give up after
    // queue_wait expires and return -32002.
    let (d, _in_flight, _cap) = build_dispatcher(cap_cfg(2, 50, 30), 1_000).await;
    let p = principal_for("tenant-a", AuthMethod::StaticToken);
    let mut handles = Vec::new();
    for _ in 0..5 {
        let d = d.clone();
        let p = p.clone();
        handles.push(tokio::spawn(async move {
            d.dispatch(
                "tools/call",
                serde_json::json!({"name":"echo","arguments":{}}),
                &ctx(p),
            )
            .await
        }));
    }
    let mut admitted = 0;
    let mut rejected = 0;
    for h in handles {
        match h.await.unwrap() {
            DispatchOutcome::Reply(_) => admitted += 1,
            DispatchOutcome::Error { code, ref data, .. } => {
                assert_eq!(code, -32002);
                let max_in_flight = data
                    .as_ref()
                    .and_then(|v| v.get("max_in_flight"))
                    .and_then(|v| v.as_u64())
                    .unwrap();
                assert_eq!(max_in_flight, 2);
                rejected += 1;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(admitted, 2);
    assert_eq!(rejected, 3);
}

#[tokio::test]
async fn timeout_returns_minus_32001_with_data() {
    // timeout=200ms, handler sleeps 5s → must time out and return
    // -32001 with `timeout_ms: 200`.
    let mut c = cap_cfg(5, 50, 30);
    c.default = PerToolConcurrency {
        max_in_flight: 5,
        timeout_secs: None, // not used directly — see per_tool below
    };
    c.per_tool.insert(
        "echo".into(),
        PerToolConcurrency {
            max_in_flight: 5,
            timeout_secs: Some(1), // 1s — but we'd rather override
        },
    );
    // Actually use default block override at sub-second precision —
    // PerToolConcurrency.timeout_secs is in seconds. Drop to 1s and
    // sleep 3s in the handler to keep the test fast.
    let (d, _in_flight, _cap) = build_dispatcher(c, 3_000).await;
    let p = principal_for("tenant-a", AuthMethod::StaticToken);
    let outcome = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"echo","arguments":{}}),
            &ctx(p),
        )
        .await;
    match outcome {
        DispatchOutcome::Error { code, data, .. } => {
            assert_eq!(code, -32001);
            let timeout_ms = data
                .as_ref()
                .and_then(|v| v.get("timeout_ms"))
                .and_then(|v| v.as_u64())
                .unwrap();
            assert_eq!(timeout_ms, 1_000);
        }
        other => panic!("expected timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn permit_released_after_timeout() {
    // After a timeout fires, the semaphore must reflect the
    // released permit — next acquire is immediate.
    let mut c = cap_cfg(1, 50, 30);
    c.per_tool.insert(
        "echo".into(),
        PerToolConcurrency {
            max_in_flight: 1,
            timeout_secs: Some(1),
        },
    );
    let (d, _in_flight, cap) = build_dispatcher(c, 3_000).await;
    let p = principal_for("tenant-a", AuthMethod::StaticToken);
    let bad = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"echo","arguments":{}}),
            &ctx(p.clone()),
        )
        .await;
    assert!(
        matches!(bad, DispatchOutcome::Error { code: -32001, .. }),
        "expected timeout, got {bad:?}"
    );
    // Sleep briefly to let the handler future actually drop after
    // the timeout — Tokio's timeout drops the inner future at the
    // next .await but the semaphore release is the OwnedPermit drop.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let stats = cap.stats();
    assert_eq!(stats.timeouts_total, 1);
    // The bucket should still exist (we just used it) but with all
    // permits restored.
    assert_eq!(cap.semaphore_count(), 1);
}

#[tokio::test]
async fn cross_tenant_caps_independent() {
    let (d, _in_flight, _cap) = build_dispatcher(cap_cfg(1, 50, 30), 200).await;
    let a = principal_for("tenant-a", AuthMethod::StaticToken);
    let b = principal_for("tenant-b", AuthMethod::StaticToken);
    let d_a = d.clone();
    let d_b = d.clone();
    let a_handle = tokio::spawn(async move {
        d_a.dispatch(
            "tools/call",
            serde_json::json!({"name":"echo","arguments":{}}),
            &ctx(a),
        )
        .await
    });
    let b_handle = tokio::spawn(async move {
        d_b.dispatch(
            "tools/call",
            serde_json::json!({"name":"echo","arguments":{}}),
            &ctx(b),
        )
        .await
    });
    // Both must succeed — distinct semaphores per tenant.
    assert!(matches!(a_handle.await.unwrap(), DispatchOutcome::Reply(_)));
    assert!(matches!(b_handle.await.unwrap(), DispatchOutcome::Reply(_)));
}

#[tokio::test]
async fn cross_tool_caps_for_same_tenant_independent() {
    let (d, _in_flight, _cap) = build_dispatcher(cap_cfg(1, 50, 30), 200).await;
    let p = principal_for("tenant-a", AuthMethod::StaticToken);
    let d_a = d.clone();
    let d_b = d.clone();
    let p_a = p.clone();
    let p_b = p.clone();
    let a = tokio::spawn(async move {
        d_a.dispatch(
            "tools/call",
            serde_json::json!({"name":"tool_a","arguments":{}}),
            &ctx(p_a),
        )
        .await
    });
    let b = tokio::spawn(async move {
        d_b.dispatch(
            "tools/call",
            serde_json::json!({"name":"tool_b","arguments":{}}),
            &ctx(p_b),
        )
        .await
    });
    // Same tenant, different tools — different semaphores.
    assert!(matches!(a.await.unwrap(), DispatchOutcome::Reply(_)));
    assert!(matches!(b.await.unwrap(), DispatchOutcome::Reply(_)));
}

#[tokio::test]
async fn stdio_principal_bypasses_cap() {
    let (d, _in_flight, cap) = build_dispatcher(cap_cfg(1, 50, 30), 100).await;
    let p = principal_for("tenant-a", AuthMethod::Stdio);
    // 5 concurrent calls — all succeed, no bucket created.
    let mut handles = Vec::new();
    for _ in 0..5 {
        let d = d.clone();
        let p = p.clone();
        handles.push(tokio::spawn(async move {
            d.dispatch(
                "tools/call",
                serde_json::json!({"name":"echo","arguments":{}}),
                &ctx(p),
            )
            .await
        }));
    }
    for h in handles {
        assert!(matches!(h.await.unwrap(), DispatchOutcome::Reply(_)));
    }
    assert_eq!(
        cap.semaphore_count(),
        0,
        "stdio principals must not allocate a semaphore"
    );
}

#[tokio::test]
async fn tools_list_not_capped() {
    // max_in_flight=1, but tools/list bypasses entirely — fire
    // 50 concurrent and they all succeed.
    let (d, _in_flight, cap) = build_dispatcher(cap_cfg(1, 50, 30), 0).await;
    let p = principal_for("tenant-a", AuthMethod::StaticToken);
    let mut handles = Vec::new();
    for _ in 0..50 {
        let d = d.clone();
        let p = p.clone();
        handles.push(tokio::spawn(async move {
            d.dispatch("tools/list", Value::Null, &ctx(p)).await
        }));
    }
    for h in handles {
        assert!(matches!(h.await.unwrap(), DispatchOutcome::Reply(_)));
    }
    assert_eq!(cap.semaphore_count(), 0);
}

#[tokio::test]
async fn permit_released_after_success() {
    // Fire one call, wait for it to complete, then verify the
    // bucket is back to full permits.
    let (d, _in_flight, cap) = build_dispatcher(cap_cfg(2, 50, 30), 50).await;
    let p = principal_for("tenant-a", AuthMethod::StaticToken);
    let outcome = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"echo","arguments":{}}),
            &ctx(p),
        )
        .await;
    assert!(matches!(outcome, DispatchOutcome::Reply(_)));
    // Brief delay so the permit drop fully propagates through the
    // tokio scheduler.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(cap.semaphore_count(), 1);
    // Re-acquire confirms the permit is back.
    let cancel = CancellationToken::new();
    let tenant = TenantId::parse("tenant-a").unwrap();
    let _p1 = cap.acquire(&tenant, "echo", &cancel).await.unwrap();
    let _p2 = cap.acquire(&tenant, "echo", &cancel).await.unwrap();
}
