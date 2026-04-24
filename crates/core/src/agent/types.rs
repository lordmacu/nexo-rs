use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunTrigger {
    User,
    Heartbeat,
    Manual,
}
#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_id: String,
    pub sender_id: Option<String>,
    pub text: String,
    pub trigger: RunTrigger,
    pub timestamp: DateTime<Utc>,
    /// Plugin that delivered this message (e.g. "whatsapp", "telegram").
    /// Used to route the reply back to the correct outbound topic.
    pub source_plugin: String,
    /// Optional plugin instance (e.g. "bot_sales" when a plugin exposes
    /// multiple bots/accounts). `None` when the plugin is single-instance
    /// or the topic didn't include an instance suffix. Used for routing
    /// filters and multi-account bookkeeping.
    pub source_instance: Option<String>,
    /// Optional media attachment resolved from the inbound plugin event.
    /// Populated when the payload carries `media_path` + `media_kind`
    /// (telegram voice/photo/video/document, whatsapp media, etc.).
    pub media: Option<InboundMedia>,
}
/// Normalized media reference the agent runtime can pass to LLMs (via
/// `Attachment`) or downstream skills (whisper/OCR/pdf-extract).
#[derive(Debug, Clone)]
pub struct InboundMedia {
    pub kind: String,
    pub path: String,
    pub mime_type: Option<String>,
}
impl InboundMessage {
    pub fn new(session_id: Uuid, agent_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            agent_id: agent_id.into(),
            sender_id: None,
            text: text.into(),
            trigger: RunTrigger::User,
            timestamp: Utc::now(),
            source_plugin: String::new(),
            source_instance: None,
            media: None,
        }
    }
}
