use agent_broker::types::Event;
use agent_broker::{AnyBroker, BrokerHandle};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn any_broker_local_pubsub() {
    let broker = AnyBroker::local();
    let mut sub = broker.subscribe("test.>").await.unwrap();

    let mut event = Event::new("test.hello", "test", json!({ "msg": "hi" }));
    event.session_id = Some(Uuid::new_v4());
    broker.publish("test.hello", event.clone()).await.unwrap();

    let received = sub.next().await.unwrap();
    assert_eq!(received.id, event.id);
}

#[tokio::test]
async fn any_broker_local_variant() {
    let broker = AnyBroker::local();
    assert!(matches!(broker, AnyBroker::Local(_)));
}
