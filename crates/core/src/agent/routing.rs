use agent_broker::{AnyBroker, BrokerHandle, Event};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::timeout;
use uuid::Uuid;

/// RAII cleaner for a pending correlation entry. Ensures the entry is
/// removed on drop — including when the `delegate` future is cancelled
/// mid-await, which the earlier manual `.remove()` at each explicit
/// exit could not cover.
struct PendingGuard {
    map: Arc<DashMap<Uuid, oneshot::Sender<Value>>>,
    id: Uuid,
    armed: bool,
}

impl PendingGuard {
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.armed {
            self.map.remove(&self.id);
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub from: String,
    pub to: String,
    pub correlation_id: Uuid,
    pub payload: AgentPayload,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentPayload {
    Delegate {
        task: String,
        #[serde(default)]
        context: Value,
    },
    Result {
        task_id: Uuid,
        output: Value,
    },
    Broadcast {
        event: String,
        data: Value,
    },
}
#[derive(Default)]
pub struct AgentRouter {
    pending: Arc<DashMap<Uuid, oneshot::Sender<Value>>>,
}
impl AgentRouter {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn delegate(
        &self,
        broker: &AnyBroker,
        from: &str,
        to: &str,
        task: &str,
        mut context: Value,
        timeout_ms: u64,
    ) -> anyhow::Result<Value> {
        // Cycle guard — context carries `_delegate_chain: [from1, from2,
        // …]`. If `to` already appears in the chain, delegating would
        // deadlock both agents waiting on each other's reply. Bail
        // before any broker publish so the pending map stays clean.
        let chain_key = "_delegate_chain";
        let chain: Vec<String> = context
            .get(chain_key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if chain.iter().any(|a| a == to) {
            anyhow::bail!(
                "delegate cycle detected: {to} already in chain [{}]",
                chain.join(" → ")
            );
        }
        let mut new_chain = chain.clone();
        new_chain.push(from.to_string());
        if let Some(map) = context.as_object_mut() {
            map.insert(
                chain_key.to_string(),
                Value::Array(new_chain.into_iter().map(Value::String).collect()),
            );
        } else {
            // Non-object context — wrap so we can attach the chain.
            context = serde_json::json!({
                chain_key: new_chain,
                "original": context,
            });
        }

        let correlation_id = Uuid::new_v4();
        let (tx, rx) = oneshot::channel();
        self.pending.insert(correlation_id, tx);
        // RAII guard ensures the entry leaves the map even if the
        // caller cancels this future before the timeout fires.
        let guard = PendingGuard {
            map: Arc::clone(&self.pending),
            id: correlation_id,
            armed: true,
        };
        let msg = AgentMessage {
            from: from.to_string(),
            to: to.to_string(),
            correlation_id,
            payload: AgentPayload::Delegate {
                task: task.to_string(),
                context,
            },
        };
        let topic = route_topic(to);
        let payload = serde_json::to_value(msg)?;
        let event = Event::new(&topic, from, payload);
        if let Err(e) = broker.publish(&topic, event).await {
            return Err(e.into());
        }
        match timeout(Duration::from_millis(timeout_ms), rx).await {
            Ok(Ok(output)) => {
                // resolve() already removed the entry when it sent;
                // keep the guard armed-as-no-op doesn't matter.
                guard.disarm();
                Ok(output)
            }
            Ok(Err(_)) => {
                anyhow::bail!(
                    "delegate response channel dropped for correlation_id={correlation_id}"
                );
            }
            Err(_) => {
                anyhow::bail!(
                    "delegate timed out after {timeout_ms}ms (correlation_id={correlation_id})"
                );
            }
        }
    }
    pub fn resolve(&self, correlation_id: Uuid, output: Value) -> bool {
        let Some((_, tx)) = self.pending.remove(&correlation_id) else {
            return false;
        };
        tx.send(output).is_ok()
    }

    /// Cleanup helper — returns the current pending entry count. Used
    /// by tests and health checks to verify the map doesn't leak.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}
pub fn route_topic(agent_id: &str) -> String {
    format!("agent.route.{agent_id}")
}
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_delegate_cleans_pending_map() {
        // Spawn delegate on a task so we can `abort()` it cleanly —
        // dropping the pinned future inline can leave the underlying
        // future parked in place even after the `Pin` wrapper is
        // dropped. An aborted task is guaranteed to release state.
        let router = Arc::new(AgentRouter::new());
        let broker = agent_broker::AnyBroker::local();
        let router_in = Arc::clone(&router);
        let broker_in = broker.clone();
        let handle = tokio::spawn(async move {
            router_in
                .delegate(
                    &broker_in,
                    "kate",
                    "research",
                    "task",
                    serde_json::json!({}),
                    60_000,
                )
                .await
        });
        // Let delegate run past publish into the timeout.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(router.pending_count(), 1, "guard should have inserted");
        handle.abort();
        let _ = handle.await;
        assert_eq!(router.pending_count(), 0, "pending entry leaked");
    }

    #[tokio::test]
    async fn delegate_cycle_is_rejected() {
        let router = AgentRouter::new();
        let broker = agent_broker::AnyBroker::local();
        // Simulate A → B delegation that's now about to route back to A
        // (B calling delegate(to="A") with the inherited chain).
        let ctx = serde_json::json!({ "_delegate_chain": ["A"] });
        let err = router
            .delegate(&broker, "B", "A", "task", ctx, 1000)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("cycle detected"), "got: {msg}");
        assert_eq!(router.pending_count(), 0);
    }

    #[test]
    fn agent_message_serde_round_trip() {
        let msg = AgentMessage {
            from: "kate".to_string(),
            to: "research".to_string(),
            correlation_id: Uuid::new_v4(),
            payload: AgentPayload::Delegate {
                task: "find latest updates".to_string(),
                context: serde_json::json!({"priority":"high"}),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.from, "kate");
        assert_eq!(decoded.to, "research");
        match decoded.payload {
            AgentPayload::Delegate { task, context } => {
                assert_eq!(task, "find latest updates");
                assert_eq!(context["priority"], "high");
            }
            _ => panic!("expected delegate payload"),
        }
    }
}
