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

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use nexo_broker::{handle::BrokerHandle, AnyBroker, Event};
use nexo_config::types::agents::InboundBinding;
use nexo_config::types::event_subscriber::{
    EventSubscriberBinding, OverflowPolicy, SynthesisMode,
};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio_util::sync::CancellationToken;
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

/// Synthesise per-`event_subscribers` `InboundBinding` entries
/// for each declared subscriber that doesn't already have one.
///
/// Idempotent — operator-declared bindings (manual override of
/// `{ plugin: "event", instance: "<id>" }`) survive untouched.
/// Returns the extended binding list (declared + auto-synth).
///
/// In-memory only: never mutates the operator's YAML. Boot
/// supervisor calls this when constructing the agent runtime so
/// the existing inbound resolver can match
/// `plugin.inbound.event.<id>` re-publishes against an
/// agent-recognised binding.
pub fn synthesize_event_inbound_bindings(
    declared: &[InboundBinding],
    event_subscribers: &[EventSubscriberBinding],
) -> Vec<InboundBinding> {
    let mut out = declared.to_vec();
    for sub in event_subscribers {
        let already = out.iter().any(|b| {
            b.plugin == EVENT_BINDING_CHANNEL && b.instance.as_deref() == Some(&sub.id)
        });
        if !already {
            let mut binding = InboundBinding::default();
            binding.plugin = EVENT_BINDING_CHANNEL.into();
            binding.instance = Some(sub.id.clone());
            out.push(binding);
        }
    }
    out
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

/// Subscribe → render → republish loop until `cancel` fires.
///
/// `Off` mode short-circuits at entry (info-log + return Ok).
/// Other modes run a producer (subscribe loop) → bounded mpsc
/// channel → consumer (render + publish) pipeline. Buffer
/// overflow follows `binding.overflow_policy`. Concurrency cap
/// enforced via `Arc<Semaphore>`. Cancel-token shutdown drops
/// the broker subscription, drops the producer side of the
/// channel, and waits ≤1s for the consumer to drain.
pub async fn run_event_subscriber(
    sub: Arc<EventSubscriber>,
    cancel: CancellationToken,
) -> Result<(), EventSubscriberError> {
    if sub.binding.synthesize_inbound == SynthesisMode::Off {
        tracing::info!(
            agent_id = %sub.agent_id,
            binding_id = %sub.binding.id,
            "event_subscriber off — task exit"
        );
        return Ok(());
    }

    let mut subscription = sub
        .broker
        .subscribe(&sub.binding.subject_pattern)
        .await
        .map_err(|e| EventSubscriberError::BrokerSubscribe {
            subject: sub.binding.subject_pattern.clone(),
            detail: e.to_string(),
        })?;

    tracing::info!(
        agent_id = %sub.agent_id,
        binding_id = %sub.binding.id,
        subject = %sub.binding.subject_pattern,
        "event_subscriber started"
    );

    // Shared deque + notifier. Producer pushes (or drops oldest
    // if full); consumer waits on `notify` and pops one at a time.
    // The deque is the unit of overflow control — `mpsc` would
    // hide the queue from the producer and prevent DropOldest.
    let buffer: Arc<Mutex<VecDeque<Event>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(sub.binding.max_buffer)));
    let notify = Arc::new(Notify::new());
    let consumer_done = Arc::new(Notify::new());

    let consumer_sub = Arc::clone(&sub);
    let consumer_buffer = Arc::clone(&buffer);
    let consumer_notify = Arc::clone(&notify);
    let consumer_done_emit = Arc::clone(&consumer_done);
    let consumer_cancel = cancel.clone();
    let consumer = tokio::spawn(async move {
        loop {
            // Wait for new event OR cancel.
            tokio::select! {
                biased;
                _ = consumer_cancel.cancelled() => break,
                _ = consumer_notify.notified() => {}
            }
            // Drain everything currently in the buffer, one at a
            // time, respecting the semaphore.
            loop {
                let event = {
                    let mut g = consumer_buffer.lock().await;
                    g.pop_front()
                };
                let Some(event) = event else { break };

                let permit = match &consumer_sub.semaphore {
                    Some(sem) => match Arc::clone(sem).acquire_owned().await {
                        Ok(p) => Some(p),
                        Err(_) => return,
                    },
                    None => None,
                };

                if let Some(payload) =
                    build_synthesised_payload(&consumer_sub.binding, &event)
                {
                    let topic = event_inbound_topic(&consumer_sub.binding.id);
                    let republish = Event::new(
                        topic.clone(),
                        format!("event_subscriber:{}", consumer_sub.binding.id),
                        payload,
                    );
                    if let Err(e) = consumer_sub.broker.publish(&topic, republish).await {
                        tracing::warn!(
                            agent_id = %consumer_sub.agent_id,
                            binding_id = %consumer_sub.binding.id,
                            topic = %topic,
                            error = %e,
                            "event_subscriber publish failed — dropping event"
                        );
                    }
                }
                drop(permit);
            }
        }
        consumer_done_emit.notify_one();
    });

    // Producer loop.
    let mut dropped_oldest: u64 = 0;
    let mut dropped_newest: u64 = 0;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                tracing::info!(
                    agent_id = %sub.agent_id,
                    binding_id = %sub.binding.id,
                    "event_subscriber cancel — draining"
                );
                break;
            }
            ev = subscription.next() => {
                let Some(event) = ev else { break };
                let max_buffer = sub.binding.max_buffer;
                let mut buf = buffer.lock().await;
                if buf.len() >= max_buffer {
                    match sub.binding.overflow_policy {
                        OverflowPolicy::DropOldest => {
                            buf.pop_front();
                            buf.push_back(event);
                            dropped_oldest += 1;
                            tracing::warn!(
                                agent_id = %sub.agent_id,
                                binding_id = %sub.binding.id,
                                dropped_total = dropped_oldest,
                                "event_subscriber buffer full — drop-oldest"
                            );
                        }
                        OverflowPolicy::DropNewest => {
                            dropped_newest += 1;
                            tracing::warn!(
                                agent_id = %sub.agent_id,
                                binding_id = %sub.binding.id,
                                dropped_total = dropped_newest,
                                "event_subscriber buffer full — drop-newest"
                            );
                        }
                        _ => {
                            // Conservative fallback for future variants.
                            dropped_newest += 1;
                        }
                    }
                } else {
                    buf.push_back(event);
                }
                drop(buf);
                notify.notify_one();
            }
        }
    }

    // Cancel triggers consumer drain via shared cancel; await
    // its completion with bounded timeout.
    let _ = tokio::time::timeout(Duration::from_secs(1), consumer_done.notified()).await;
    consumer.abort();

    tracing::info!(
        agent_id = %sub.agent_id,
        binding_id = %sub.binding.id,
        dropped_oldest,
        dropped_newest,
        "event_subscriber stopped"
    );
    Ok(())
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

    #[test]
    fn synthesize_event_inbound_appends_when_absent() {
        let declared: Vec<InboundBinding> = vec![];
        let subs = vec![mk_binding("github", "x.>"), mk_binding("stripe", "y.>")];
        let out = synthesize_event_inbound_bindings(&declared, &subs);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].plugin, "event");
        assert_eq!(out[0].instance.as_deref(), Some("github"));
        assert_eq!(out[1].plugin, "event");
        assert_eq!(out[1].instance.as_deref(), Some("stripe"));
    }

    #[test]
    fn synthesize_event_inbound_idempotent_when_manual_present() {
        let mut manual = InboundBinding::default();
        manual.plugin = "event".into();
        manual.instance = Some("github".into());
        // Tag a custom field so we can prove the manual is preserved
        manual.allowed_tools = Some(vec!["custom_tool".into()]);
        let declared = vec![manual.clone()];
        let subs = vec![mk_binding("github", "x.>")];
        let out = synthesize_event_inbound_bindings(&declared, &subs);
        assert_eq!(out.len(), 1, "manual override preserved, no duplicate");
        assert_eq!(out[0].allowed_tools.as_ref().unwrap()[0], "custom_tool");
    }

    #[test]
    fn synthesize_event_inbound_partial_overlap() {
        let mut manual = InboundBinding::default();
        manual.plugin = "event".into();
        manual.instance = Some("github".into());
        let declared = vec![manual];
        let subs = vec![mk_binding("github", "x.>"), mk_binding("stripe", "y.>")];
        let out = synthesize_event_inbound_bindings(&declared, &subs);
        assert_eq!(out.len(), 2);
        // github survives manual; stripe synthesised.
        assert_eq!(out[0].instance.as_deref(), Some("github"));
        assert_eq!(out[1].instance.as_deref(), Some("stripe"));
    }

    #[test]
    fn synthesize_event_inbound_preserves_unrelated_bindings() {
        let mut wa = InboundBinding::default();
        wa.plugin = "whatsapp".into();
        wa.instance = Some("personal".into());
        let declared = vec![wa];
        let subs = vec![mk_binding("github", "x.>")];
        let out = synthesize_event_inbound_bindings(&declared, &subs);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].plugin, "whatsapp");
        assert_eq!(out[1].plugin, "event");
    }

    // ────────────────────────────────────────────────────────
    // Step 5+6 — runtime loop
    // ────────────────────────────────────────────────────────

    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_loop_synthesise_republishes() {
        let broker = AnyBroker::local();
        let mut binding = mk_binding("github", "test.>");
        binding.inbound_template = Some("got {{event_kind}}".into());
        let sub = Arc::new(EventSubscriber::new("ana", binding, broker.clone()));
        let cancel = CancellationToken::new();

        // Subscribe FIRST so we don't miss the republish.
        let mut listener = broker
            .subscribe("plugin.inbound.event.github")
            .await
            .unwrap();

        let task = tokio::spawn(run_event_subscriber(Arc::clone(&sub), cancel.clone()));
        // Let the producer subscribe.
        tokio::time::sleep(Duration::from_millis(50)).await;

        broker
            .publish(
                "test.opened",
                Event::new(
                    "test.opened",
                    "tester",
                    serde_json::json!({"event_kind": "opened"}),
                ),
            )
            .await
            .unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), listener.next())
            .await
            .expect("timeout waiting for republish")
            .expect("no event");
        assert_eq!(received.payload["text"], "got opened");
        assert_eq!(received.payload["from"], "event:github");
        assert_eq!(
            received.payload[EVENT_SOURCE_PAYLOAD_FIELD]["synthesis_mode"],
            "synthesize"
        );

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_loop_tick_republishes_marker() {
        let broker = AnyBroker::local();
        let mut binding = mk_binding("alerts", "alert.>");
        binding.synthesize_inbound = SynthesisMode::Tick;
        let sub = Arc::new(EventSubscriber::new("ana", binding, broker.clone()));
        let cancel = CancellationToken::new();

        let mut listener = broker
            .subscribe("plugin.inbound.event.alerts")
            .await
            .unwrap();
        let task = tokio::spawn(run_event_subscriber(Arc::clone(&sub), cancel.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;

        broker
            .publish(
                "alert.cpu.high",
                Event::new("alert.cpu.high", "tester", serde_json::json!({})),
            )
            .await
            .unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), listener.next())
            .await
            .unwrap()
            .unwrap();
        let text = received.payload["text"].as_str().unwrap();
        assert!(text.starts_with("<event subject=\"alert.cpu.high\""));

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_loop_off_mode_exits_immediately() {
        let broker = AnyBroker::local();
        let mut binding = mk_binding("x", "y.>");
        binding.synthesize_inbound = SynthesisMode::Off;
        let sub = Arc::new(EventSubscriber::new("ana", binding, broker));
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_event_subscriber(Arc::clone(&sub), cancel));
        // Off mode short-circuits — task should finish on its own
        // without ever needing cancel.
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("task should finish without cancel");
        assert!(result.unwrap().is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_loop_cancel_token_stops_within_one_second() {
        let broker = AnyBroker::local();
        let binding = mk_binding("x", "y.>");
        let sub = Arc::new(EventSubscriber::new("ana", binding, broker));
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_event_subscriber(Arc::clone(&sub), cancel.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_secs(2), task).await;
        assert!(res.is_ok(), "task should join within 2s of cancel");
    }
}
