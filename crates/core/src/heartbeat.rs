use std::time::Duration;

use anyhow::{Context, Result};

use nexo_broker::{AnyBroker, BrokerHandle, Event};
use nexo_config::types::agents::AgentConfig;

pub fn heartbeat_interval(config: &AgentConfig) -> Result<Option<Duration>> {
    if !config.heartbeat.enabled {
        return Ok(None);
    }

    let interval = humantime::parse_duration(&config.heartbeat.interval).with_context(|| {
        format!(
            "invalid heartbeat.interval `{}` for agent {}",
            config.heartbeat.interval, config.id
        )
    })?;

    Ok(Some(interval))
}

pub fn heartbeat_topic(agent_id: &str) -> String {
    format!("agent.events.{agent_id}.heartbeat")
}

pub async fn publish_heartbeat(broker: &AnyBroker, agent_id: &str) -> Result<()> {
    let topic = heartbeat_topic(agent_id);
    let event = Event::new(
        &topic,
        "heartbeat",
        serde_json::json!({
            "agent_id": agent_id,
            "kind": "heartbeat",
        }),
    );

    broker
        .publish(&topic, event)
        .await
        .map_err(|e| anyhow::anyhow!("failed to publish heartbeat for {agent_id}: {e}"))
}
