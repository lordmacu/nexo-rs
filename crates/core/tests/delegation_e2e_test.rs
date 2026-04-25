//! End-to-end test for agent-to-agent delegation over the real NATS broker.
//!
//! Gated by `NATS_URL` — skipped otherwise. Publishes an `AgentMessage` with
//! `AgentPayload::Delegate` to `agent.route.kate` and waits for a matching
//! `AgentPayload::Result` on the reply topic. The LLM call inside kate may
//! succeed or fail; either outcome still produces a `Result` payload (the
//! runtime wraps errors), so this test validates the routing contract
//! independently of any live LLM credentials.
//!
//! Typical invocation against the docker-compose stack:
//!     NATS_URL=nats://127.0.0.1:4222 cargo test -p nexo-core --test delegation_e2e_test -- --nocapture

use std::time::Duration;

use futures::StreamExt;
use nexo_core::agent::{AgentMessage, AgentPayload};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn delegate_to_live_agent_round_trips() -> anyhow::Result<()> {
    let Ok(nats_url) = std::env::var("NATS_URL") else {
        eprintln!("NATS_URL not set — skipping delegation E2E");
        return Ok(());
    };
    // Target must be an agent id that is actually running in the compose stack.
    let target = std::env::var("DELEGATE_TARGET").unwrap_or_else(|_| "kate".into());

    let client = async_nats::connect(&nats_url).await?;
    // Unique caller id so this run doesn't race other tests or real agents.
    let caller_id = format!("smoke-caller-{}", Uuid::new_v4());
    let reply_topic = format!("agent.route.{caller_id}");
    let mut replies = client.subscribe(reply_topic.clone()).await?;

    let correlation_id = Uuid::new_v4();
    let msg = AgentMessage {
        from: caller_id.clone(),
        to: target.clone(),
        correlation_id,
        payload: AgentPayload::Delegate {
            task: "integration-smoke ping".into(),
            context: json!({ "source": "integration_stack_smoke" }),
        },
    };
    // The runtime deserialises the broker Event's payload as AgentMessage, so
    // wrap the AgentMessage exactly the way AnyBroker::publish would.
    let event_payload = serde_json::json!({
        "id": Uuid::new_v4(),
        "timestamp": chrono::Utc::now(),
        "topic": format!("agent.route.{target}"),
        "source": caller_id.clone(),
        "session_id": null,
        "payload": msg,
    });
    client
        .publish(
            format!("agent.route.{target}"),
            serde_json::to_vec(&event_payload)?.into(),
        )
        .await?;
    client.flush().await?;

    // Wait up to 30s for a Result with matching correlation_id.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Some(msg) = tokio::time::timeout(remaining, replies.next()).await? else {
            anyhow::bail!("reply subscription closed before response arrived");
        };
        let event: serde_json::Value = serde_json::from_slice(&msg.payload)?;
        let Some(inner) = event.get("payload") else {
            continue;
        };
        let Ok(reply) = serde_json::from_value::<AgentMessage>(inner.clone()) else {
            continue;
        };
        if reply.correlation_id != correlation_id {
            continue;
        }
        match reply.payload {
            AgentPayload::Result { task_id, output } => {
                assert_eq!(task_id, correlation_id);
                eprintln!("delegation reply output: {output}");
                // The runtime always produces an object with either `text` (LLM ok)
                // or `error` (LLM failed / other). Both shapes prove the routing
                // contract worked.
                assert!(
                    output.get("text").is_some() || output.get("error").is_some(),
                    "unexpected reply output shape: {output}"
                );
                return Ok(());
            }
            other => anyhow::bail!("expected Result, got {other:?}"),
        }
    }
    anyhow::bail!("timed out waiting for delegation reply");
}
