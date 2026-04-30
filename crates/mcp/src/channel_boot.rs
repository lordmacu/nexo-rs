//! Phase 80.9 boot helper — wires the channel pipeline together.
//!
//! Composes the four building blocks shipped in this phase into
//! the smallest set of handles `main.rs` has to manage:
//!
//! ```text
//! per process:
//!   ChannelBridge   → consumes mcp.channel.>  → resolves session → sink
//!   SharedChannelRegistry  ← registered handlers live here
//!
//! per (binding, server) pair:
//!   ChannelInboundLoop   → gate-once + per-message parse + dispatch
//! ```
//!
//! The helper is intentionally side-effect-free at construction:
//! it builds value types (`ChannelBootContext`,
//! `ChannelInboundLoopConfig` per server) so the caller can spawn
//! them at the right point in the boot sequence and own the
//! cancellation tokens.

use std::sync::Arc;

use nexo_broker::AnyBroker;
use nexo_config::types::channels::{ApprovedChannel, ChannelsConfig};

use crate::channel::{
    BrokerChannelDispatcher, ChannelDispatcher, ChannelInboundLoopConfig, ChannelRegistry,
    SharedChannelRegistry,
};
use crate::channel_bridge::{
    ChannelBridge, ChannelBridgeConfig, ChannelInboundSink, InMemorySessionRegistry,
    SessionRegistry,
};

/// Operator-level context for wiring channels into a process.
/// Cheap to clone (everything is `Arc`).
#[derive(Clone)]
pub struct ChannelBootContext {
    pub broker: AnyBroker,
    pub registry: SharedChannelRegistry,
    pub session_registry: Arc<dyn SessionRegistry>,
    pub dispatcher: Arc<dyn ChannelDispatcher>,
}

impl ChannelBootContext {
    /// Build the default in-process context: `BrokerChannelDispatcher`
    /// over the supplied broker, `InMemorySessionRegistry`,
    /// fresh `ChannelRegistry`.
    pub fn in_memory(broker: AnyBroker) -> Self {
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        let session_registry: Arc<dyn SessionRegistry> = Arc::new(InMemorySessionRegistry::new());
        let dispatcher: Arc<dyn ChannelDispatcher> =
            Arc::new(BrokerChannelDispatcher::new(broker.clone()));
        Self {
            broker,
            registry,
            session_registry,
            dispatcher,
        }
    }

    /// Convenience: build the [`ChannelBridgeConfig`] for the
    /// caller's `sink`. Subject + GC defaults are kept; override
    /// fields on the returned struct if needed before spawning.
    pub fn bridge_config(&self, sink: Arc<dyn ChannelInboundSink>) -> ChannelBridgeConfig {
        ChannelBridgeConfig::new(self.broker.clone(), self.session_registry.clone(), sink)
    }

    /// Spawn the bridge with `sink` and return the handle. The
    /// caller decides shutdown timing through `cancel`.
    pub async fn spawn_bridge(
        &self,
        sink: Arc<dyn ChannelInboundSink>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::channel_bridge::ChannelBridgeHandle, nexo_broker::types::BrokerError> {
        let cfg = self.bridge_config(sink);
        ChannelBridge::new(cfg).spawn(cancel).await
    }
}

/// Build the per-server [`ChannelInboundLoopConfig`] for a single
/// `(binding, server)` pair. Pure — does not spawn the loop.
/// The caller decides spawn timing (typically right after the
/// MCP handshake completes for the server).
pub fn build_inbound_loop_config(
    ctx: &ChannelBootContext,
    server_name: impl Into<String>,
    binding_id: impl Into<String>,
    plugin_source: Option<String>,
    cfg: Arc<ChannelsConfig>,
    binding_allowlist: Arc<Vec<String>>,
    capability_declared: bool,
    permission_capability: bool,
) -> ChannelInboundLoopConfig {
    ChannelInboundLoopConfig {
        server_name: server_name.into(),
        binding_id: binding_id.into(),
        plugin_source,
        cfg,
        binding_allowlist,
        capability_declared,
        permission_capability,
        registry: ctx.registry.clone(),
        dispatcher: ctx.dispatcher.clone(),
    }
}

/// Convenience: pull every operator-approved entry from
/// [`ChannelsConfig`] that matches a binding's allowlist. Returns
/// the (server, plugin_source-snapshot) pairs the caller should
/// build inbound loops for.
pub fn enumerate_targets<'a>(
    cfg: &'a ChannelsConfig,
    binding_allowlist: &'a [String],
) -> Vec<&'a ApprovedChannel> {
    cfg.approved
        .iter()
        .filter(|e| binding_allowlist.iter().any(|s| s == &e.server))
        .collect()
}

/// Phase 80.9.f — build a [`crate::channel::ReevaluateInputs`]
/// from a flat list of `(binding_id, ChannelsConfig,
/// allowed_servers)` tuples. Caller produces this from the
/// freshly-loaded YAML inside the Phase 18 reload post-hook.
pub fn build_reevaluate_inputs(
    bindings: impl IntoIterator<Item = (String, Arc<ChannelsConfig>, Vec<String>)>,
) -> crate::channel::ReevaluateInputs {
    use crate::channel::ReevaluateBinding;
    let mut by_binding = std::collections::HashMap::new();
    for (binding_id, cfg, allowed) in bindings {
        by_binding.insert(
            binding_id,
            ReevaluateBinding {
                cfg,
                allowed_servers: allowed,
            },
        );
    }
    crate::channel::ReevaluateInputs { by_binding }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_bridge::{ChannelInboundEvent, SinkError};
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct NoopSink {
        seen: StdMutex<Vec<ChannelInboundEvent>>,
    }

    #[async_trait]
    impl ChannelInboundSink for NoopSink {
        async fn deliver(&self, event: ChannelInboundEvent) -> Result<(), SinkError> {
            self.seen.lock().unwrap().push(event);
            Ok(())
        }
    }

    #[tokio::test]
    async fn in_memory_context_builds_with_local_broker() {
        let broker = AnyBroker::local();
        let ctx = ChannelBootContext::in_memory(broker);
        assert_eq!(ctx.registry.count().await, 0);
        assert_eq!(ctx.session_registry.len().await, 0);
    }

    #[tokio::test]
    async fn bridge_config_uses_supplied_sink() {
        let broker = AnyBroker::local();
        let ctx = ChannelBootContext::in_memory(broker);
        let sink: Arc<dyn ChannelInboundSink> = Arc::new(NoopSink::default());
        let cfg = ctx.bridge_config(sink);
        assert_eq!(cfg.subject, "mcp.channel.>");
        assert_eq!(cfg.gc_interval_ms, 5 * 60_000);
        assert_eq!(cfg.max_idle_ms, 60 * 60_000);
    }

    #[test]
    fn enumerate_targets_filters_by_binding_allowlist() {
        let cfg = ChannelsConfig {
            enabled: true,
            approved: vec![
                ApprovedChannel {
                    server: "slack".into(),
                    plugin_source: None,
                    outbound_tool_name: None,
            rate_limit: None,
                },
                ApprovedChannel {
                    server: "telegram".into(),
                    plugin_source: None,
                    outbound_tool_name: None,
            rate_limit: None,
                },
                ApprovedChannel {
                    server: "imessage".into(),
                    plugin_source: None,
                    outbound_tool_name: None,
            rate_limit: None,
                },
            ],
            ..Default::default()
        };
        let allow = vec!["slack".to_string(), "imessage".to_string()];
        let targets = enumerate_targets(&cfg, &allow);
        assert_eq!(targets.len(), 2);
        assert!(targets.iter().any(|e| e.server == "slack"));
        assert!(targets.iter().any(|e| e.server == "imessage"));
        assert!(!targets.iter().any(|e| e.server == "telegram"));
    }

    #[test]
    fn build_inbound_loop_config_threads_handles_through() {
        let broker = AnyBroker::local();
        let ctx = ChannelBootContext::in_memory(broker);
        let cfg = Arc::new(ChannelsConfig::default());
        let allow = Arc::new(vec!["slack".to_string()]);
        let lc = build_inbound_loop_config(
            &ctx,
            "slack",
            "wp:default",
            Some("slack@anthropic".into()),
            cfg,
            allow,
            true,
            false,
        );
        assert_eq!(lc.server_name, "slack");
        assert_eq!(lc.binding_id, "wp:default");
        assert!(lc.capability_declared);
        assert_eq!(lc.plugin_source.as_deref(), Some("slack@anthropic"));
    }

    #[tokio::test]
    async fn spawn_bridge_returns_handle() {
        let broker = AnyBroker::local();
        let ctx = ChannelBootContext::in_memory(broker);
        let sink: Arc<dyn ChannelInboundSink> = Arc::new(NoopSink::default());
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle = ctx.spawn_bridge(sink, cancel.clone()).await.unwrap();
        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), handle.consumer)
            .await;
    }
}
