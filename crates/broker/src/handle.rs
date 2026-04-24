use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::types::{BrokerError, Event, Message};

pub struct Subscription {
    pub topic: String,
    receiver: mpsc::Receiver<Event>,
}

impl Subscription {
    pub(crate) fn new(topic: String, receiver: mpsc::Receiver<Event>) -> Self {
        Self { topic, receiver }
    }

    pub async fn next(&mut self) -> Option<Event> {
        self.receiver.recv().await
    }
}

#[async_trait]
pub trait BrokerHandle: Send + Sync {
    async fn publish(&self, topic: &str, event: Event) -> Result<(), BrokerError>;
    async fn subscribe(&self, topic: &str) -> Result<Subscription, BrokerError>;
    async fn request(
        &self,
        topic: &str,
        msg: Message,
        timeout: Duration,
    ) -> Result<Message, BrokerError>;
}
