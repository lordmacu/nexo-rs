//! Phase 82.4.b — runtime task that subscribes to a NATS subject
//! pattern, renders the operator's mustache-lite template (or
//! builds a `<event/>` tick marker), and re-publishes a
//! synthesised [`InboundEvent::Message`] so the existing inbound
//! pipeline drives the agent turn.
//!
//! Spawned by the boot supervisor — one task per `(agent, binding)`
//! pair. Lifecycle:
//!
//! ```text
//! spawn → subscribe → mpsc<buffer> → consumer worker → cancel.cancelled() → drain → exit
//! ```
//!
//! The wire shape, validation rules, and `BindingContext.event_source`
//! field were shipped in the Phase 82.4 foundations slice.

use std::sync::Arc;

use nexo_broker::{AnyBroker, Event};
use nexo_config::types::event_subscriber::{EventSubscriberBinding, SynthesisMode};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::Semaphore;
use uuid::Uuid;

/// Topic prefix the runtime re-publishes synthesised inbounds to.
/// Format: `plugin.inbound.event.<source_id>`.
pub const EVENT_INBOUND_TOPIC_PREFIX: &str = "plugin.inbound.event";

/// Channel name used for the auto-synthesised `InboundBinding`
/// (`{ plugin: "event", instance: "<source_id>" }`). Microapps
/// that read `BindingContext.channel` see this string when an
/// inbound originated from an event subscriber.
pub const EVENT_BINDING_CHANNEL: &str = "event";

/// Field name on the synthesised `InboundEvent::Message` payload
/// that carries the `EventSourceMeta` JSON. The inbound resolver
/// reads it via [`extract_nexo_event_source`] when matching a
/// `plugin.inbound.event.*` topic.
pub const EVENT_SOURCE_PAYLOAD_FIELD: &str = "_nexo_event_source";

/// Build the canonical re-publish topic for a given source id.
pub fn event_inbound_topic(source_id: &str) -> String {
    format!("{EVENT_INBOUND_TOPIC_PREFIX}.{source_id}")
}

/// Build the synthesised inbound payload for one event.
///
/// `Synthesize` mode renders the operator's mustache-lite
/// template (or falls back to `serde_json::to_string(payload)`)
/// into the message body. `Tick` mode produces a fixed
/// `<event subject="..." envelope_id="..."/>` marker. `Off` mode
/// returns `None` (caller short-circuits).
///
/// The payload follows the `InboundEvent::Message` shape so the
/// existing `plugin.inbound.>` listener handles it without
/// modification, plus a top-level `_nexo_event_source` field
/// that the inbound resolver reads to populate
/// `BindingContext.event_source`.
pub fn build_synthesised_payload(
    binding: &EventSubscriberBinding,
    event: &Event,
) -> Option<Value> {
    if binding.synthesize_inbound == SynthesisMode::Off {
        return None;
    }

    let envelope_id = extract_envelope_id(&event.payload);
    let from = format!("event:{}", binding.id);
    let timestamp = chrono::Utc::now().timestamp_millis();
    let msg_id = envelope_id
        .map(|u| u.to_string())
        .unwrap_or_else(|| event.id.to_string());

    let text = match binding.synthesize_inbound {
        SynthesisMode::Tick => format!(
            "<event subject=\"{}\" envelope_id=\"{}\"/>",
            event.topic,
            envelope_id
                .map(|u| u.to_string())
                .unwrap_or_else(|| "null".to_string())
        ),
        SynthesisMode::Synthesize => match binding.inbound_template.as_deref() {
            Some(tmpl) => nexo_tool_meta::render_template(tmpl, &event.payload),
            None => serde_json::to_string(&event.payload)
                .unwrap_or_else(|_| "<unrenderable payload>".to_string()),
        },
        SynthesisMode::Off => unreachable!("checked above"),
        // `SynthesisMode` is `#[non_exhaustive]`; future variants
        // fall back to a marker that signals the runtime saw an
        // unknown mode (operator must update or downgrade).
        _ => "<unknown synthesis mode>".to_string(),
    };

    let event_source = json!({
        "subject": event.topic,
        "envelope_id": envelope_id,
        "synthesis_mode": binding.synthesize_inbound.as_str(),
    });

    Some(json!({
        "kind": "message",
        "from": from,
        "chat": from,
        "text": text,
        "reply_to": Value::Null,
        "is_group": false,
        "timestamp": timestamp,
        "msg_id": msg_id,
        EVENT_SOURCE_PAYLOAD_FIELD: event_source,
    }))
}

/// Read `envelope_id` from the upstream event payload when the
/// publisher embedded one (e.g. `WebhookEnvelope`). `None` for
/// payloads without the field.
fn extract_envelope_id(payload: &Value) -> Option<Uuid> {
    payload
        .as_object()?
        .get("envelope_id")?
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// Read `_nexo_event_source` from a re-published inbound payload
/// and parse it into a typed [`nexo_tool_meta::EventSourceMeta`].
///
/// Called by the inbound resolver when matching topics on
/// `plugin.inbound.event.*`. Returns `None` for payloads without
/// the field (legacy / native-channel inbounds) or when the field
/// fails to deserialise (shape mismatch — log + skip).
pub fn extract_nexo_event_source(
    payload: &Value,
) -> Option<nexo_tool_meta::EventSourceMeta> {
    let raw = payload.as_object()?.get(EVENT_SOURCE_PAYLOAD_FIELD)?;
    serde_json::from_value(raw.clone()).ok()
}

/// One subscription task. Cheap to clone (everything inside is
/// `Arc`-shared); the runtime holds a single `Arc<EventSubscriber>`
/// and passes it into [`run_event_subscriber`].
pub struct EventSubscriber {
    /// Agent that owns this binding (used for tracing + the
    /// auto-synthesised `InboundBinding`).
    pub agent_id: String,
    /// Operator-declared binding (cloned at boot; the loop reads
    /// every field on each event).
    pub binding: Arc<EventSubscriberBinding>,
    /// Broker handle used both for `subscribe` (producer side)
    /// and `publish` (consumer re-publish).
    pub broker: AnyBroker,
    /// Per-binding concurrency cap. `None` when
    /// `binding.max_concurrency == 0` (unbounded — not exposed
    /// as a YAML option in v0; reserved for future use).
    pub semaphore: Option<Arc<Semaphore>>,
}

impl EventSubscriber {
    /// Build the subscription handle. Validation is the caller's
    /// responsibility — boot supervisor calls
    /// `binding.validate()` first and skips invalid bindings.
    pub fn new(
        agent_id: impl Into<String>,
        binding: EventSubscriberBinding,
        broker: AnyBroker,
    ) -> Self {
        let semaphore = if binding.max_concurrency == 0 {
            None
        } else {
            Some(Arc::new(Semaphore::new(binding.max_concurrency as usize)))
        };
        Self {
            agent_id: agent_id.into(),
            binding: Arc::new(binding),
            broker,
            semaphore,
        }
    }
}

/// Operator-facing diagnostic for a runtime task that exited
/// abnormally. The boot supervisor logs these and continues —
/// daemon stays up.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum EventSubscriberError {
    /// Broker subscribe failed (NATS unreachable, local broker
    /// closed, etc.). Logged at task spawn — the binding stays
    /// inactive until the daemon restarts.
    #[error("broker subscribe failed on `{subject}`: {detail}")]
    BrokerSubscribe {
        /// Subject pattern that was attempted.
        subject: String,
        /// Underlying broker error stringified.
        detail: String,
    },
    /// Broker publish failed when re-publishing the synthesised
    /// inbound. Logged + continues; the runtime drops the event
    /// rather than back-pressure the broker.
    #[error("broker publish failed on `{topic}`: {detail}")]
    BrokerPublish {
        /// Re-publish topic that failed.
        topic: String,
        /// Underlying broker error stringified.
        detail: String,
    },
}

#[cfg(test)]
mod skeleton_tests {
    use super::*;
    use nexo_broker::AnyBroker;
    use nexo_config::types::event_subscriber::{OverflowPolicy, SynthesisMode};

    fn mk_binding(id: &str, pattern: &str) -> EventSubscriberBinding {
        EventSubscriberBinding {
            id: id.into(),
            subject_pattern: pattern.into(),
            synthesize_inbound: SynthesisMode::Synthesize,
            inbound_template: None,
            max_concurrency: 1,
            max_buffer: 64,
            overflow_policy: OverflowPolicy::DropOldest,
        }
    }

    #[test]
    fn topic_prefix_lock_down() {
        assert_eq!(EVENT_INBOUND_TOPIC_PREFIX, "plugin.inbound.event");
        assert_eq!(EVENT_BINDING_CHANNEL, "event");
        assert_eq!(EVENT_SOURCE_PAYLOAD_FIELD, "_nexo_event_source");
    }

    #[test]
    fn event_inbound_topic_renders_dotted() {
        assert_eq!(
            event_inbound_topic("github_main"),
            "plugin.inbound.event.github_main"
        );
    }

    #[tokio::test]
    async fn new_with_max_concurrency_one_creates_semaphore() {
        let broker = AnyBroker::local();
        let sub = EventSubscriber::new("ana", mk_binding("a", "x.>"), broker);
        assert!(sub.semaphore.is_some());
        assert_eq!(
            sub.semaphore
                .as_ref()
                .unwrap()
                .available_permits(),
            1
        );
    }

    #[tokio::test]
    async fn new_with_zero_concurrency_skips_semaphore() {
        let broker = AnyBroker::local();
        let mut binding = mk_binding("a", "x.>");
        binding.max_concurrency = 0;
        let sub = EventSubscriber::new("ana", binding, broker);
        assert!(sub.semaphore.is_none());
    }

    fn mk_event(topic: &str, payload: serde_json::Value) -> Event {
        Event::new(topic, "tester", payload)
    }

    #[test]
    fn build_synthesise_renders_template() {
        let mut binding = mk_binding("github", "webhook.>");
        binding.inbound_template = Some("got {{event_kind}}".into());
        let event = mk_event(
            "webhook.github.opened",
            serde_json::json!({"event_kind": "opened"}),
        );
        let payload = build_synthesised_payload(&binding, &event).unwrap();
        assert_eq!(payload["kind"], "message");
        assert_eq!(payload["from"], "event:github");
        assert_eq!(payload["chat"], "event:github");
        assert_eq!(payload["text"], "got opened");
        assert_eq!(payload["is_group"], false);
        let es = &payload[EVENT_SOURCE_PAYLOAD_FIELD];
        assert_eq!(es["subject"], "webhook.github.opened");
        assert_eq!(es["synthesis_mode"], "synthesize");
    }

    #[test]
    fn build_tick_renders_event_marker() {
        let mut binding = mk_binding("alerts", "alert.>");
        binding.synthesize_inbound = SynthesisMode::Tick;
        let event = mk_event("alert.cpu.high", serde_json::json!({}));
        let payload = build_synthesised_payload(&binding, &event).unwrap();
        let text = payload["text"].as_str().unwrap();
        assert!(text.starts_with("<event subject=\"alert.cpu.high\""));
        assert!(text.ends_with("/>"));
        assert_eq!(payload[EVENT_SOURCE_PAYLOAD_FIELD]["synthesis_mode"], "tick");
    }

    #[test]
    fn build_off_returns_none() {
        let mut binding = mk_binding("x", "y.>");
        binding.synthesize_inbound = SynthesisMode::Off;
        let event = mk_event("y.z", serde_json::json!({}));
        assert!(build_synthesised_payload(&binding, &event).is_none());
    }

    #[test]
    fn build_synthesise_falls_back_to_json_when_template_missing() {
        let binding = mk_binding("x", "y.>");
        // No inbound_template → fallback to JSON-stringify.
        let event = mk_event("y.z", serde_json::json!({"a": 1, "b": "two"}));
        let payload = build_synthesised_payload(&binding, &event).unwrap();
        let text = payload["text"].as_str().unwrap();
        assert!(text.contains("\"a\":1"));
        assert!(text.contains("\"b\":\"two\""));
    }

    #[test]
    fn build_extracts_envelope_id_when_present() {
        let binding = mk_binding("github", "webhook.>");
        let envelope_id = Uuid::from_u128(42);
        let event = mk_event(
            "webhook.github.x",
            serde_json::json!({"envelope_id": envelope_id.to_string()}),
        );
        let payload = build_synthesised_payload(&binding, &event).unwrap();
        assert_eq!(payload["msg_id"], envelope_id.to_string());
        assert_eq!(
            payload[EVENT_SOURCE_PAYLOAD_FIELD]["envelope_id"],
            envelope_id.to_string()
        );
    }

    #[test]
    fn extract_nexo_event_source_happy_path() {
        let binding = mk_binding("github", "webhook.>");
        let mut binding_t = binding.clone();
        binding_t.inbound_template = Some("x".into());
        let event = mk_event("webhook.github.x", serde_json::json!({}));
        let payload = build_synthesised_payload(&binding_t, &event).unwrap();
        let meta = extract_nexo_event_source(&payload).unwrap();
        assert_eq!(meta.subject, "webhook.github.x");
        assert_eq!(meta.synthesis_mode, "synthesize");
    }

    #[test]
    fn extract_nexo_event_source_returns_none_when_missing() {
        let payload = serde_json::json!({"kind": "message", "from": "x"});
        assert!(extract_nexo_event_source(&payload).is_none());
    }

    #[test]
    fn extract_nexo_event_source_returns_none_for_malformed_shape() {
        let payload = serde_json::json!({
            EVENT_SOURCE_PAYLOAD_FIELD: "not-an-object"
        });
        assert!(extract_nexo_event_source(&payload).is_none());
    }
}
