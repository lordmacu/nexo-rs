//! [`InboundMessageMeta`] — per-turn metadata about the inbound
//! message that triggered an agent turn. Stamped under
//! `_meta.nexo.inbound` (peer of `_meta.nexo.binding`).
//!
//! Distinct from [`crate::BindingContext`]: the binding identifies
//! *which tenant* the call belongs to (stable across turns within
//! the same binding), the inbound meta describes *which message*
//! triggered this turn (varies per turn).
//!
//! Provider-agnostic by construction. The same shape is produced
//! by every inbound path:
//! - native channel plugins (whatsapp, future telegram/email/…)
//! - event-subscriber bindings (NATS subject → agent turn)
//! - webhook receiver
//! - MCP-channel server inbounds
//! - inter-agent delegation receives
//! - heartbeat / scheduler ticks

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 3-way discriminator for the origin of an agent turn.
///
/// Mirrors the trichotomy used by inbound provenance tracking
/// (external_user / inter_session / internal_system) so microapps
/// can branch handlers on the message origin without re-deriving
/// it from sender_id presence alone.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboundKind {
    /// External end-user message via a channel plugin (whatsapp,
    /// telegram, email, …), webhook receiver, or MCP-channel
    /// server. `sender_id` is typically populated.
    ExternalUser,
    /// System-injected turn (heartbeat tick, scheduler fire,
    /// event-subscriber binding declared as `internal_system`).
    /// `sender_id` is `None`.
    InternalSystem,
    /// Inter-agent delegation receive. `origin_session_id`
    /// carries the calling agent's session.
    InterSession,
}

/// Per-turn metadata about the inbound message that triggered the
/// agent turn.
///
/// Stamped on every tool call dispatched to a Phase 11 stdio
/// extension or a Phase 12 MCP server (under
/// `params._meta.nexo.inbound`). All fields except `kind` are
/// optional; consumers must tolerate absence and branch
/// gracefully.
///
/// # Example — external user message
///
/// ```
/// use nexo_tool_meta::{InboundKind, InboundMessageMeta};
///
/// let meta = InboundMessageMeta::external_user("+5491100", "wa.ABCD1234");
/// assert_eq!(meta.kind, InboundKind::ExternalUser);
/// assert_eq!(meta.sender_id.as_deref(), Some("+5491100"));
/// assert_eq!(meta.msg_id.as_deref(), Some("wa.ABCD1234"));
/// ```
///
/// # Example — internal system tick
///
/// ```
/// use nexo_tool_meta::{InboundKind, InboundMessageMeta};
///
/// let meta = InboundMessageMeta::internal_system();
/// assert_eq!(meta.kind, InboundKind::InternalSystem);
/// assert!(meta.sender_id.is_none());
/// assert!(meta.msg_id.is_none());
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundMessageMeta {
    /// Origin discriminator. Always present when the bucket is.
    pub kind: InboundKind,

    /// Provider-native sender id (E.164 phone, telegram_user_id,
    /// RFC822 from-address, slack U-id, …). `None` for kinds
    /// other than `ExternalUser` and for paths where the provider
    /// does not expose a sender id (e.g. webhook with no auth).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sender_id: Option<String>,

    /// Provider-native message id (single canonical, no shortened
    /// alias). Used for idempotency, dedupe, and as the
    /// `reply_to_msg_id` target on subsequent turns. When a
    /// debounce window collapses several inbound messages into a
    /// single turn, this carries the id of the **first** message
    /// in the batch.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub msg_id: Option<String>,

    /// Inbound timestamp in UTC. Wire shape: RFC3339 string via
    /// chrono's serde adapter. Producers must clamp wildly-skewed
    /// provider timestamps to `now()` before emitting.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub inbound_ts: Option<DateTime<Utc>>,

    /// Provider-native id of the message this one replies to.
    /// `None` when the inbound is not a reply.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reply_to_msg_id: Option<String>,

    /// `true` when the inbound carries non-text media
    /// (image/audio/video/file/sticker). Microapps gate
    /// media-aware tools on this; the actual bytes/URLs live in
    /// `arguments`, not in `_meta`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_media: bool,

    /// Origin session id when `kind == InterSession` (delegation
    /// receive). `None` otherwise.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub origin_session_id: Option<Uuid>,
}

fn is_false(b: &bool) -> bool {
    !b
}

impl InboundMessageMeta {
    /// Build the minimum meta for an external user message
    /// (channel plugin / webhook / MCP-channel inbound).
    pub fn external_user(sender_id: impl Into<String>, msg_id: impl Into<String>) -> Self {
        Self {
            kind: InboundKind::ExternalUser,
            sender_id: Some(sender_id.into()),
            msg_id: Some(msg_id.into()),
            inbound_ts: None,
            reply_to_msg_id: None,
            has_media: false,
            origin_session_id: None,
        }
    }

    /// Build the meta for a heartbeat / scheduler tick or a
    /// yaml-declared `internal_system` event-subscriber binding.
    pub fn internal_system() -> Self {
        Self {
            kind: InboundKind::InternalSystem,
            sender_id: None,
            msg_id: None,
            inbound_ts: None,
            reply_to_msg_id: None,
            has_media: false,
            origin_session_id: None,
        }
    }

    /// Build the meta for an inter-agent delegation receive.
    /// `origin_session_id` is required — `None` would be
    /// indistinguishable from `internal_system`.
    pub fn inter_session(origin_session_id: Uuid) -> Self {
        Self {
            kind: InboundKind::InterSession,
            sender_id: None,
            msg_id: None,
            inbound_ts: None,
            reply_to_msg_id: None,
            has_media: false,
            origin_session_id: Some(origin_session_id),
        }
    }

    /// Builder: layer the inbound timestamp.
    pub fn with_ts(mut self, ts: DateTime<Utc>) -> Self {
        self.inbound_ts = Some(ts);
        self
    }

    /// Builder: layer the reply-to message id.
    pub fn with_reply_to(mut self, reply_to_msg_id: impl Into<String>) -> Self {
        self.reply_to_msg_id = Some(reply_to_msg_id.into());
        self
    }

    /// Builder: flag that the inbound carries non-text media.
    pub fn with_media(mut self) -> Self {
        self.has_media = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn external_user_builder_sets_kind_and_sender_msg() {
        let m = InboundMessageMeta::external_user("+5491100", "wa.ABCD");
        assert_eq!(m.kind, InboundKind::ExternalUser);
        assert_eq!(m.sender_id.as_deref(), Some("+5491100"));
        assert_eq!(m.msg_id.as_deref(), Some("wa.ABCD"));
        assert!(m.inbound_ts.is_none());
        assert!(m.reply_to_msg_id.is_none());
        assert!(!m.has_media);
        assert!(m.origin_session_id.is_none());
    }

    #[test]
    fn internal_system_builder_clears_sender_msg_origin() {
        let m = InboundMessageMeta::internal_system();
        assert_eq!(m.kind, InboundKind::InternalSystem);
        assert!(m.sender_id.is_none());
        assert!(m.msg_id.is_none());
        assert!(m.origin_session_id.is_none());
    }

    #[test]
    fn inter_session_builder_carries_origin_session_id() {
        let id = Uuid::from_u128(0x42);
        let m = InboundMessageMeta::inter_session(id);
        assert_eq!(m.kind, InboundKind::InterSession);
        assert!(m.sender_id.is_none());
        assert_eq!(m.origin_session_id, Some(id));
    }

    #[test]
    fn with_ts_with_reply_to_with_media_layer_correctly() {
        let ts = Utc.with_ymd_and_hms(2026, 5, 1, 12, 34, 56).unwrap();
        let m = InboundMessageMeta::external_user("+5491100", "wa.ABCD")
            .with_ts(ts)
            .with_reply_to("wa.PREV0001")
            .with_media();
        assert_eq!(m.inbound_ts, Some(ts));
        assert_eq!(m.reply_to_msg_id.as_deref(), Some("wa.PREV0001"));
        assert!(m.has_media);
    }

    #[test]
    fn serialise_skips_none_and_false_fields() {
        let m = InboundMessageMeta::internal_system();
        let v = serde_json::to_value(&m).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("kind"));
        assert!(!obj.contains_key("sender_id"));
        assert!(!obj.contains_key("msg_id"));
        assert!(!obj.contains_key("inbound_ts"));
        assert!(!obj.contains_key("reply_to_msg_id"));
        // has_media: false is omitted.
        assert!(!obj.contains_key("has_media"));
        assert!(!obj.contains_key("origin_session_id"));
    }

    #[test]
    fn round_trip_through_serde_full_payload() {
        let ts = Utc.with_ymd_and_hms(2026, 5, 1, 12, 34, 56).unwrap();
        let original = InboundMessageMeta::external_user("user@host", "msg-1")
            .with_ts(ts)
            .with_reply_to("msg-0")
            .with_media();
        let s = serde_json::to_string(&original).unwrap();
        let back: InboundMessageMeta = serde_json::from_str(&s).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn parse_rejects_unknown_kind_string() {
        // `kind` is a closed enum on the wire (no #[serde(other)]).
        // A future producer emitting a new kind must wait for the
        // consumer to bump the type. Strict reject is the safer
        // default.
        let raw = serde_json::json!({ "kind": "future_kind" });
        let r: Result<InboundMessageMeta, _> = serde_json::from_value(raw);
        assert!(r.is_err(), "unknown kind must reject, not silently accept");
    }

    #[test]
    fn clone_eq_holds_for_full_payload() {
        let a = InboundMessageMeta::external_user("+5491100", "wa.ABCD")
            .with_ts(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
            .with_reply_to("wa.PREV")
            .with_media();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn provider_agnostic_sender_id_accepts_arbitrary_string() {
        // Validates abstraction: shape is not whatsapp-only.
        // Same struct happily carries telegram, email, slack ids.
        let wa = InboundMessageMeta::external_user("+5491100", "wa.1");
        let tg = InboundMessageMeta::external_user("tg.user_42", "tg.msg.7");
        let em = InboundMessageMeta::external_user("alice@example.com", "<id@host>");
        assert_eq!(wa.sender_id.as_deref(), Some("+5491100"));
        assert_eq!(tg.sender_id.as_deref(), Some("tg.user_42"));
        assert_eq!(em.sender_id.as_deref(), Some("alice@example.com"));
    }
}
