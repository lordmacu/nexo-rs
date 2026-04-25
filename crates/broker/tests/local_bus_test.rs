use std::time::Duration;

use nexo_broker::{BrokerHandle, Event, LocalBroker, Message};
use serde_json::json;

#[tokio::test]
async fn pubsub_basic() {
    let broker = LocalBroker::new();

    let mut sub = broker.subscribe("plugin.inbound.whatsapp").await.unwrap();

    let event = Event::new("plugin.inbound.whatsapp", "test", json!({"text": "hello"}));
    broker
        .publish("plugin.inbound.whatsapp", event.clone())
        .await
        .unwrap();

    let received = sub.next().await.unwrap();
    assert_eq!(received.topic, "plugin.inbound.whatsapp");
    assert_eq!(received.payload, json!({"text": "hello"}));
}

#[tokio::test]
async fn pubsub_wildcard_single_segment() {
    let broker = LocalBroker::new();

    let mut sub = broker.subscribe("agent.events.*").await.unwrap();

    let e1 = Event::new("agent.events.kate", "test", json!(1));
    let e2 = Event::new("agent.events.ventas", "test", json!(2));
    let e3 = Event::new("agent.events.kate.click", "test", json!(3));

    broker.publish("agent.events.kate", e1).await.unwrap();
    broker.publish("agent.events.ventas", e2).await.unwrap();
    broker.publish("agent.events.kate.click", e3).await.unwrap();

    let r1 = sub.next().await.unwrap();
    assert_eq!(r1.payload, json!(1));

    let r2 = sub.next().await.unwrap();
    assert_eq!(r2.payload, json!(2));

    // e3 should NOT arrive — verify with a timeout
    let timeout = tokio::time::timeout(Duration::from_millis(50), sub.next()).await;
    assert!(
        timeout.is_err(),
        "wildcard * should not match multi-segment topic"
    );
}

#[tokio::test]
async fn request_response() {
    let broker = LocalBroker::new();
    let broker2 = broker.clone();

    // Responder task: listens on topic, replies to reply_to inbox
    tokio::spawn(async move {
        let mut sub = broker2.subscribe("service.echo").await.unwrap();
        if let Some(event) = sub.next().await {
            let req: Message = serde_json::from_value(event.payload).unwrap();
            if let Some(inbox) = req.reply_to {
                let reply = Message::new(&inbox, json!({"echo": req.payload}));
                let reply_event =
                    Event::new(&inbox, "responder", serde_json::to_value(&reply).unwrap());
                broker2.publish(&inbox, reply_event).await.unwrap();
            }
        }
    });

    tokio::time::sleep(Duration::from_millis(10)).await;

    let msg = Message::new("service.echo", json!({"input": "ping"}));
    let reply = broker
        .request("service.echo", msg, Duration::from_secs(1))
        .await
        .unwrap();

    assert_eq!(reply.payload["echo"]["input"], "ping");
}

#[tokio::test]
async fn request_timeout() {
    let broker = LocalBroker::new();

    let msg = Message::new("service.nobody_listening", json!({}));
    let result = broker
        .request("service.nobody_listening", msg, Duration::from_millis(50))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("timed out"),
        "expected timeout error, got: {err}"
    );
}

#[tokio::test]
async fn dead_subscriber_does_not_panic() {
    let broker = LocalBroker::new();

    {
        let _sub = broker.subscribe("some.topic").await.unwrap();
        // _sub dropped here — channel closed
    }

    // publish after subscriber is dead: should not panic or error
    let event = Event::new("some.topic", "test", json!({}));
    let result = broker.publish("some.topic", event).await;
    assert!(result.is_ok());
}
