use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use nexo_config::types::broker::{BrokerInner, BrokerKind};

use crate::handle::{BrokerHandle, Subscription};
use crate::local::LocalBroker;
use crate::nats::NatsBroker;
use crate::types::{BrokerError, Event, Message};

#[derive(Clone)]
pub enum AnyBroker {
    Local(LocalBroker),
    Nats(Arc<NatsBroker>),
}

impl AnyBroker {
    pub async fn from_config(cfg: &BrokerInner) -> anyhow::Result<Self> {
        match cfg.kind {
            BrokerKind::Nats => {
                let broker = NatsBroker::connect(cfg).await?;
                Ok(Self::Nats(Arc::new(broker)))
            }
            BrokerKind::Local => Ok(Self::Local(LocalBroker::new())),
        }
    }

    pub fn local() -> Self {
        Self::Local(LocalBroker::new())
    }

    pub fn is_ready(&self) -> bool {
        match self {
            Self::Local(_) => true,
            Self::Nats(b) => b.is_connected(),
        }
    }
}

#[async_trait]
impl BrokerHandle for AnyBroker {
    async fn publish(&self, topic: &str, event: Event) -> Result<(), BrokerError> {
        match self {
            Self::Local(b) => b.publish(topic, event).await,
            Self::Nats(b) => b.publish(topic, event).await,
        }
    }

    async fn subscribe(&self, topic: &str) -> Result<Subscription, BrokerError> {
        match self {
            Self::Local(b) => b.subscribe(topic).await,
            Self::Nats(b) => b.subscribe(topic).await,
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
        }
    }
}
