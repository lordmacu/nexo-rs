//! Phase 67.3/67.4 bin. Default: AllowAllDecider with a loud warning
//! (DEV ONLY). With `--socket <path>` (Phase 67.4) it forwards every
//! request to the daemon-side decider over a Unix socket.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use nexo_driver_permission::{
    AllowAllDecider, DenyAllDecider, PermissionDecider, PermissionError, PermissionMcpServer,
    PermissionRequest, PermissionResponse, SocketDecider,
};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let mut args = std::env::args().skip(1);
    let mut mode = Mode::AllowAll;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--allow-all" => mode = Mode::AllowAll,
            "--deny-all" => {
                let reason = args
                    .next()
                    .unwrap_or_else(|| "denied by --deny-all (no reason given)".into());
                mode = Mode::DenyAll(reason);
            }
            "--socket" => {
                let path = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--socket requires <path>"))?;
                mode = Mode::Socket(PathBuf::from(path));
            }
            "-h" | "--help" => {
                eprintln!(
                    "nexo-driver-permission-mcp [--socket <path> | --allow-all | --deny-all <reason>]\n\
                     \n\
                     Default: --allow-all (DEV ONLY). --socket forwards to the daemon's decider."
                );
                return Ok(());
            }
            other => {
                anyhow::bail!("unknown arg: {other}");
            }
        }
    }

    let decider = match mode {
        Mode::AllowAll => {
            tracing::warn!(
                "permission_mcp: AllowAllDecider active — DEV ONLY. \
                 Use --socket <path> for production."
            );
            BinDecider::AllowAll(AllowAllDecider)
        }
        Mode::DenyAll(reason) => {
            tracing::info!(target: "permission_mcp", "DenyAllDecider active: {reason}");
            BinDecider::DenyAll(DenyAllDecider { reason })
        }
        Mode::Socket(path) => {
            tracing::info!(
                target: "permission_mcp",
                "SocketDecider active: socket={}",
                path.display()
            );
            BinDecider::Socket(SocketDecider::new(path, Duration::from_secs(30)))
        }
    };

    let server = PermissionMcpServer::new(Arc::new(decider));
    let cancel = CancellationToken::new();
    nexo_mcp::server::run_stdio_server(server, cancel).await?;
    Ok(())
}

enum Mode {
    AllowAll,
    DenyAll(String),
    Socket(PathBuf),
}

/// Concrete enum so the bin can carry a single `Arc<BinDecider>`
/// through the MCP server. Lets the server stay monomorphised
/// behind `Arc<D>` instead of `Arc<dyn>`.
enum BinDecider {
    AllowAll(AllowAllDecider),
    DenyAll(DenyAllDecider),
    Socket(SocketDecider),
}

#[async_trait]
impl PermissionDecider for BinDecider {
    async fn decide(
        &self,
        request: PermissionRequest,
    ) -> Result<PermissionResponse, PermissionError> {
        match self {
            Self::AllowAll(d) => d.decide(request).await,
            Self::DenyAll(d) => d.decide(request).await,
            Self::Socket(d) => d.decide(request).await,
        }
    }
}
