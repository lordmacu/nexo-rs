//! [`WebhookEnvelope`] — the JSON payload nexo publishes to NATS
//! after a webhook source verifies and parses an inbound HTTP
//! request.
//!
//! Microapps subscribe to the broker subject (typically
//! `webhook.<source_id>.<event_kind>`) and deserialise this
//! envelope to react to provider events.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Current `WebhookEnvelope.schema` version. The daemon stamps
/// every envelope with this constant; consumers read it to gate
/// behaviour against a known shape.
pub const ENVELOPE_SCHEMA_VERSION: u8 = 1;

/// Typed JSON envelope nexo publishes after every accepted
/// webhook request.
///
/// Subscribers correlate events via `envelope_id` (deterministic
/// dedup) and `received_at_ms` (late-binding analytics).
/// `headers_subset` is a defensive allowlist — secrets like
/// `Authorization` / `Cookie` / signature headers are stripped
/// before publish, so a NATS subscriber sees only non-secret
/// correlation IDs.
///
/// Unlike [`crate::BindingContext`], this struct is intentionally
/// *not* `#[non_exhaustive]`: it represents a wire-shape value
/// constructed on both sides (the daemon writes it; tests +
/// mocks build it via struct-literal). Field additions are
/// semver-major because the JSON wire shape changes regardless.
///
/// # Example
///
/// Microapps typically deserialise the envelope from a NATS
/// payload:
///
/// ```
/// use nexo_tool_meta::WebhookEnvelope;
///
/// let payload = serde_json::json!({
///     "schema": 1,
///     "source_id": "github_main",
///     "event_kind": "pull_request",
///     "body_json": {"action": "opened"},
///     "headers_subset": {},
///     "received_at_ms": 0,
///     "envelope_id": "00000000-0000-0000-0000-000000000000",
///     "client_ip": null
/// });
/// let env: WebhookEnvelope = serde_json::from_value(payload).unwrap();
/// assert_eq!(env.schema, 1);
/// assert_eq!(env.source_id, "github_main");
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WebhookEnvelope {
    /// Wire-shape version. Always [`ENVELOPE_SCHEMA_VERSION`].
    pub schema: u8,
    /// Operator-assigned source identifier — matches the
    /// `webhook_receiver.sources[].id` YAML field.
    pub source_id: String,
    /// Event kind extracted from the inbound request (header or
    /// JSON body path, per source config).
    pub event_kind: String,
    /// Inbound body, parsed as JSON. Non-JSON bodies are wrapped
    /// as `{ "raw_base64": "..." }` upstream.
    pub body_json: serde_json::Value,
    /// Allowlisted headers forwarded for downstream correlation.
    /// Authorization / Cookie / signature headers are stripped.
    pub headers_subset: BTreeMap<String, String>,
    /// Server-side receipt timestamp in milliseconds since epoch.
    pub received_at_ms: i64,
    /// Random per-envelope identifier — useful for dedup.
    pub envelope_id: Uuid,
    /// Resolved client IP (after trusted-proxy logic). `None`
    /// for envelopes built outside an HTTP request context.
    pub client_ip: Option<IpAddr>,
}

/// Phase 72 turn-log marker.
///
/// Returns `"webhook:<source_id>"` so a downstream audit row can
/// distinguish webhook-originated turns from native-channel
/// inbounds. Mirrors the `"channel:<server>"` convention shipped
/// for Phase 80.9 MCP channels.
///
/// # Example
///
/// ```
/// use nexo_tool_meta::format_webhook_source;
/// assert_eq!(format_webhook_source("github_main"), "webhook:github_main");
/// ```
pub fn format_webhook_source(source_id: &str) -> String {
    format!("webhook:{source_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> WebhookEnvelope {
        WebhookEnvelope {
            schema: ENVELOPE_SCHEMA_VERSION,
            source_id: "github_main".into(),
            event_kind: "pull_request".into(),
            body_json: serde_json::json!({"action": "opened"}),
            headers_subset: BTreeMap::new(),
            received_at_ms: 1_700_000_000,
            envelope_id: Uuid::nil(),
            client_ip: None,
        }
    }

    #[test]
    fn schema_constant_locked_at_1() {
        assert_eq!(ENVELOPE_SCHEMA_VERSION, 1);
        assert_eq!(sample().schema, 1);
    }

    #[test]
    fn round_trip_through_serde() {
        let original = sample();
        let json = serde_json::to_string(&original).unwrap();
        let back: WebhookEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn wire_shape_lock_down() {
        let env = sample();
        let v = serde_json::to_value(&env).unwrap();
        for key in [
            "schema",
            "source_id",
            "event_kind",
            "body_json",
            "headers_subset",
            "received_at_ms",
            "envelope_id",
            "client_ip",
        ] {
            assert!(v.get(key).is_some(), "missing key `{key}` in envelope");
        }
    }

    #[test]
    fn format_webhook_source_prefixes() {
        assert_eq!(format_webhook_source("github_main"), "webhook:github_main");
        assert_eq!(format_webhook_source(""), "webhook:");
    }
}
