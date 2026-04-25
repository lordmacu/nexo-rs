//! Resolve outbound deliveries from a `TickOutcome` and publish them
//! to the right per-instance topic. Modules return `OutboundDelivery`
//! instances; the runner calls [`publish`] once per item — that's the
//! one place that touches Phase 17 resolver semantics so audit /
//! metrics happen in a single spot.

use nexo_auth::{AgentCredentialResolver, Channel};
use nexo_broker::{AnyBroker, BrokerHandle, Event};
use serde_json::json;

use crate::error::PollerError;
use crate::poller::OutboundDelivery;

/// Topic prefix per channel — must match what the WhatsApp / Telegram
/// plugins subscribe to in their dispatchers.
fn topic_for(channel: Channel, instance: &str) -> String {
    format!("plugin.outbound.{channel}.{instance}")
}

const SOURCE: &str = "plugin.poller";

pub async fn publish(
    broker: &AnyBroker,
    resolver: &AgentCredentialResolver,
    agent_id: &str,
    delivery: &OutboundDelivery,
) -> Result<(), PollerError> {
    let handle = resolver.resolve(agent_id, delivery.channel).map_err(|_| {
        PollerError::CredentialsMissing {
            agent: agent_id.to_string(),
            channel: delivery.channel,
        }
    })?;

    let topic = topic_for(delivery.channel, handle.account_id_raw());

    // Mirror the WhatsApp/Telegram tool envelope so plugin dispatchers
    // accept the message without a special branch.
    let mut payload = delivery.payload.clone();
    if let Some(map) = payload.as_object_mut() {
        map.entry("to".to_string())
            .or_insert(json!(delivery.recipient));
    } else {
        // payload is e.g. a bare string — wrap it.
        payload = json!({ "text": payload, "to": delivery.recipient });
    }

    nexo_auth::audit::audit_outbound(&handle, &topic);
    nexo_auth::telemetry::inc_usage(
        delivery.channel,
        handle.account_id_raw(),
        agent_id,
        "outbound",
    );

    let event = Event::new(&topic, SOURCE, payload);
    broker
        .publish(&topic, event)
        .await
        .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_auth::handle::{CredentialHandle, WHATSAPP};
    use nexo_auth::resolver::AgentCredentialResolver;
    use nexo_broker::AnyBroker;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn build_resolver_for_ana_personal() -> AgentCredentialResolver {
        let h = CredentialHandle::new(WHATSAPP, "personal", "ana");
        let mut inner: HashMap<&'static str, CredentialHandle> = HashMap::new();
        inner.insert(WHATSAPP, h);
        let mut outer: HashMap<Arc<str>, HashMap<&'static str, CredentialHandle>> = HashMap::new();
        outer.insert(Arc::from("ana"), inner);
        AgentCredentialResolver::from_raw(outer)
    }

    #[tokio::test]
    async fn publish_routes_to_per_instance_topic() {
        let broker = AnyBroker::local();
        let mut sub = broker
            .subscribe("plugin.outbound.whatsapp.personal")
            .await
            .unwrap();
        let resolver = build_resolver_for_ana_personal();
        let delivery = OutboundDelivery {
            channel: WHATSAPP,
            recipient: "57300@s.whatsapp.net".into(),
            payload: json!({ "text": "hello" }),
        };
        publish(&broker, &resolver, "ana", &delivery).await.unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(1), sub.next())
            .await
            .unwrap()
            .expect("event received");
        assert_eq!(ev.topic, "plugin.outbound.whatsapp.personal");
        assert_eq!(ev.payload["to"], "57300@s.whatsapp.net");
        assert_eq!(ev.payload["text"], "hello");
    }

    #[tokio::test]
    async fn publish_errors_when_agent_has_no_binding() {
        let broker = AnyBroker::local();
        let resolver = AgentCredentialResolver::empty();
        let delivery = OutboundDelivery {
            channel: WHATSAPP,
            recipient: "x".into(),
            payload: json!({}),
        };
        let err = publish(&broker, &resolver, "ana", &delivery)
            .await
            .unwrap_err();
        assert!(matches!(err, PollerError::CredentialsMissing { .. }));
    }

    #[tokio::test]
    async fn publish_wraps_string_payload() {
        let broker = AnyBroker::local();
        let mut sub = broker
            .subscribe("plugin.outbound.whatsapp.personal")
            .await
            .unwrap();
        let resolver = build_resolver_for_ana_personal();
        let delivery = OutboundDelivery {
            channel: WHATSAPP,
            recipient: "57300@s.whatsapp.net".into(),
            payload: json!("plain text"),
        };
        publish(&broker, &resolver, "ana", &delivery).await.unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(1), sub.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ev.payload["text"], "plain text");
        assert_eq!(ev.payload["to"], "57300@s.whatsapp.net");
    }
}
