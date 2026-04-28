//! Phase 76.15 — MCP server template (stdio + optional HTTP).
//!
//! Boot model:
//!   * Always opens the stdio transport (the agent's extension
//!     supervisor pipes JSON-RPC frames in/out of the child).
//!   * When `MCP_TEMPLATE_HTTP_BIND` is set (e.g.
//!     `127.0.0.1:7676`), additionally boots the HTTP+SSE
//!     transport on that address — useful for direct testing with
//!     `curl` or `claude mcp` without going through the agent.
//!
//! Reference patterns:
//!   * `claude-code-leak/src/entrypoints/mcp.ts:35-78` — minimal
//!     stdio MCP server. The leak uses `setRequestHandler` per
//!     schema; we use `McpServerBuilder::tool(impl Tool)` for the
//!     same effect.
//!   * `claude-code-leak/src/utils/computerUse/mcpServer.ts:60-78`
//!     — `createXxxMcpServer()` factory + post-construction
//!     handler swap. We collapse that into one builder chain.
//!   * `crates/mcp/src/bin/mock_mcp_server.rs` — exhaustive
//!     reference for protocol corner cases (initialize, paginate,
//!     errors, notifications). Treat this template as the
//!     starting point and that file as the field guide.

mod tools;

use std::time::Duration;

use anyhow::Context;
use nexo_mcp::server::builder::McpServerBuilder;
use nexo_mcp::server::http_config::HttpTransportConfig;
use nexo_mcp::{run_stdio_server, start_http_server};
use tokio_util::sync::CancellationToken;

const SERVER_NAME: &str = "template-mcp-server";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs to stderr so stdio JSON-RPC stays clean on stdout.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Build a fresh handler per transport. `BuiltHandler` is not
    // `Clone`, but `McpServerBuilder` is cheap to construct and
    // `Tool` impls are unit structs — duplicating registration
    // costs nothing and keeps each transport with its own state.
    let build_handler = || {
        McpServerBuilder::new(SERVER_NAME, SERVER_VERSION)
            .tool(tools::Echo)
            .build_handler()
    };

    let shutdown = CancellationToken::new();

    // Phase 76.15 — optional HTTP transport. Boots only when the
    // operator opts in by setting `MCP_TEMPLATE_HTTP_BIND`. The
    // returned `HttpServerHandle` is dropped at end of main; the
    // server exits when `shutdown` is cancelled (SIGTERM below).
    let http_handle = if let Ok(bind) = std::env::var("MCP_TEMPLATE_HTTP_BIND") {
        let bind_addr = bind
            .parse()
            .with_context(|| format!("MCP_TEMPLATE_HTTP_BIND={bind} is not a valid socket addr"))?;
        let mut cfg = HttpTransportConfig::default();
        cfg.enabled = true;
        cfg.bind = bind_addr;
        // Operator opts into a static-token auth via env. Empty =
        // loopback dev (none). For production, prefer the YAML-
        // driven `nexo mcp-server` boot path which supports JWT +
        // mTLS too — see `docs/src/extensions/mcp-server.md`.
        if let Ok(tok) = std::env::var("MCP_TEMPLATE_HTTP_TOKEN") {
            if !tok.is_empty() {
                cfg.auth_token = Some(tok);
            }
        }
        let handle = start_http_server(build_handler(), cfg, shutdown.clone())
            .await
            .with_context(|| format!("failed to bind HTTP transport at {bind_addr}"))?;
        tracing::info!(addr = %handle.bind_addr, "template-mcp-server http listening");
        Some(handle)
    } else {
        None
    };

    // SIGTERM / SIGINT → graceful shutdown.
    let shutdown_for_signal = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown_for_signal.cancel();
    });

    // Stdio is always on — when the agent's extension supervisor
    // closes our stdin, `run_stdio_server` returns and we drop
    // out of main.
    run_stdio_server(build_handler(), shutdown.clone()).await?;
    shutdown.cancel();

    if let Some(h) = http_handle {
        // Bound the HTTP join so a misbehaving SSE consumer can't
        // hold the process up.
        let _ = tokio::time::timeout(Duration::from_secs(3), h.join).await;
    }

    Ok(())
}
