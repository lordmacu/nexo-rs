//! Payload shapes the plugin publishes on `plugin.inbound.whatsapp`.
//!
//! Core's agent runtime reads `text` / `from` out of the payload to build
//! its `InboundMessage`. Anything else is passthrough metadata that tools
//! / extensions can inspect but core ignores.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Discriminated union of everything the plugin may publish inbound.
///
/// Serialised with `#[serde(tag = "kind", rename_all = "snake_case")]`
/// so consumers can match on a single string field rather than carrying
/// an out-of-band schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboundEvent {
    /// Regular text / reply / media caption message routed to the agent.
    ///
    /// The top-level `text` / `from` fields are flattened so core's
    /// existing runtime (which only inspects those two keys) keeps
    /// working without changes.
    Message {
        from: String,
        chat: String,
        text: Option<String>,
        reply_to: Option<String>,
        is_group: bool,
        timestamp: i64,
        msg_id: String,
    },
    /// Paired with `Message` when an incoming message carried media.
    /// The file has already been downloaded to disk when this event is
    /// published.
    MediaReceived {
        from: String,
        chat: String,
        msg_id: String,
        local_path: PathBuf,
        mime: String,
        caption: Option<String>,
    },
    /// Fresh QR code for pairing. `ascii` is a terminal-friendly render;
    /// `png_base64` is the raw bitmap for UIs.
    Qr {
        ascii: String,
        png_base64: String,
        expires_at: i64,
    },
    Connected { our_jid: String },
    Disconnected { reason: String },
    Reconnecting { attempt: u32 },
    PairingSuccess { device_jid: String },
    CredentialsExpired,
    /// Emitted by the bridge when an inbound message didn't receive an
    /// outbound reply within `bridge.response_timeout_ms`. Observability
    /// only — the actual user-facing fallback is handled by
    /// `bridge.on_timeout`.
    BridgeTimeout { session_id: uuid::Uuid },
}

impl InboundEvent {
    /// JSON payload in the shape the core runtime expects for `Message`
    /// events. Flattens `text` and `from` to the top level.
    pub fn to_payload(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}
