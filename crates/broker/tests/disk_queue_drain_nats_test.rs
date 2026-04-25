//! End-to-end validation of `DiskQueue::drain_nats` against a real NATS server.
//!
//! Gated by `NATS_URL` — skipped otherwise. Covers the recovery path:
//! events queued while NATS was down must be delivered (in order) once the
//! client reconnects.
//!
//! Typical invocation against the docker-compose stack:
//!     NATS_URL=nats://127.0.0.1:4222 cargo test -p nexo-broker --test disk_queue_drain_nats_test -- --nocapture

use std::time::Duration;

use nexo_broker::{DiskQueue, Event};
use futures::StreamExt;
use uuid::Uuid;

fn event(topic: &str, label: &str) -> Event {
    Event::new(topic, "drain-test", serde_json::json!({ "label": label }))
}

#[tokio::test]
async fn drain_nats_publishes_pending_events_in_order() -> anyhow::Result<()> {
    let Ok(nats_url) = std::env::var("NATS_URL") else {
        eprintln!("NATS_URL not set — skipping drain_nats E2E");
        return Ok(());
    };

    // Unique topic so the test doesn't race other runs or live traffic.
    let topic = format!("smoke.drain.{}", Uuid::new_v4());

    // `:memory:` keeps the test hermetic — no disk cleanup, no collisions with
    // other test runs.
    let queue = DiskQueue::new(":memory:", 100).await?;
    queue.enqueue(&topic, &event(&topic, "one")).await?;
    queue.enqueue(&topic, &event(&topic, "two")).await?;
    queue.enqueue(&topic, &event(&topic, "three")).await?;

    let client = async_nats::connect(&nats_url).await?;
    let mut sub = client.subscribe(topic.clone()).await?;
    // Subscribing is async — give the server a moment to wire the interest up
    // so we don't miss the first drained publish.
    client.flush().await?;

    let drained = queue.drain_nats(&client).await?;
    assert_eq!(drained, 3, "expected 3 events drained, got {drained}");

    let mut labels = Vec::new();
    for _ in 0..3 {
        let msg = tokio::time::timeout(Duration::from_secs(5), sub.next())
            .await?
            .expect("subscription closed");
        let event: Event = serde_json::from_slice(&msg.payload)?;
        labels.push(
            event
                .payload
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
        );
    }
    assert_eq!(
        labels,
        vec!["one", "two", "three"],
        "drain_nats must preserve enqueue order"
    );

    // A second drain must report zero — the pending rows were removed on success.
    let residue = queue.drain_nats(&client).await?;
    assert_eq!(
        residue, 0,
        "pending_events not emptied after successful drain"
    );

    Ok(())
}
