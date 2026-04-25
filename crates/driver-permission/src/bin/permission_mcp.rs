//! Phase 67.3 placeholder bin. Without `--socket <path>` (Phase 67.4)
//! this defaults to `AllowAllDecider` and emits a loud warning. Use
//! `--deny-all <reason>` to test the deny path.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nexo_driver_permission::{
    AllowAllDecider, DenyAllDecider, PermissionDecider, PermissionError, PermissionMcpServer,
    PermissionRequest, PermissionResponse,
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
            "-h" | "--help" => {
                eprintln!(
                    "nexo-driver-permission-mcp [--allow-all | --deny-all <reason>]\n\
                     \n\
                     Phase 67.3 placeholder. Phase 67.4 will swap these flags for\n\
                     `--socket <path>` to wire the bin to the daemon's decider."
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
                 Phase 67.4 wires --socket to the daemon."
            );
            BinDecider::AllowAll(AllowAllDecider)
        }
        Mode::DenyAll(reason) => {
            tracing::info!(target: "permission_mcp", "DenyAllDecider active: {reason}");
            BinDecider::DenyAll(DenyAllDecider { reason })
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
}

/// Concrete enum so the bin can carry a single `Arc<BinDecider>`
/// through the server. Phase 67.4 will replace this with a
/// `SocketDecider` variant that talks to the daemon.
enum BinDecider {
    AllowAll(AllowAllDecider),
    DenyAll(DenyAllDecider),
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
        }
    }
}
