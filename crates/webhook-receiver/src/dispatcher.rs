//! Phase 82.2 — webhook dispatch trait + broker-backed default.
//!
//! Pattern matches Phase 80.9 `ChannelDispatcher` so the same
//! mental model applies across inbound surfaces. The dispatcher
//! consumes a typed `WebhookEnvelope` produced by the server's
//! 5-gate pipeline; serialisation + transport choice is its job.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Headers we are willing to forward inside `WebhookEnvelope`.
/// Forwarding everything would leak Authorization / Cookie / signature
/// secrets to NATS subscribers; this allowlist captures the
/// non-secret correlation IDs that downstream consumers actually
/// need (delivery dedup, observability).
pub const FORWARD_HEADERS: &[&str] = &[
    "x-github-delivery",
    "x-stripe-event-id",
    "x-event-id",
    "x-request-id",
    "idempotency-key",
    "user-agent",
];

/// JSON envelope published to NATS for every accepted webhook.
/// `schema = 1` is fixed; future shape changes bump it explicitly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookEnvelope {
    pub schema: u8,
    pub source_id: String,
    pub event_kind: String,
    pub body_json: serde_json::Value,
    pub headers_subset: BTreeMap<String, String>,
    pub received_at_ms: i64,
    pub envelope_id: Uuid,
    /// Resolved client IP (after trusted-proxy logic). `None` for
    /// hand-built envelopes in tests.
    pub client_ip: Option<IpAddr>,
}

impl WebhookEnvelope {
    /// Build the envelope from the pure-fn `HandledEvent` produced
    /// by Phase 80.12, plus the wire-level metadata the HTTP
    /// handler observed.
    pub fn from_handled(
        handled: super::HandledEvent,
        raw_headers: &BTreeMap<String, String>,
        client_ip: Option<IpAddr>,
    ) -> Self {
        Self {
            schema: 1,
            source_id: handled.source_id,
            event_kind: handled.event_kind,
            body_json: handled.payload,
            headers_subset: filter_forward_headers(raw_headers),
            received_at_ms: Utc::now().timestamp_millis(),
            envelope_id: Uuid::new_v4(),
            client_ip,
        }
    }
}

/// Allowlist filter — case-insensitive header-name match.
pub fn filter_forward_headers(
    headers: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if FORWARD_HEADERS.iter().any(|allowed| *allowed == lower) {
            out.insert(lower, value.clone());
        }
    }
    out
}

/// Errors a dispatcher can return. The HTTP handler maps `Broker`
/// to 502 Bad Gateway and `Rejected` to 422 Unprocessable Entity.
#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("broker publish failed: {0}")]
    Broker(String),
    #[error("envelope rejected: {0}")]
    Rejected(String),
}

/// Trait the HTTP handler invokes after the 5-gate pipeline. Tests
/// inject `RecordingWebhookDispatcher`; production injects
/// `BrokerWebhookDispatcher`.
#[async_trait]
pub trait WebhookDispatcher: Send + Sync {
    /// Topic resolution + transport. Implementors get the topic
    /// pre-rendered by the per-source publish_to template.
    async fn dispatch(&self, topic: &str, envelope: WebhookEnvelope) -> Result<(), DispatchError>;
}

/// Test helper — captures every dispatched envelope in-memory.
/// Public so the integration tests in Step 5/6 can wire it as a
/// `Arc<dyn WebhookDispatcher>` without depending on the broker
/// reactor.
pub struct RecordingWebhookDispatcher {
    inner: tokio::sync::Mutex<Vec<(String, WebhookEnvelope)>>,
}

impl Default for RecordingWebhookDispatcher {
    fn default() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(Vec::new()),
        }
    }
}

impl RecordingWebhookDispatcher {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn captured(&self) -> Vec<(String, WebhookEnvelope)> {
        self.inner.lock().await.clone()
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}

#[async_trait]
impl WebhookDispatcher for RecordingWebhookDispatcher {
    async fn dispatch(&self, topic: &str, envelope: WebhookEnvelope) -> Result<(), DispatchError> {
        self.inner.lock().await.push((topic.to_string(), envelope));
        Ok(())
    }
}

/// Phase 72 turn-log marker. Re-exported via `lib.rs` so callers
/// (poller / agent_turn / dispatch) stamp `TurnRecord.source`
/// consistently with Phase 80.9.h `"channel:<server>"` convention.
pub fn format_webhook_source(source_id: &str) -> String {
    format!("webhook:{source_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HandledEvent;

    fn raw_headers() -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("Authorization".into(), "Bearer secret123".into());
        m.insert("X-GitHub-Delivery".into(), "abc-123".into());
        m.insert("Idempotency-Key".into(), "k-42".into());
        m.insert("Cookie".into(), "session=evil".into());
        m
    }

    #[test]
    fn filter_keeps_allowlisted_drops_secrets() {
        let out = filter_forward_headers(&raw_headers());
        assert!(out.contains_key("x-github-delivery"));
        assert!(out.contains_key("idempotency-key"));
        assert!(!out.contains_key("authorization"));
        assert!(!out.contains_key("cookie"));
    }

    #[test]
    fn from_handled_pins_schema_and_generates_envelope_id() {
        let handled = HandledEvent {
            source_id: "github".into(),
            event_kind: "pull_request".into(),
            topic: "webhook.github.pull_request".into(),
            payload: serde_json::json!({"action": "opened"}),
        };
        let env = WebhookEnvelope::from_handled(handled, &raw_headers(), None);
        assert_eq!(env.schema, 1);
        assert_eq!(env.source_id, "github");
        assert_eq!(env.event_kind, "pull_request");
        assert_eq!(env.body_json["action"], "opened");
        assert!(env.headers_subset.contains_key("x-github-delivery"));
        assert!(env.received_at_ms > 0);
        // Two envelopes get distinct ids.
        let env2 = WebhookEnvelope::from_handled(
            HandledEvent {
                source_id: "github".into(),
                event_kind: "pull_request".into(),
                topic: "x".into(),
                payload: serde_json::Value::Null,
            },
            &raw_headers(),
            None,
        );
        assert_ne!(env.envelope_id, env2.envelope_id);
    }

    #[tokio::test]
    async fn recording_dispatcher_captures_calls() {
        let rec = RecordingWebhookDispatcher::new();
        let env = WebhookEnvelope {
            schema: 1,
            source_id: "x".into(),
            event_kind: "y".into(),
            body_json: serde_json::Value::Null,
            headers_subset: BTreeMap::new(),
            received_at_ms: 0,
            envelope_id: Uuid::nil(),
            client_ip: None,
        };
        rec.dispatch("topic.y", env.clone()).await.unwrap();
        rec.dispatch("topic.z", env).await.unwrap();
        let cap = rec.captured().await;
        assert_eq!(cap.len(), 2);
        assert_eq!(cap[0].0, "topic.y");
        assert_eq!(cap[1].0, "topic.z");
    }

    #[test]
    fn format_webhook_source_prefixes() {
        assert_eq!(format_webhook_source("github_main"), "webhook:github_main");
        // Pass-through; sanitisation is the operator's responsibility
        // (subject-safety is enforced upstream in Phase 80.12).
        assert_eq!(format_webhook_source(""), "webhook:");
    }

    #[test]
    fn webhook_envelope_serialises_with_stable_field_names() {
        let env = WebhookEnvelope {
            schema: 1,
            source_id: "s".into(),
            event_kind: "e".into(),
            body_json: serde_json::json!({}),
            headers_subset: BTreeMap::new(),
            received_at_ms: 0,
            envelope_id: Uuid::nil(),
            client_ip: None,
        };
        let v = serde_json::to_value(&env).unwrap();
        // Lock the wire shape — anything renaming these breaks
        // every microapp that reads them.
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
}
