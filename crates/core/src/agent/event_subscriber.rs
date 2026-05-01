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

use nexo_broker::AnyBroker;
use nexo_config::types::event_subscriber::EventSubscriberBinding;
use thiserror::Error;
use tokio::sync::Semaphore;

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
}
