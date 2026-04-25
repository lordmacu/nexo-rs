//! `AgentHarness` — the trait every driver impl conforms to.

use async_trait::async_trait;

use crate::attempt::{AttemptParams, AttemptResult, CompactParams, CompactResult, ResetParams};
use crate::error::HarnessError;
use crate::support::{Support, SupportContext};

/// Drives one or more attempts against a goal. Implementors live in
/// downstream crates (`nexo-driver-claude`, `nexo-driver-codex`, …).
#[async_trait]
pub trait AgentHarness: Send + Sync + 'static {
    /// Stable, machine-readable id — `"claude-code"`, `"codex-app-server"`.
    fn id(&self) -> &str;

    /// Human-readable label for logs and admin-ui.
    fn label(&self) -> &str;

    /// `Some(plugin_id)` when the harness is registered by a plugin
    /// (Phase 11). `None` for in-tree harnesses.
    fn plugin_id(&self) -> Option<&str> {
        None
    }

    /// Cheap, sync support check. Selector calls this before
    /// dispatching. Implementors return `Support::Unsupported { .. }`
    /// instead of panicking on unfamiliar input.
    fn supports(&self, ctx: &SupportContext) -> Support;

    /// Drive one attempt to completion. Long-running. Implementors
    /// MUST poll `params.cancel.is_cancelled()` between events and
    /// honour `params.goal.budget`.
    async fn run_attempt(&self, params: AttemptParams) -> Result<AttemptResult, HarnessError>;

    /// Optional context-compaction hook.
    async fn compact(&self, _params: CompactParams) -> Result<CompactResult, HarnessError> {
        Ok(CompactResult::skipped("compact not implemented"))
    }

    /// Optional reset hook (clear session bindings, drop cached state).
    async fn reset(&self, _params: ResetParams) -> Result<(), HarnessError> {
        Ok(())
    }

    /// Optional teardown — invoked when the harness is being unloaded.
    async fn dispose(&self) -> Result<(), HarnessError> {
        Ok(())
    }
}
