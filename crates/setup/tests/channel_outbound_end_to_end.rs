//! Phase 83.8.4.b.3 — end-to-end test that an `OutboundMessage`
//! dispatched through `BrokerOutboundDispatcher` lands on the
//! per-channel `plugin.outbound.<channel>[.<account>]` topic
//! with the payload shape the plugin's existing dispatcher
//! consumes.

use nexo_broker::{AnyBroker, BrokerHandle, LocalBroker};
use nexo_core::agent::admin_rpc::channel_outbound::{
    ChannelOutboundDispatcher, OutboundMessage,
};
use nexo_setup::admin_adapters::{BrokerOutboundDispatcher, WhatsAppTranslator};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn takeover_send_round_trips_through_broker_to_whatsapp_topic() {
    let broker: AnyBroker = AnyBroker::Local(LocalBroker::new());
    let mut sub = broker
        .subscribe("plugin.outbound.whatsapp.test-acct")
        .await
        .expect("subscribe");

    let dispatcher = BrokerOutboundDispatcher::new(broker)
        .with_translator(Box::new(WhatsAppTranslator));

    let msg = OutboundMessage {
        channel: "whatsapp".into(),
        account_id: "test-acct".into(),
        to: "wa.42".into(),
        body: "end-to-end".into(),
        msg_kind: "text".into(),
        attachments: vec![],
        reply_to_msg_id: None,
    };
    let ack = dispatcher.send(msg).await.expect("dispatch");
    assert!(
        ack.outbound_message_id.is_none(),
        "v0 fire-and-forget — provider id correlation deferred to 83.8.4.c"
    );

    let evt = tokio::time::timeout(Duration::from_millis(200), sub.next())
        .await
        .expect("subscriber received event before timeout")
        .expect("subscriber stream open");

    assert_eq!(evt.topic, "plugin.outbound.whatsapp.test-acct");
    assert_eq!(evt.source, "whatsapp");
    assert_eq!(evt.payload["to"], json!("wa.42"));
    assert_eq!(evt.payload["text"], json!("end-to-end"));
    assert_eq!(evt.payload["kind"], json!("text"));
}

#[tokio::test]
async fn dispatcher_publishes_on_default_account_to_base_topic() {
    let broker: AnyBroker = AnyBroker::Local(LocalBroker::new());
    let mut sub = broker
        .subscribe("plugin.outbound.whatsapp")
        .await
        .expect("subscribe");

    let dispatcher = BrokerOutboundDispatcher::new(broker)
        .with_translator(Box::new(WhatsAppTranslator));

    let msg = OutboundMessage {
        channel: "whatsapp".into(),
        account_id: "default".into(),
        to: "wa.0".into(),
        body: "hello".into(),
        msg_kind: "text".into(),
        attachments: vec![],
        reply_to_msg_id: None,
    };
    dispatcher.send(msg).await.expect("dispatch");

    let evt = tokio::time::timeout(Duration::from_millis(200), sub.next())
        .await
        .expect("subscriber received event before timeout")
        .expect("subscriber stream open");
    assert_eq!(evt.topic, "plugin.outbound.whatsapp");
}
