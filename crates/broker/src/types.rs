use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub topic: String,
    pub source: String,
    pub session_id: Option<Uuid>,
    pub payload: serde_json::Value,
}

impl Event {
    pub fn new(
        topic: impl Into<String>,
        source: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            topic: topic.into(),
            source: source.into(),
            session_id: None,
            payload,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub topic: String,
    pub reply_to: Option<String>,
    pub payload: serde_json::Value,
}

impl Message {
    pub fn new(topic: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            id: Uuid::new_v4(),
            topic: topic.into(),
            reply_to: None,
            payload,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("request timed out on topic '{0}'")]
    RequestTimeout(String),
    #[error("failed to send event to subscriber: {0}")]
    SendError(String),
    #[error("subscribe failed: {0}")]
    SubscribeError(String),
}
