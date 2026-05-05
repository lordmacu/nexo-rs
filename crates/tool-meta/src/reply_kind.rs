//! `OutboundReplyKind` — channel-agnostic shape for what the agent
//! is about to send back on the wire.
//!
//! The framework's reply pipeline produces a `Text` by default and
//! runs it through a chain of `OutboundReplyTransformer`s before
//! handing it to the channel plugin. Transformers can replace the
//! variant entirely (e.g. `Text → VoiceNote` for a TTS-driven
//! microapp) or pass through unchanged.
//!
//! Channel plugins decide which variants they support. Each plugin
//! must reject unsupported variants with a clear error so operators
//! see the contract violation instead of silent drops.
//!
//! Stays in `tool-meta` — no extension-side dep on `nexo-core`. Both
//! the daemon (which produces and dispatches) and microapps (which
//! transform via tools) can speak this enum without pulling the
//! framework runtime.

use serde::{Deserialize, Serialize};

/// What the agent wants to send back on a channel. One of N kinds;
/// channel plugins map each variant to their native primitive
/// (`send_text`, `send_voice_note`, `send_image`, …).
///
/// `#[serde(tag = "kind")]` keeps the wire shape extensible: future
/// variants land additively without breaking existing JSON
/// deserialisers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutboundReplyKind {
    /// Plain text. Default for new replies; every channel must
    /// support this variant.
    Text {
        /// Message body, UTF-8.
        body: String,
    },
    /// Voice note (PTT on WhatsApp, audio_message on Telegram).
    /// `audio_bytes` is the raw container (mp3/ogg/m4a — caller
    /// chooses, plugin re-encodes if needed). `transcript` is
    /// optional and surfaces in audit logs / takeover dashboards
    /// so operators can see what was said without playing the
    /// audio.
    VoiceNote {
        /// Raw audio bytes. JSON-encoded as base64.
        #[serde(with = "base64_bytes")]
        audio_bytes: Vec<u8>,
        /// MIME type of `audio_bytes` (`audio/mpeg`, `audio/ogg`, …).
        mimetype: String,
        /// Optional plain-text transcript for audit / dashboard.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript: Option<String>,
    },
    /// Image with optional caption.
    Image {
        /// Raw image bytes. JSON-encoded as base64.
        #[serde(with = "base64_bytes")]
        bytes: Vec<u8>,
        /// MIME type of `bytes` (`image/png`, `image/jpeg`, …).
        mimetype: String,
        /// Optional caption rendered alongside the image.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
}

impl OutboundReplyKind {
    /// Convenience for the most common case.
    pub fn text(body: impl Into<String>) -> Self {
        Self::Text { body: body.into() }
    }

    /// Discriminant string used in tracing / admin event audit.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::VoiceNote { .. } => "voice_note",
            Self::Image { .. } => "image",
        }
    }

    /// Plain-text projection for transcript/audit purposes. `Text`
    /// returns the body verbatim; non-text variants return their
    /// caption / transcript when present, empty string otherwise.
    pub fn as_text_summary(&self) -> &str {
        match self {
            Self::Text { body } => body,
            Self::VoiceNote { transcript, .. } => transcript.as_deref().unwrap_or(""),
            Self::Image { caption, .. } => caption.as_deref().unwrap_or(""),
        }
    }
}

/// Context the framework hands transformers + channel plugins for
/// every outbound reply. Read-only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutboundReplyContext {
    /// Stable agent id producing the reply.
    pub agent_id: String,
    /// Session UUID stringified.
    pub session_id: String,
    /// `whatsapp` / `telegram` / `email` / etc.
    pub channel: String,
    /// Plugin instance discriminator (e.g. `smoketest`,
    /// `sucursal-norte`). `None` for single-instance channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Channel-native recipient address (whatsapp jid, telegram
    /// chat_id, …). Optional because some pipelines (e.g. system-
    /// injected ticks) don't have one yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    /// Tenant id when the agent is tenant-scoped, otherwise `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Conversation key used by operator UIs to group messages —
    /// matches the one the firehose store + chat sidebar use
    /// (`<agent_id>:session:<session_id>` for transcript-keyed
    /// chats). Microapps key per-conversation state on this.
    pub conversation_key: String,
    /// ISO-639-1 hint from `agents.yaml.<id>.language`. Lets reply
    /// transformers localise their behaviour (e.g. voice_mode
    /// picks a region-appropriate Edge voice when the operator
    /// hasn't manually overridden it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

mod base64_bytes {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&B64.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        B64.decode(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips() {
        let r = OutboundReplyKind::text("hola");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["kind"], "text");
        assert_eq!(v["body"], "hola");
        let back: OutboundReplyKind = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn voice_note_round_trips_via_base64() {
        let r = OutboundReplyKind::VoiceNote {
            audio_bytes: vec![0x49, 0x44, 0x33, 0x04],
            mimetype: "audio/mpeg".into(),
            transcript: Some("hola".into()),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"kind\":\"voice_note\""));
        // Base64 of bytes is "SUQzBA==" — small so the assertion is stable.
        assert!(s.contains("SUQzBA=="));
        let back: OutboundReplyKind = serde_json::from_str(&s).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn unknown_variant_fails_loud() {
        let s = r#"{"kind":"sticker","sticker_id":"x"}"#;
        let r: Result<OutboundReplyKind, _> = serde_json::from_str(s);
        assert!(r.is_err());
    }

    #[test]
    fn kind_label_matches_variant() {
        assert_eq!(OutboundReplyKind::text("a").kind_label(), "text");
    }
}
