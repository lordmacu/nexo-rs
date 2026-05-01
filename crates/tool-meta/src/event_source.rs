//! [`EventSourceMeta`] — typed metadata describing the NATS
//! subject + envelope that triggered an agent turn via the
//! Phase 82.4 event-subscriber pipeline.
//!
//! Stamped on [`crate::BindingContext::event_source`] when the
//! agent's inbound was synthesised from an event (vs arriving
//! via a native channel like WhatsApp / Telegram). Microapps
//! read it from `_meta.nexo.binding.event_source.subject` to
//! distinguish event-triggered turns from human-message turns.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Metadata describing the event that triggered an agent turn.
///
/// Wire-shape struct — caller-built by the event-subscriber
/// runtime, caller-read by microapps and downstream tools.
/// Field additions are deliberate semver-major (the JSON wire
/// shape changes regardless), so this type is intentionally
/// **not** `#[non_exhaustive]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSourceMeta {
    /// NATS subject that triggered the turn (e.g.
    /// `"webhook.github_main.pull_request"`).
    pub subject: String,
    /// Envelope id from the upstream event payload (when
    /// available — webhook events carry one;
    /// custom-published events may not).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub envelope_id: Option<Uuid>,
    /// Synthesis mode the binding was configured with —
    /// `"synthesize"` or `"tick"`. Tools can branch behaviour
    /// (e.g. fetch payload only when mode is `"tick"`).
    pub synthesis_mode: String,
}

/// Phase 72 turn-log marker. Returns `"event:<source_id>"` so a
/// downstream audit row can distinguish event-subscriber turns
/// from native-channel inbounds. Mirrors the `"channel:<server>"`
/// convention from Phase 80.9 and `"webhook:<source_id>"` from
/// Phase 82.2.
///
/// # Example
///
/// ```
/// use nexo_tool_meta::format_event_subscriber_source;
/// assert_eq!(
///     format_event_subscriber_source("github_main"),
///     "event:github_main"
/// );
/// ```
pub fn format_event_subscriber_source(source_id: &str) -> String {
    format!("event:{source_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_serde() {
        let original = EventSourceMeta {
            subject: "webhook.github.pull_request".into(),
            envelope_id: Some(Uuid::nil()),
            synthesis_mode: "synthesize".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: EventSourceMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn skips_envelope_id_when_none() {
        let meta = EventSourceMeta {
            subject: "x".into(),
            envelope_id: None,
            synthesis_mode: "tick".into(),
        };
        let v = serde_json::to_value(&meta).unwrap();
        assert!(v.get("subject").is_some());
        assert!(v.get("envelope_id").is_none());
        assert_eq!(v["synthesis_mode"], "tick");
    }

    #[test]
    fn wire_shape_lock_down() {
        let meta = EventSourceMeta {
            subject: "s".into(),
            envelope_id: Some(Uuid::from_u128(1)),
            synthesis_mode: "synthesize".into(),
        };
        let v = serde_json::to_value(&meta).unwrap();
        for key in ["subject", "envelope_id", "synthesis_mode"] {
            assert!(v.get(key).is_some(), "missing key `{key}` in event_source");
        }
    }

    #[test]
    fn format_marker_prefixes_with_event() {
        assert_eq!(
            format_event_subscriber_source("github_main"),
            "event:github_main"
        );
        assert_eq!(format_event_subscriber_source(""), "event:");
    }
}
