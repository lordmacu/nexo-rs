use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::handle::{BrokerHandle, Subscription};
use crate::topic::topic_matches;
use crate::types::{BrokerError, Event, Message};

const CHANNEL_CAPACITY: usize = 256;

type SubMap = Arc<DashMap<Uuid, (String, mpsc::Sender<Event>)>>;

#[derive(Clone, Default)]
pub struct LocalBroker {
    subs: SubMap,
}

impl LocalBroker {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl BrokerHandle for LocalBroker {
    async fn publish(&self, topic: &str, event: Event) -> Result<(), BrokerError> {
        use tokio::sync::mpsc::error::TrySendError;
        let mut dead: Vec<Uuid> = Vec::new();

        for entry in self.subs.iter() {
            let (pattern, tx) = entry.value();
            if !topic_matches(pattern, topic) {
                continue;
            }
            match tx.try_send(event.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    // Subscriber is backlogged. Drop this one event and
                    // log; do NOT remove the subscription — it may catch
                    // up. Matches the NATS semantics of "slow consumer"
                    // where messages are shed but the sub stays live.
                    tracing::warn!(
                        topic,
                        pattern,
                        "local broker dropping event: subscriber channel full (slow consumer)"
                    );
                }
                Err(TrySendError::Closed(_)) => {
                    // Receiver side was dropped — sub is truly dead.
                    dead.push(*entry.key());
                }
            }
        }

        for id in dead {
            self.subs.remove(&id);
        }

        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<Subscription, BrokerError> {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let id = Uuid::new_v4();
        self.subs.insert(id, (topic.to_string(), tx));
        Ok(Subscription::new(topic.to_string(), rx))
    }

    async fn request(
        &self,
        topic: &str,
        mut msg: Message,
        timeout: Duration,
    ) -> Result<Message, BrokerError> {
        let inbox = format!("_inbox.{}", Uuid::new_v4());
        msg.reply_to = Some(inbox.clone());

        let mut sub = self.subscribe(&inbox).await?;

        let event = Event::new(
            topic,
            "local",
            serde_json::to_value(&msg).map_err(|e| BrokerError::SendError(e.to_string()))?,
        );
        self.publish(topic, event).await?;

        match tokio::time::timeout(timeout, sub.next()).await {
            Ok(Some(reply_event)) => {
                let reply_msg: Message =
                    serde_json::from_value(reply_event.payload).map_err(|e| {
                        BrokerError::SendError(format!("failed to deserialize reply: {e}"))
                    })?;
                Ok(reply_msg)
            }
            Ok(None) => Err(BrokerError::RequestTimeout(topic.to_string())),
            Err(_) => Err(BrokerError::RequestTimeout(topic.to_string())),
        }
    }
}
