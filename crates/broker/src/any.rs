use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use nexo_config::types::broker::{BrokerInner, BrokerKind};

use crate::handle::{BrokerHandle, Subscription};
use crate::local::LocalBroker;
use crate::nats::NatsBroker;
use crate::stdio_bridge::StdioBridgeBroker;
use crate::types::{BrokerError, Event, Message};

#[derive(Clone)]
pub enum AnyBroker {
    Local(LocalBroker),
    Nats(Arc<NatsBroker>),
    /// Subprocess-side broker that pipes through the
    /// parent daemon's stdio JSON-RPC channel. Constructed via
    /// [`AnyBroker::stdio_bridge`] rather than `from_config`
    /// because building one requires a writer handle (typically
    /// the SDK's `PluginAdapter`-owned stdout) and a stdin
    /// dispatcher to feed `handle_inbound_line`; both live one
    /// layer up in `nexo-microapp-sdk`.
    StdioBridge(StdioBridgeBroker),
}

impl AnyBroker {
    pub async fn from_config(cfg: &BrokerInner) -> anyhow::Result<Self> {
        match cfg.kind {
            BrokerKind::Nats => {
                let broker = NatsBroker::connect(cfg).await?;
                Ok(Self::Nats(Arc::new(broker)))
            }
            BrokerKind::Local => Ok(Self::Local(LocalBroker::new())),
            BrokerKind::StdioBridge => {
                // `from_config` cannot wire the stdio bridge
                // because it has no access to the plugin
                // adapter's outbound mpsc Sender. Callers needing
                // a bridge-backed broker must build one via
                // `nexo-microapp-sdk::plugin::PluginAdapter::
                // with_stdio_bridge_broker` (which returns the
                // wired adapter + an `Arc<StdioBridgeBroker>`)
                // and wrap it in `AnyBroker::stdio_bridge`.
                anyhow::bail!(
                    "BrokerKind::StdioBridge cannot be built from config; \
                     use PluginAdapter::with_stdio_bridge_broker() in the plugin's main.rs"
                )
            }
        }
    }

    pub fn local() -> Self {
        Self::Local(LocalBroker::new())
    }

    /// Wrap a pre-constructed [`StdioBridgeBroker`]. The caller
    /// retains a clone of the inner bridge so it can feed inbound
    /// lines via [`StdioBridgeBroker::handle_inbound_line`]
    /// (typically from the SDK's stdin multiplexer).
    pub fn stdio_bridge(bridge: StdioBridgeBroker) -> Self {
        Self::StdioBridge(bridge)
    }

    pub fn is_ready(&self) -> bool {
        match self {
            Self::Local(_) => true,
            Self::Nats(b) => b.is_connected(),
            // The bridge is "ready" as soon as the stdio handle
            // is wired. Daemon-side reachability is verified
            // implicitly on the first `publish` reply or by a
            // higher-level liveness probe.
            Self::StdioBridge(_) => true,
        }
    }
}

#[async_trait]
impl BrokerHandle for AnyBroker {
    async fn publish(&self, topic: &str, event: Event) -> Result<(), BrokerError> {
        match self {
            Self::Local(b) => b.publish(topic, event).await,
            Self::Nats(b) => b.publish(topic, event).await,
            Self::StdioBridge(b) => b.publish(topic, event).await,
        }
    }

    async fn subscribe(&self, topic: &str) -> Result<Subscription, BrokerError> {
        match self {
            Self::Local(b) => b.subscribe(topic).await,
            Self::Nats(b) => b.subscribe(topic).await,
            Self::StdioBridge(b) => b.subscribe(topic).await,
        }
    }

    async fn request(
        &self,
        topic: &str,
        msg: Message,
        timeout: Duration,
    ) -> Result<Message, BrokerError> {
        match self {
            Self::Local(b) => b.request(topic, msg, timeout).await,
            Self::Nats(b) => b.request(topic, msg, timeout).await,
            Self::StdioBridge(b) => b.request(topic, msg, timeout).await,
        }
    }
}
