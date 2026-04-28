//! Payloads published on `plugin.inbound.telegram`.

use serde::{Deserialize, Serialize};

/// Normalized description of an inbound media attachment. The plugin
/// downloads the file to `local_path` before publishing so downstream
/// skills (whisper, OCR, PDF extract, vision LLMs) can consume it by path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaDescriptor {
    pub kind: String,
    pub local_path: String,
    pub file_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

/// Minimal forward metadata so the agent can tell "user wrote this"
/// from "user forwarded this from someone else". Useful for trust
/// checks (don't execute tool calls based on forwarded text) and for
/// provenance-aware summarisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardInfo {
    /// Human-readable source description — `"@alice"`, `"chat:Anouncements"`,
    /// etc. Best-effort; may be empty if the origin is anonymous.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_user_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_chat_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<i64>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboundEvent {
    Message {
        from: String,
        chat: String,
        chat_type: String,
        text: Option<String>,
        reply_to: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_question_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ask_question_id: Option<String>,
        is_group: bool,
        timestamp: i64,
        msg_id: String,
        username: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media: Vec<MediaDescriptor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        latitude: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        longitude: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forward: Option<ForwardInfo>,
    },
    /// Inline-keyboard button press. The plugin auto-ACKs it (empty
    /// toast) before publishing so Telegram's spinner disappears; the
    /// event itself is informational for the agent runtime.
    CallbackQuery {
        from: String,
        chat: String,
        data: String,
        /// Message id the button was attached to (when known).
        msg_id: Option<String>,
        username: Option<String>,
        callback_id: String,
    },
    /// Bot's membership in a chat changed: added to a group, kicked,
    /// user blocked the bot, user unblocked / started DM, etc. Useful
    /// for cleanup (drop per-chat state when kicked) or onboarding
    /// (send welcome when user starts the DM).
    ChatMembership {
        chat: String,
        chat_title: Option<String>,
        old_status: String,
        new_status: String,
        changed_by: String,
        timestamp: i64,
    },
    Connected {
        bot_username: String,
        bot_id: i64,
    },
    Disconnected {
        reason: String,
    },
    BridgeTimeout {
        session_id: uuid::Uuid,
    },
}

impl InboundEvent {
    pub fn to_payload(&self) -> serde_json::Value {
        let v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Self::Message {
            from,
            text,
            media,
            forward,
            ..
        } = self
        {
            let mut obj = match v {
                serde_json::Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            if !obj.contains_key("from") {
                obj.insert("from".into(), serde_json::Value::String(from.clone()));
            }
            if !obj.contains_key("text") {
                if let Some(t) = text {
                    obj.insert("text".into(), serde_json::Value::String(t.clone()));
                }
            }
            // Promote media local_path to a top-level convenience field
            // so agents can branch on its presence without matching the
            // full MediaDescriptor object.
            if let Some(m) = media.first() {
                obj.insert(
                    "media_kind".into(),
                    serde_json::Value::String(m.kind.clone()),
                );
                obj.insert(
                    "media_path".into(),
                    serde_json::Value::String(m.local_path.clone()),
                );
            }
            // Flatten forward origin as a top-level `forwarded_from`
            // hint so the agent can branch on it without matching the
            // full ForwardInfo struct. Useful for "don't execute tools
            // based on forwarded text" heuristics.
            if let Some(f) = forward {
                obj.insert(
                    "forwarded_from".into(),
                    serde_json::Value::String(f.source.clone()),
                );
            }
            return serde_json::Value::Object(obj);
        }
        v
    }
}
