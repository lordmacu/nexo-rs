use nexo_broker::types::Event;
use nexo_broker::{AnyBroker, DiskQueue};
use serde_json::json;

async fn make_queue(max_pending: usize) -> DiskQueue {
    DiskQueue::new(":memory:", max_pending).await.unwrap()
}

fn dummy_event(text: &str) -> Event {
    Event::new("test.topic", "test", json!({ "text": text }))
}

#[tokio::test]
async fn disk_queue_enqueue_drain() {
    let queue = make_queue(100).await;
    let broker = AnyBroker::local();

    let e1 = dummy_event("a");
    let e2 = dummy_event("b");
    let e3 = dummy_event("c");

    queue.enqueue("test.topic", &e1).await.unwrap();
    queue.enqueue("test.topic", &e2).await.unwrap();
    queue.enqueue("test.topic", &e3).await.unwrap();

    assert_eq!(queue.pending_count().await.unwrap(), 3);

    let published = queue.drain(&broker).await.unwrap();
    assert_eq!(published, 3);
    assert_eq!(queue.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn disk_queue_cap_drops_oldest() {
    let queue = make_queue(2).await;

    let e1 = dummy_event("oldest");
    let e2 = dummy_event("middle");
    let e3 = dummy_event("newest");

    queue.enqueue("test.topic", &e1).await.unwrap();
    queue.enqueue("test.topic", &e2).await.unwrap();
    // third enqueue: cap=2, drops oldest (e1)
    queue.enqueue("test.topic", &e3).await.unwrap();

    assert_eq!(queue.pending_count().await.unwrap(), 2);
}

#[tokio::test]
async fn disk_queue_applies_backpressure_over_halfway() {
    use std::time::Instant;

    // max=10, so half=5. Below half: fast. Above half: sleeps up to 500ms.
    let queue = make_queue(10).await;
    for i in 0..5 {
        queue
            .enqueue("test.topic", &dummy_event(&format!("{i}")))
            .await
            .unwrap();
    }

    // At the halfway mark — next enqueue should still be near-instant.
    let t0 = Instant::now();
    queue
        .enqueue("test.topic", &dummy_event("halfway+1"))
        .await
        .unwrap();
    let fast = t0.elapsed();
    assert!(
        fast < std::time::Duration::from_millis(60),
        "enqueue just above half should barely sleep, got {fast:?}"
    );

    // Push to 9/10 — should sleep meaningfully (~400ms).
    for i in 0..3 {
        queue
            .enqueue("test.topic", &dummy_event(&format!("fill-{i}")))
            .await
            .unwrap();
    }
    let t1 = Instant::now();
    queue
        .enqueue("test.topic", &dummy_event("near-cap"))
        .await
        .unwrap();
    let slow = t1.elapsed();
    assert!(
        slow >= std::time::Duration::from_millis(150),
        "enqueue near cap should backpressure, got {slow:?}"
    );
}

#[tokio::test]
async fn disk_queue_moves_to_dlq_after_max_attempts() {
    use nexo_broker::handle::Subscription;
    use nexo_broker::types::BrokerError;
    use nexo_broker::types::Message;
    use nexo_broker::BrokerHandle;
    use async_trait::async_trait;
    use std::time::Duration;

    // Broker that always fails publish
    #[derive(Clone)]
    struct FailBroker;

    #[async_trait]
    impl BrokerHandle for FailBroker {
        async fn publish(&self, _topic: &str, _event: Event) -> Result<(), BrokerError> {
            Err(BrokerError::SendError("simulated failure".into()))
        }
        async fn subscribe(&self, _topic: &str) -> Result<Subscription, BrokerError> {
            unimplemented!()
        }
        async fn request(
            &self,
            _topic: &str,
            _msg: Message,
            _timeout: Duration,
        ) -> Result<Message, BrokerError> {
            unimplemented!()
        }
    }

    let queue = make_queue(100).await;
    let fail_broker = FailBroker;
    let event = dummy_event("fail-me");
    queue.enqueue("test.topic", &event).await.unwrap();

    // 3 drain calls → attempts reaches 3 → moves to DLQ
    queue.drain(&fail_broker).await.unwrap();
    queue.drain(&fail_broker).await.unwrap();
    queue.drain(&fail_broker).await.unwrap();

    assert_eq!(queue.pending_count().await.unwrap(), 0);
    assert_eq!(queue.dead_letter_count().await.unwrap(), 1);
}
