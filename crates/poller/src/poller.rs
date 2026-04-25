//! Trait + value types every built-in module implements. Kept tiny on
//! purpose — the runner owns scheduling, dispatch, and persistence.

use std::sync::Arc;
use std::time::Duration;

use agent_auth::resolver::CredentialStores;
use agent_auth::{AgentCredentialResolver, Channel};
use agent_broker::AnyBroker;
use agent_llm::ToolDef;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::PollerError;
use crate::PollerRunner;

/// Implemented by every poller module (gmail, rss, calendar, …).
/// `Send + Sync + 'static` because the runner stores `Arc<dyn Poller>`
/// and spawns tasks against them.
#[async_trait]
pub trait Poller: Send + Sync + 'static {
    /// Discriminator used in YAML (`kind: gmail`) and metrics.
    fn kind(&self) -> &'static str;

    /// Human label for `agent pollers list`. Defaults to empty.
    fn description(&self) -> &'static str {
        ""
    }

    /// Validate the per-job `config` JSON at boot. Errors fail loading
    /// of that job only; siblings keep going. Default: accept anything
    /// (the module trusts its own deserialization in `tick`).
    fn validate(&self, _config: &Value) -> Result<(), PollerError> {
        Ok(())
    }

    /// Run one tick. Returns the items dispatched + the new cursor +
    /// optional next-interval hint. Errors flow into the runner's
    /// backoff / breaker / pause logic via [`PollerError::classify`].
    async fn tick(&self, ctx: &PollContext) -> Result<TickOutcome, PollerError>;

    /// Optional: per-kind LLM tools to register alongside the six
    /// generic `pollers_*` tools. Use this for kind-specific
    /// affordances — e.g. `gmail_validate_query` to dry-run a Gmail
    /// search, `rss_check_feed` to ping a feed URL without spawning
    /// the job, `calendar_sync_token_age` to inspect a syncToken.
    /// Default: empty.
    ///
    /// The tools wrap a closure that receives the same `PollerRunner`
    /// the runtime uses, so they can list / inspect / mutate state
    /// the same way the built-in `pollers_*` tools do.
    fn custom_tools(&self) -> Vec<CustomToolSpec> {
        Vec::new()
    }
}

/// One per-kind tool. Returned by [`Poller::custom_tools`] and
/// adapted into an `agent-core` `ToolHandler` by the
/// `agent-poller-tools` crate at registration time.
pub struct CustomToolSpec {
    pub def: ToolDef,
    pub handler: Arc<dyn CustomToolHandler>,
}

/// Local handler trait — kept inside `agent-poller` so the crate
/// stays free of `agent-core` (and therefore free of the
/// `plugin-google ↔ core` cycle). The adapter lives in
/// `agent-poller-tools`.
#[async_trait]
pub trait CustomToolHandler: Send + Sync + 'static {
    async fn call(
        &self,
        runner: Arc<PollerRunner>,
        args: Value,
    ) -> anyhow::Result<Value>;
}

/// What the runner hands a module on every tick.
pub struct PollContext {
    pub job_id: String,
    pub agent_id: String,
    pub kind: &'static str,
    /// Phase 17 resolver — modules use it to find the agent's
    /// credential handle for whichever channel they need (typically
    /// `GOOGLE` for inbound fetch, then `WHATSAPP`/`TELEGRAM` for
    /// outbound, but the trait is channel-agnostic).
    pub credentials: Arc<AgentCredentialResolver>,
    /// Read-only handles to the per-channel credential stores. Pollers
    /// that need paths (gmail → Google client_id_path / token_path,
    /// future calendar → same store) borrow this. `None` in test
    /// fixtures that don't wire a full bundle.
    pub stores: Option<Arc<CredentialStores>>,
    /// Local broker handle. Modules SHOULD NOT publish directly —
    /// return `OutboundDelivery` and let the runner dispatch.
    /// Exposed in case a module needs to subscribe (rare).
    pub broker: AnyBroker,
    pub now: DateTime<Utc>,
    /// Opaque cursor returned by the previous successful tick. `None`
    /// on first run, after `agent pollers reset <id>`, or after a
    /// `Permanent` error that cleared state.
    pub cursor: Option<Vec<u8>>,
    /// The `config:` block from `pollers.yaml` for this job.
    pub config: Value,
    /// Schedule's nominal interval, before jitter. Useful for modules
    /// that want to compute their own internal pacing (e.g. RSS feeds
    /// honoring `<ttl>`).
    pub interval_hint: Duration,
    /// Cancelled when the runner is shutting down OR when this job is
    /// being hot-reloaded. Modules with long inner loops (HTTP retries,
    /// pagination) should `tokio::select!` on this so reload doesn't
    /// hang behind a stuck request.
    pub cancel: CancellationToken,
    /// Phase 20 — LLM access for the `agent_turn` built-in (and any
    /// future module that needs to call a model). `None` when the
    /// runner was not wired with `with_llm` at boot — modules MUST
    /// surface a clean `PollerError::Config` in that case rather than
    /// panicking.
    pub llm_registry: Option<Arc<agent_llm::LlmRegistry>>,
    pub llm_config: Option<Arc<agent_config::LlmConfig>>,
}

/// What a module returns on a successful tick.
#[derive(Debug, Default)]
pub struct TickOutcome {
    /// Items the module saw at the source (matches the query / regex
    /// / feed). For metrics — not all of them have to dispatch.
    pub items_seen: u32,
    /// Items actually included in `deliver`. Equals `deliver.len()`
    /// if the module hand-built it.
    pub items_dispatched: u32,
    /// Outbound payloads the runner will publish to the right topic.
    pub deliver: Vec<OutboundDelivery>,
    /// Persisted as the next tick's `cursor`. None means "keep what
    /// you had". To reset, return `Some(vec![])` or call `reset` from
    /// admin.
    pub next_cursor: Option<Vec<u8>>,
    /// Override the next interval just for this scheduling slot. None
    /// uses the configured schedule.
    pub next_interval_hint: Option<Duration>,
}

/// Channel-agnostic outbound payload. The runner resolves the target
/// instance from the agent's `credentials.<channel>` binding and
/// publishes to `plugin.outbound.<channel>.<instance>`.
#[derive(Debug, Clone)]
pub struct OutboundDelivery {
    pub channel: Channel,
    pub recipient: String,
    pub payload: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn poll_context_is_send_sync() {
        assert_send_sync::<PollContext>();
    }

    #[test]
    fn tick_outcome_default_is_empty() {
        let o = TickOutcome::default();
        assert_eq!(o.items_seen, 0);
        assert_eq!(o.items_dispatched, 0);
        assert!(o.deliver.is_empty());
        assert!(o.next_cursor.is_none());
        assert!(o.next_interval_hint.is_none());
    }
}
