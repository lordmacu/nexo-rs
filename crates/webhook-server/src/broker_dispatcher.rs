//! Broker-backed `WebhookDispatcher` — production transport.
//!
//! The trait + envelope live in `nexo-webhook-receiver`; this
//! file is the impl that depends on `nexo-broker`. Splitting like
//! this avoids the circular dep between `nexo-config` (which
//! holds `WebhookServerConfig`) and `nexo-broker`.

use async_trait::async_trait;
use nexo_broker::{handle::BrokerHandle, AnyBroker, Event};
use nexo_webhook_receiver::{DispatchError, WebhookDispatcher, WebhookEnvelope};

pub struct BrokerWebhookDispatcher {
    broker: AnyBroker,
}

impl BrokerWebhookDispatcher {
    pub fn new(broker: AnyBroker) -> Self {
        Self { broker }
    }
}

#[async_trait]
impl WebhookDispatcher for BrokerWebhookDispatcher {
    async fn dispatch(
        &self,
        topic: &str,
        envelope: WebhookEnvelope,
    ) -> Result<(), DispatchError> {
        let source = envelope.source_id.clone();
        let payload = serde_json::to_value(&envelope)
            .map_err(|e| DispatchError::Rejected(format!("envelope serialise: {e}")))?;
        let event = Event::new(topic, source, payload);
        self.broker
            .publish(topic, event)
            .await
            .map_err(|e| DispatchError::Broker(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use uuid::Uuid;

    #[tokio::test]
    async fn publishes_envelope_to_local_broker() {
        let broker = AnyBroker::local();
        let mut sub = broker.subscribe("webhook.x.opened").await.unwrap();
        let disp = BrokerWebhookDispatcher::new(broker);
        let env = WebhookEnvelope {
            schema: 1,
            source_id: "x".into(),
            event_kind: "opened".into(),
            body_json: serde_json::json!({"a":1}),
            headers_subset: BTreeMap::new(),
            received_at_ms: 1,
            envelope_id: Uuid::nil(),
            client_ip: None,
        };
        disp.dispatch("webhook.x.opened", env).await.unwrap();
        let received = sub.next().await.expect("event received");
        assert_eq!(received.topic, "webhook.x.opened");
        assert_eq!(received.source, "x");
        assert_eq!(received.payload["schema"], 1);
        assert_eq!(received.payload["body_json"]["a"], 1);
    }
}
