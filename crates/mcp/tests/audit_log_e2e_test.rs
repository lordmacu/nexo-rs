#![allow(clippy::all)] // Phase 76 scaffolding — re-enable when 76.x fully shipped

//! Phase 76.11 — end-to-end test: dispatcher emits one audit row
//! per `tools/call` dispatch, regardless of outcome.
//!
//! Boots a `Dispatcher` with a `MemoryAuditLogStore` + an
//! `AuditWriter`, fires a mix of Reply / Error / Cancelled
//! dispatches, asserts every one shows up in the audit log with
//! the right outcome label.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nexo_mcp::server::audit_log::{
    AuditFilter, AuditLogStore, AuditOutcome, AuditWriter, MemoryAuditLogStore,
};
use nexo_mcp::server::auth::{AuthMethod, Principal, TenantId};
use nexo_mcp::server::{DispatchContext, DispatchOutcome, Dispatcher, McpServerHandler};
use nexo_mcp::types::{McpContent, McpServerInfo, McpTool, McpToolResult};
use nexo_mcp::McpError;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct ToggleHandler;

#[async_trait]
impl McpServerHandler for ToggleHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "audit-test".into(),
            version: "0.0.1".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![McpTool {
            name: "toggle".into(),
            description: None,
            input_schema: serde_json::json!({"type":"object"}),
            output_schema: None,
        }])
    }
    async fn call_tool(&self, name: &str, args: Value) -> Result<McpToolResult, McpError> {
        let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("ok");
        match (name, mode) {
            (_, "ok") => Ok(McpToolResult {
                content: vec![McpContent::Text { text: "ok".into() }],
                is_error: false,
                structured_content: None,
            }),
            (_, "err") => Err(McpError::Protocol("intentional".into())),
            _ => Err(McpError::Protocol("unknown mode".into())),
        }
    }
}

fn ctx_for(tenant: &str) -> DispatchContext {
    let mut p = Principal::stdio_local();
    p.tenant = TenantId::parse(tenant).unwrap();
    p.auth_method = AuthMethod::StaticToken;
    DispatchContext {
        session_id: Some("sess-1".into()),
        request_id: Some(serde_json::json!(1)),
        cancel: CancellationToken::new(),
        principal: Some(p),
        progress_token: None,
        session_sink: None,
        correlation_id: Some("req-X".into()),
    }
}

#[tokio::test]
async fn each_tools_call_emits_one_audit_row() {
    let store: Arc<dyn AuditLogStore> = Arc::new(MemoryAuditLogStore::new());
    let mut cfg = nexo_mcp::server::audit_log::AuditLogConfig::default();
    cfg.flush_interval_ms = 20;
    cfg.flush_batch_size = 1;
    let writer = AuditWriter::spawn(cfg, Arc::clone(&store)).unwrap();
    let d = Dispatcher::with_full_stack(
        ToggleHandler,
        None,
        None,
        None,
        None,
        Some(Arc::clone(&writer)),
    );

    // 3 OK dispatches.
    for _ in 0..3 {
        let outcome = d
            .dispatch(
                "tools/call",
                serde_json::json!({"name":"toggle","arguments":{"mode":"ok"}}),
                &ctx_for("tenant-a"),
            )
            .await;
        assert!(matches!(outcome, DispatchOutcome::Reply(_)));
    }

    // 2 error dispatches.
    for _ in 0..2 {
        let outcome = d
            .dispatch(
                "tools/call",
                serde_json::json!({"name":"toggle","arguments":{"mode":"err"}}),
                &ctx_for("tenant-a"),
            )
            .await;
        assert!(matches!(outcome, DispatchOutcome::Error { .. }));
    }

    // Flush + drain.
    writer.drain(Duration::from_secs(2)).await;

    let total = store.count(&AuditFilter::default()).await.unwrap();
    assert_eq!(total, 5, "5 dispatches → 5 audit rows");

    let ok = store
        .count(&AuditFilter {
            outcome: Some(AuditOutcome::Ok),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ok, 3, "3 OK rows expected");

    let errs = store
        .count(&AuditFilter {
            outcome: Some(AuditOutcome::Error),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(errs, 2, "2 Error rows expected");
}

#[tokio::test]
async fn audit_row_carries_correlation_and_tenant() {
    let store: Arc<dyn AuditLogStore> = Arc::new(MemoryAuditLogStore::new());
    let mut cfg = nexo_mcp::server::audit_log::AuditLogConfig::default();
    cfg.flush_interval_ms = 20;
    cfg.flush_batch_size = 1;
    let writer = AuditWriter::spawn(cfg, Arc::clone(&store)).unwrap();
    let d = Dispatcher::with_full_stack(
        ToggleHandler,
        None,
        None,
        None,
        None,
        Some(Arc::clone(&writer)),
    );

    let outcome = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"toggle","arguments":{"mode":"ok"}}),
            &ctx_for("acme"),
        )
        .await;
    assert!(matches!(outcome, DispatchOutcome::Reply(_)));

    writer.drain(Duration::from_secs(2)).await;
    let rows = store.tail(&AuditFilter::default(), 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.tenant, "acme");
    assert_eq!(row.tool_name.as_deref(), Some("toggle"));
    assert_eq!(row.method, "tools/call");
    assert_eq!(row.request_id.as_deref(), Some("req-X"));
    assert_eq!(row.session_id.as_deref(), Some("sess-1"));
    assert_eq!(row.auth_method, "static_token");
    assert_eq!(row.outcome, AuditOutcome::Ok);
    assert!(row.duration_ms.is_some());
}

#[tokio::test]
async fn no_writer_no_panic() {
    // Sanity: dispatcher with None audit writer still works.
    let d = Dispatcher::new(ToggleHandler, None);
    let outcome = d
        .dispatch(
            "tools/call",
            serde_json::json!({"name":"toggle","arguments":{"mode":"ok"}}),
            &ctx_for("t"),
        )
        .await;
    assert!(matches!(outcome, DispatchOutcome::Reply(_)));
}
