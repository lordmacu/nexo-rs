#![allow(clippy::all)] // Phase 76 scaffolding — re-enable when 76.x fully shipped

//! Phase 76.4 — multi-tenant isolation fixture.
//!
//! Two tenants (`tenant-a`, `tenant-b`) reach the same in-process
//! handler over HTTP, each authenticated with its own static token
//! pinned to its own tenant. The handler:
//!
//!   * computes its tenant-scoped path via `tenant_scoped_path`,
//!   * writes a marker file,
//!   * lists files under its tenant dir.
//!
//! Assertions:
//!   1. Each tenant sees ONLY its own marker (no cross-tenant
//!      visibility).
//!   2. `tenant_db_path` returns distinct paths for distinct tenants.
//!   3. `TenantScoped::try_into_inner` rejects an extraction made
//!      under the wrong tenant.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nexo_mcp::server::auth::{
    tenant_db_path, tenant_scoped_path, AuthConfig, CrossTenantError, TenantId, TenantScoped,
};
use nexo_mcp::server::http_config::HttpTransportConfig;
use nexo_mcp::types::{McpContent, McpServerInfo, McpTool, McpToolResult};
use nexo_mcp::{start_http_server, HttpServerHandle, McpError, McpServerHandler};
use reqwest::Client;
use serde_json::Value;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// Handler that, on `tools/call name=write_marker`, looks up the
/// caller's tenant from `DispatchContext` and writes a marker file
/// + reports the directory listing back as text content.
///
/// `DispatchContext` isn't passed into `call_tool` in the current
/// public trait; instead the test handler reads the principal from
/// a thread-local set by a wrapper. Cleaner in production would be
/// to plumb principal into ToolContext (Phase 76.4 follow-up); for
/// the fixture we set up the work via separate per-tenant handlers.
#[derive(Clone)]
struct PerTenantHandler {
    tenant: TenantId,
    root: Arc<PathBuf>,
}

#[async_trait]
impl McpServerHandler for PerTenantHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: format!("h-{}", self.tenant),
            version: "0.0.1".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![McpTool {
            name: "write_marker".into(),
            description: None,
            input_schema: serde_json::json!({"type":"object"}),
            output_schema: None,
        }])
    }
    async fn call_tool(&self, name: &str, _: Value) -> Result<McpToolResult, McpError> {
        if name != "write_marker" {
            return Err(McpError::Protocol(format!("unknown tool {name}")));
        }
        let dir = tenant_scoped_path(&self.root, &self.tenant, "");
        std::fs::create_dir_all(&dir).map_err(|e| McpError::Protocol(e.to_string()))?;
        let marker = tenant_scoped_path(&self.root, &self.tenant, "marker.txt");
        std::fs::write(&marker, self.tenant.as_str().as_bytes())
            .map_err(|e| McpError::Protocol(e.to_string()))?;
        // List tenant's own dir.
        let mut listing: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| McpError::Protocol(e.to_string()))?
            .flatten()
        {
            listing.push(entry.file_name().to_string_lossy().into_owned());
        }
        Ok(McpToolResult {
            content: vec![McpContent::Text {
                text: listing.join(","),
            }],
            is_error: false,
            structured_content: None,
        })
    }
}

async fn boot_tenant(
    tenant_str: &str,
    root: Arc<PathBuf>,
    token: &str,
) -> (HttpServerHandle, Client, CancellationToken) {
    let tenant = TenantId::parse(tenant_str).unwrap();
    let handler = PerTenantHandler {
        tenant: tenant.clone(),
        root,
    };
    let mut cfg = HttpTransportConfig::default();
    cfg.enabled = true;
    cfg.bind = "127.0.0.1:0".parse().unwrap();
    cfg.auth = Some(AuthConfig::StaticToken {
        token: Some(token.into()),
        token_env: None,
        tenant: Some(tenant_str.to_string()),
    });
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

async fn call_write_marker(client: &Client, addr: std::net::SocketAddr, token: &str) -> String {
    let url = format!("http://{addr}/mcp");
    // initialize first
    let init = client
        .post(&url)
        .header("authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"initialize","params":{},"id":1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(init.status().as_u16(), 200);
    let session = init
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    // tools/call write_marker
    let resp = client
        .post(&url)
        .header("authorization", format!("Bearer {token}"))
        .header("mcp-session-id", session)
        .json(&serde_json::json!({
            "jsonrpc":"2.0",
            "method":"tools/call",
            "params":{"name":"write_marker","arguments":{}},
            "id":2
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.unwrap();
    body["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string()
}

async fn shutdown(handle: HttpServerHandle, token: CancellationToken) {
    token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), handle.join).await;
}

#[tokio::test]
async fn cross_tenant_marker_isolated() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(tmp.path().to_path_buf());

    let (h_a, c_a, s_a) = boot_tenant("tenant-a", root.clone(), "tok-a").await;
    let (h_b, c_b, s_b) = boot_tenant("tenant-b", root.clone(), "tok-b").await;

    // Each tenant calls write_marker through its own server. The
    // listing reported by the handler must contain ONLY that
    // tenant's own marker, not the sibling's.
    let listing_a = call_write_marker(&c_a, h_a.bind_addr, "tok-a").await;
    let listing_b = call_write_marker(&c_b, h_b.bind_addr, "tok-b").await;

    assert!(
        listing_a.contains("marker.txt"),
        "tenant-a should see its own marker, got `{listing_a}`"
    );
    assert!(
        listing_b.contains("marker.txt"),
        "tenant-b should see its own marker, got `{listing_b}`"
    );
    // Each listing is rooted in its own tenant dir, so neither
    // can show files from the other.
    assert!(
        !listing_a.contains("tenant-b"),
        "tenant-a saw tenant-b name in its listing: {listing_a}"
    );
    assert!(
        !listing_b.contains("tenant-a"),
        "tenant-b saw tenant-a name in its listing: {listing_b}"
    );

    // Confirm at the filesystem layer: two distinct marker files
    // under two distinct tenant dirs.
    let path_a = tmp.path().join("tenants/tenant-a/marker.txt");
    let path_b = tmp.path().join("tenants/tenant-b/marker.txt");
    assert!(path_a.exists(), "tenant-a marker missing");
    assert!(path_b.exists(), "tenant-b marker missing");
    assert_eq!(std::fs::read_to_string(&path_a).unwrap(), "tenant-a");
    assert_eq!(std::fs::read_to_string(&path_b).unwrap(), "tenant-b");

    shutdown(h_a, s_a).await;
    shutdown(h_b, s_b).await;
}

#[tokio::test]
async fn tenant_db_paths_distinct() {
    let root = std::path::Path::new("/var/lib/nexo");
    let t1 = TenantId::parse("acme").unwrap();
    let t2 = TenantId::parse("globex").unwrap();
    let p1 = tenant_db_path(root, &t1);
    let p2 = tenant_db_path(root, &t2);
    assert_ne!(p1, p2);
    assert!(p1.ends_with("tenants/acme/state.sqlite3"));
    assert!(p2.ends_with("tenants/globex/state.sqlite3"));
}

#[tokio::test]
async fn scoped_unwrap_blocks_cross_access() {
    let t1 = TenantId::parse("tenant-a").unwrap();
    let t2 = TenantId::parse("tenant-b").unwrap();
    // Pretend `db_handle: u64` is a per-tenant SQLite handle scoped
    // to tenant-a. Trying to extract under tenant-b must fail
    // hard rather than silently leak the handle.
    let scoped: TenantScoped<u64> = TenantScoped::new(t1.clone(), 42);
    match scoped.try_into_inner(&t2) {
        Err(CrossTenantError { held, requested }) => {
            assert_eq!(held, "tenant-a");
            assert_eq!(requested, "tenant-b");
        }
        Ok(v) => panic!("must not extract cross-tenant; got {v}"),
    }
}
