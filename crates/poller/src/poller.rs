//! Trait + context every Poller implements. Kept Laravel-style:
//! the runner is a dumb scheduler — the trait knows nothing about
//! channels, credentials, or outbound topics. Pollers reach the world
//! through [`PollerHost`], which is the single point of egress.
//!
//! Compared to the pre-Phase-96 shape, this module no longer exports
//! `OutboundDelivery` / `TickOutcome` / channel-typed bundle fields.
//! Those concerns live with the poller now.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nexo_llm::ToolDef;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::PollerError;
use crate::host::{PollerHost, TickAck};
use crate::PollerRunner;

/// Implemented by every poller module (built-in or out-of-tree).
/// `Send + Sync + 'static` because the runner stores `Arc<dyn Poller>`
/// and spawns tasks against them.
#[async_trait]
pub trait Poller: Send + Sync + 'static {
    /// Discriminator used in YAML (`kind: rss`) and metrics.
    fn kind(&self) -> &'static str;

    /// Human label for `agent pollers list`. Defaults to empty.
    fn description(&self) -> &'static str {
        ""
    }

    /// Validate the per-job `config` JSON at boot. Errors fail loading
    /// of that job only; siblings keep going. Default: accept anything.
    fn validate(&self, _config: &Value) -> Result<(), PollerError> {
        Ok(())
    }

    /// Run one tick. The runner persists the returned cursor and
    /// honors the interval hint; anything the poller wants to send
    /// outbound (broker publish, LLM call, credential lookup) goes
    /// through `ctx.host`.
    async fn tick(&self, ctx: &PollContext) -> Result<TickAck, PollerError>;

    /// Optional per-kind LLM tools registered alongside the generic
    /// `pollers_*` tools. Pollers without custom tools leave this
    /// empty (default).
    fn custom_tools(&self) -> Vec<CustomToolSpec> {
        Vec::new()
    }
}

/// Per-kind LLM tool spec — adapted into an `nexo-core` `ToolHandler`
/// by the `nexo-poller-tools` crate at registration time.
pub struct CustomToolSpec {
    pub def: ToolDef,
    pub handler: Arc<dyn CustomToolHandler>,
}

/// Local handler trait — kept inside `nexo-poller` so the crate
/// stays free of `nexo-core`. The adapter lives in `nexo-poller-tools`.
#[async_trait]
pub trait CustomToolHandler: Send + Sync + 'static {
    async fn call(&self, runner: Arc<PollerRunner>, args: Value) -> anyhow::Result<Value>;
}

/// What the runner hands a poller on every tick. Compared to V1:
/// dropped `credentials`, `bundle`, `broker`, `llm_registry`,
/// `llm_config`. Added `host` — the single egress.
pub struct PollContext {
    pub job_id: String,
    pub agent_id: String,
    pub kind: &'static str,
    /// The `config:` block from `pollers.yaml` for this job.
    pub config: Value,
    /// Opaque cursor returned by the previous successful tick. `None`
    /// on first run, after `agent pollers reset <id>`, or after a
    /// `Permanent` error that cleared state.
    pub cursor: Option<Vec<u8>>,
    pub now: DateTime<Utc>,
    /// Schedule's nominal interval, before jitter. Useful for pollers
    /// that pace internally (RSS feeds honoring `<ttl>`).
    pub interval_hint: Duration,
    /// Cancelled when the runner is shutting down OR when this job is
    /// hot-reloaded. Pollers with long inner loops (HTTP retries,
    /// pagination) should `tokio::select!` on this so reload doesn't
    /// hang behind a stuck request.
    pub cancel: CancellationToken,
    /// Single egress for runtime-level concerns. Pollers use this for
    /// broker publishes, credential resolution, LLM invocations,
    /// logging, and metrics.
    pub host: Arc<dyn PollerHost>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn poll_context_is_send_sync() {
        assert_send_sync::<PollContext>();
    }
}
