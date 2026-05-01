use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunTrigger {
    User,
    Heartbeat,
    Manual,
    Tick,
}

/// Inbound queue priority for autonomous runtime processing.
///
/// Semantics mirror Claude Code's queue model:
/// - `Now`: highest priority (operator/user urgent interrupt)
/// - `Next`: default priority (regular inbound user message)
/// - `Later`: deferred notifications / background chatter
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MessagePriority {
    Now,
    #[default]
    Next,
    Later,
}

impl MessagePriority {
    pub fn rank(self) -> u8 {
        match self {
            Self::Now => 0,
            Self::Next => 1,
            Self::Later => 2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Now => "now",
            Self::Next => "next",
            Self::Later => "later",
        }
    }
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
    /// Queue priority used by the per-session runtime debounce loop.
    pub priority: MessagePriority,
    /// Pairing trust bit stamped by runtime intake for user-triggered
    /// inbound events. Defaults to false for non-chat paths (delegate,
    /// heartbeat, proactive ticks) so trusted-only dispatch fails closed
    /// unless intake explicitly marked the sender as admitted.
    pub sender_trusted: bool,
    /// Phase 82.5 — per-turn inbound metadata (provider-native
    /// sender id, msg id, reply target, has_media flag, …) built
    /// at the intake site from the raw `InboundEvent` payload.
    /// Carried on the message so the per-turn dispatch loop can
    /// stamp it on the cloned `AgentContext` before invoking the
    /// LLM / tools (see `extension_tool::inject_context_meta`).
    /// `None` for legacy paths not yet migrated.
    pub inbound: Option<nexo_tool_meta::InboundMessageMeta>,
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
            priority: MessagePriority::Next,
            sender_trusted: false,
            inbound: None,
        }
    }
}
