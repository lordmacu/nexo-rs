//! Phase 82.2 — webhook dispatch trait + envelope builder.
//!
//! The data-shape (`WebhookEnvelope`, schema constant) lives in
//! `nexo-tool-meta` so a third-party microapp can `cargo add
//! nexo-tool-meta` and consume the envelope without depending on
//! this crate. The trait + the `RecordingWebhookDispatcher` test
//! helper stay here — they are runtime-time concerns coupled to
//! the dispatcher contract, not to the wire shape.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;

pub use nexo_tool_meta::{
    format_webhook_source, WebhookEnvelope, ENVELOPE_SCHEMA_VERSION,
};

/// Header allowlist forwarded inside a [`WebhookEnvelope`].
///
/// Forwarding everything would leak `Authorization` / `Cookie` /
/// signature secrets to NATS subscribers; this allowlist captures
/// the non-secret correlation IDs downstream consumers actually
/// need (delivery dedup, observability).
pub const FORWARD_HEADERS: &[&str] = &[
    "x-github-delivery",
    "x-stripe-event-id",
    "x-event-id",
    "x-request-id",
    "idempotency-key",
    "user-agent",
];

/// Build a [`WebhookEnvelope`] from a Phase 80.12 [`HandledEvent`]
/// plus the wire-level metadata the HTTP handler observed.
///
/// Lives here (not on `WebhookEnvelope` itself) because
/// [`WebhookEnvelope`] is `#[non_exhaustive]` in `nexo-tool-meta`
/// and the constructor needs `HandledEvent` which is a
/// receiver-crate type. Callers in `nexo-webhook-server` invoke
/// this fn after the 5-gate pipeline succeeds.
pub fn envelope_from_handled(
    handled: super::HandledEvent,
    raw_headers: &BTreeMap<String, String>,
    client_ip: Option<IpAddr>,
) -> WebhookEnvelope {
    WebhookEnvelope {
        schema: ENVELOPE_SCHEMA_VERSION,
        source_id: handled.source_id,
        event_kind: handled.event_kind,
        body_json: handled.payload,
        headers_subset: filter_forward_headers(raw_headers),
        received_at_ms: Utc::now().timestamp_millis(),
        envelope_id: Uuid::new_v4(),
        client_ip,
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
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum DispatchError {
    /// The broker (or whichever transport) refused the publish.
    #[error("broker publish failed: {0}")]
    Broker(String),
    /// The envelope failed pre-publish validation (typically
    /// serialisation).
    #[error("envelope rejected: {0}")]
    Rejected(String),
}

/// Trait the HTTP handler invokes after the 5-gate pipeline. Tests
/// inject [`RecordingWebhookDispatcher`]; production injects the
/// broker-backed dispatcher from `nexo-webhook-server`.
#[async_trait]
pub trait WebhookDispatcher: Send + Sync {
    /// Topic resolution + transport. Implementors get the topic
    /// pre-rendered by the per-source publish_to template.
    async fn dispatch(
        &self,
        topic: &str,
        envelope: WebhookEnvelope,
    ) -> Result<(), DispatchError>;
}

/// Test helper — captures every dispatched envelope in-memory.
/// Lets integration tests in downstream crates wire it as
/// `Arc<dyn WebhookDispatcher>` without depending on a real
/// broker reactor.
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
    /// Construct a fresh recorder wrapped in `Arc` for sharing.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Snapshot of every captured `(topic, envelope)` pair so far.
    pub async fn captured(&self) -> Vec<(String, WebhookEnvelope)> {
        self.inner.lock().await.clone()
    }

    /// Number of dispatch calls observed so far.
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}

#[async_trait]
impl WebhookDispatcher for RecordingWebhookDispatcher {
    async fn dispatch(
        &self,
        topic: &str,
        envelope: WebhookEnvelope,
    ) -> Result<(), DispatchError> {
        self.inner.lock().await.push((topic.to_string(), envelope));
        Ok(())
    }
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
    fn envelope_from_handled_pins_schema_and_generates_envelope_id() {
        let handled = HandledEvent {
            source_id: "github".into(),
            event_kind: "pull_request".into(),
            topic: "webhook.github.pull_request".into(),
            payload: serde_json::json!({"action": "opened"}),
        };
        let env = envelope_from_handled(handled, &raw_headers(), None);
        assert_eq!(env.schema, ENVELOPE_SCHEMA_VERSION);
        assert_eq!(env.source_id, "github");
        assert_eq!(env.event_kind, "pull_request");
        assert_eq!(env.body_json["action"], "opened");
        assert!(env.headers_subset.contains_key("x-github-delivery"));
        assert!(env.received_at_ms > 0);

        let env2 = envelope_from_handled(
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
        let handled = HandledEvent {
            source_id: "x".into(),
            event_kind: "y".into(),
            topic: "topic.y".into(),
            payload: serde_json::Value::Null,
        };
        let env = envelope_from_handled(handled, &BTreeMap::new(), None);
        rec.dispatch("topic.y", env.clone()).await.unwrap();
        rec.dispatch("topic.z", env).await.unwrap();
        let cap = rec.captured().await;
        assert_eq!(cap.len(), 2);
        assert_eq!(cap[0].0, "topic.y");
        assert_eq!(cap[1].0, "topic.z");
    }
}
