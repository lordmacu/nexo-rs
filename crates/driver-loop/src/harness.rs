//! `ClaudeHarness` — closes the `AgentHarness` contract from 67.0
//! by delegating to `attempt::run_attempt`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use nexo_driver_claude::{ClaudeConfig, SessionBindingStore};
use nexo_driver_types::{
    AgentHarness, AttemptParams, AttemptResult, HarnessError, Support, SupportContext,
};

use crate::acceptance::{AcceptanceEvaluator, NoopAcceptanceEvaluator};

fn default_acceptance() -> Arc<dyn AcceptanceEvaluator> {
    Arc::new(NoopAcceptanceEvaluator)
}
use crate::attempt::{run_attempt, AttemptContext};
use crate::error::DriverError;
use crate::workspace::WorkspaceManager;

pub struct ClaudeHarness {
    config: ClaudeConfig,
    binding_store: Arc<dyn SessionBindingStore>,
    workspace_manager: Arc<WorkspaceManager>,
    acceptance: Arc<dyn AcceptanceEvaluator>,
    bin_path: PathBuf,
    socket_path: PathBuf,
}

impl ClaudeHarness {
    pub fn new(
        config: ClaudeConfig,
        binding_store: Arc<dyn SessionBindingStore>,
        workspace_manager: Arc<WorkspaceManager>,
        bin_path: PathBuf,
        socket_path: PathBuf,
    ) -> Self {
        Self {
            config,
            binding_store,
            workspace_manager,
            acceptance: default_acceptance(),
            bin_path,
            socket_path,
        }
    }

    pub fn with_acceptance(mut self, ae: Arc<dyn AcceptanceEvaluator>) -> Self {
        self.acceptance = ae;
        self
    }
}

#[async_trait]
impl AgentHarness for ClaudeHarness {
    fn id(&self) -> &str {
        "claude-code"
    }
    fn label(&self) -> &str {
        "Claude Code (Anthropic)"
    }
    fn supports(&self, ctx: &SupportContext) -> Support {
        if ctx.provider == "claude-code" {
            Support::Supported {
                priority: 100,
                reason: None,
            }
        } else {
            Support::Unsupported {
                reason: format!("unknown provider: {}", ctx.provider),
            }
        }
    }
    async fn run_attempt(&self, params: AttemptParams) -> Result<AttemptResult, HarnessError> {
        let workspace = self
            .workspace_manager
            .ensure(&params.goal)
            .await
            .map_err(|e: DriverError| HarnessError::Other(e.to_string()))?;
        let mcp_path =
            crate::mcp_config::write_mcp_config(&workspace, &self.bin_path, &self.socket_path)
                .map_err(|e| HarnessError::Other(e.to_string()))?;
        let cancel = params.cancel.clone();
        let ctx = AttemptContext {
            claude_cfg: &self.config,
            binding_store: &self.binding_store,
            acceptance: &self.acceptance,
            workspace: &workspace,
            mcp_config_path: &mcp_path,
            bin_path: &self.bin_path,
            cancel,
        };
        run_attempt(ctx, params).await.map_err(|e| match e {
            DriverError::Harness(h) => h,
            other => HarnessError::Other(other.to_string()),
        })
    }
}
