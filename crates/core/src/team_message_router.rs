//! Phase 79.6 — broker bridge for team DM + broadcast messages.
//!
//! Two topic patterns:
//! * `team.<team_id>.dm.<member_name>` — point-to-point.
//! * `team.<team_id>.broadcast` — fan-out from the lead.
//!
//! Each pattern carries a `DmFrame` JSON payload. Members
//! subscribe to a per-team `tokio::sync::broadcast::Sender` that
//! the router multiplexes (DMs to the named member + every
//! broadcast). The broker side listens to `team.>` once per
//! process; per-member subscriptions are in-memory.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/SendMessageTool/SendMessageTool.ts:1-58`
//!     — leak's discriminated union of structured messages
//!     (`shutdown_request`, `shutdown_response`,
//!     `plan_approval_response`). The leak persists DMs in a
//!     per-teammate file (`writeToMailbox`); we replace with
//!     broker subjects so members can be idle (goal in
//!     `Pending`) and still receive messages.
//!   * `claude-code-leak/src/tools/TeamCreateTool/prompt.ts:53-61`
//!     — "Messages from teammates are automatically delivered to
//!     you. You do NOT need to manually check your inbox." We
//!     mirror that semantics: a DM landing on a `Pending` goal
//!     transitions it to `Running` via Phase 67's wake-on hook.

use nexo_broker::{BrokerHandle, Event};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

const PER_TEAM_BUFFER: usize = 64;
const SOURCE_TAG: &str = "team-router";

/// One in-flight team message. The `to` field discriminates
/// point-to-point (`<member_name>`) vs fan-out (`"broadcast"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DmFrame {
    pub team_id: String,
    pub to: String,
    pub from: String,
    pub body: serde_json::Value,
    pub correlation_id: Option<String>,
}

pub struct TeamMessageRouter<B: BrokerHandle + ?Sized> {
    broker: Arc<B>,
    per_team: dashmap::DashMap<String, broadcast::Sender<DmFrame>>,
}

impl<B: BrokerHandle + ?Sized + 'static> TeamMessageRouter<B> {
    pub fn new(broker: Arc<B>) -> Arc<Self> {
        Arc::new(Self {
            broker,
            per_team: dashmap::DashMap::new(),
        })
    }

    /// Spawn the broker subscriber on `team.>`. Cancelled by
    /// dropping the supplied `CancellationToken`. Returns the
    /// `JoinHandle` so the caller can `await` graceful shutdown.
    pub fn spawn(self: &Arc<Self>, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut sub = match me.broker.subscribe("team.>").await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        target: "team::router",
                        error = %e,
                        "[team] could not subscribe to team.> — DMs offline"
                    );
                    return;
                }
            };
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    next = sub.next() => {
                        match next {
                            Some(ev) => me.dispatch_inbound(ev),
                            None => break,
                        }
                    }
                }
            }
        })
    }

    fn dispatch_inbound(&self, ev: Event) {
        // Topic shape: `team.<team_id>.dm.<name>` or
        // `team.<team_id>.broadcast`.
        let parts: Vec<&str> = ev.topic.splitn(4, '.').collect();
        if parts.len() < 3 || parts[0] != "team" {
            return;
        }
        let team_id = parts[1].to_string();
        let frame: DmFrame = match serde_json::from_value(ev.payload) {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!(
                    target: "team::router",
                    topic = %ev.topic,
                    error = %e,
                    "[team] dropping malformed DmFrame"
                );
                return;
            }
        };
        if let Some(sender) = self.per_team.get(&team_id) {
            let _ = sender.send(frame);
        }
    }

    /// Publish a DM to a single team member. `to` is the
    /// member's human-readable name (e.g. `"researcher"`).
    pub async fn publish_dm(
        &self,
        team_id: &str,
        from: &str,
        to: &str,
        body: serde_json::Value,
        correlation_id: Option<String>,
    ) -> anyhow::Result<()> {
        let topic = format!("team.{team_id}.dm.{to}");
        let frame = DmFrame {
            team_id: team_id.to_string(),
            to: to.to_string(),
            from: from.to_string(),
            body,
            correlation_id,
        };
        let payload = serde_json::to_value(&frame)?;
        let event = Event::new(topic.clone(), SOURCE_TAG, payload);
        self.broker.publish(&topic, event).await?;
        Ok(())
    }

    /// Publish a broadcast to every member of a team. `from` is
    /// the lead's name (typically `"team-lead"`).
    pub async fn publish_broadcast(
        &self,
        team_id: &str,
        from: &str,
        body: serde_json::Value,
    ) -> anyhow::Result<()> {
        let topic = format!("team.{team_id}.broadcast");
        let frame = DmFrame {
            team_id: team_id.to_string(),
            to: "broadcast".to_string(),
            from: from.to_string(),
            body,
            correlation_id: None,
        };
        let payload = serde_json::to_value(&frame)?;
        let event = Event::new(topic.clone(), SOURCE_TAG, payload);
        self.broker.publish(&topic, event).await?;
        Ok(())
    }

    /// Subscribe a member runtime to its team's DM + broadcast
    /// stream. The receiver yields every message addressed to
    /// `name` plus every broadcast for `team_id`. The caller
    /// MUST filter on its own `name` (the broadcast subject is
    /// shared across the team).
    pub fn subscribe_member(&self, team_id: &str, _name: &str) -> broadcast::Receiver<DmFrame> {
        let entry = self
            .per_team
            .entry(team_id.to_string())
            .or_insert_with(|| broadcast::channel(PER_TEAM_BUFFER).0);
        entry.subscribe()
    }

    /// Drop the per-team channel. After this, future inbound
    /// messages for `team_id` are silently dropped at the
    /// router. Called by `TeamDelete` post soft-delete.
    pub fn drop_team(&self, team_id: &str) {
        self.per_team.remove(team_id);
    }

    /// Number of teams currently routed (diagnostics).
    pub fn live_teams(&self) -> usize {
        self.per_team.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_broker::AnyBroker;
    use serde_json::json;

    /// In-memory broker for fast tests. Production uses NATS via
    /// `AnyBroker::Nats(...)`; the local broker has identical
    /// publish/subscribe semantics for our purposes.
    fn local_broker() -> Arc<AnyBroker> {
        Arc::new(AnyBroker::local())
    }

    #[tokio::test]
    async fn publish_dm_routes_to_subscriber() {
        let broker = local_broker();
        let router = TeamMessageRouter::new(broker.clone());
        let cancel = CancellationToken::new();
        router.spawn(cancel.clone());

        let mut rx = router.subscribe_member("feature-x", "researcher");
        // Give the spawned subscriber a moment to set up.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        router
            .publish_dm(
                "feature-x",
                "team-lead",
                "researcher",
                json!({"hello": 1}),
                None,
            )
            .await
            .unwrap();

        let frame = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out")
            .expect("recv error");
        assert_eq!(frame.team_id, "feature-x");
        assert_eq!(frame.to, "researcher");
        assert_eq!(frame.body["hello"], 1);
        cancel.cancel();
    }

    #[tokio::test]
    async fn publish_broadcast_fans_out_to_all_subscribers() {
        let broker = local_broker();
        let router = TeamMessageRouter::new(broker.clone());
        let cancel = CancellationToken::new();
        router.spawn(cancel.clone());

        let mut rx_a = router.subscribe_member("feature-x", "researcher");
        let mut rx_b = router.subscribe_member("feature-x", "tester");
        let mut rx_c = router.subscribe_member("feature-x", "tech-writer");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        router
            .publish_broadcast(
                "feature-x",
                "team-lead",
                json!({"type": "shutdown_request"}),
            )
            .await
            .unwrap();

        for rx in [&mut rx_a, &mut rx_b, &mut rx_c] {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                .await
                .expect("timed out")
                .expect("recv error");
            assert_eq!(frame.to, "broadcast");
            assert_eq!(frame.body["type"], "shutdown_request");
        }
        cancel.cancel();
    }

    #[tokio::test]
    async fn drop_team_unsubscribes_and_future_publishes_drop() {
        let broker = local_broker();
        let router = TeamMessageRouter::new(broker.clone());
        let cancel = CancellationToken::new();
        router.spawn(cancel.clone());

        let mut rx = router.subscribe_member("feature-x", "researcher");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        router.drop_team("feature-x");
        // Subscribe receivers see the channel close once their
        // sender drops.
        router
            .publish_dm(
                "feature-x",
                "team-lead",
                "researcher",
                json!({"hi": 1}),
                None,
            )
            .await
            .unwrap();
        // Receiver: either Closed (sender dropped) or no message
        // before timeout.
        let res = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await;
        match res {
            Ok(Err(_lagged_or_closed)) => {} // expected
            Err(_timeout) => {}              // expected
            Ok(Ok(_)) => panic!("dropped team should not deliver new messages"),
        }
        assert_eq!(router.live_teams(), 0);
        cancel.cancel();
    }

    #[tokio::test]
    async fn malformed_dm_frame_is_dropped_silently() {
        // Direct-publish a non-DmFrame payload; the router should
        // log + skip without crashing.
        let broker = local_broker();
        let router = TeamMessageRouter::new(broker.clone());
        let cancel = CancellationToken::new();
        router.spawn(cancel.clone());

        let _rx = router.subscribe_member("feature-x", "researcher");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let bad = Event::new(
            "team.feature-x.dm.researcher",
            "manual",
            json!({"not_a_dmframe": true}),
        );
        broker
            .publish("team.feature-x.dm.researcher", bad)
            .await
            .unwrap();
        // Just ensure no panic + tasks alive a beat later.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn topic_with_too_few_parts_is_dropped() {
        let broker = local_broker();
        let router = TeamMessageRouter::new(broker.clone());
        let cancel = CancellationToken::new();
        router.spawn(cancel.clone());

        // Publish on `team.foo` (missing the third segment).
        // dispatch_inbound returns early; nothing crashes.
        let bad = Event::new("team.foo", "manual", json!({}));
        let _ = broker.publish("team.foo", bad).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn correlation_id_passes_through_unchanged() {
        let broker = local_broker();
        let router = TeamMessageRouter::new(broker.clone());
        let cancel = CancellationToken::new();
        router.spawn(cancel.clone());

        let mut rx = router.subscribe_member("feature-x", "researcher");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        router
            .publish_dm(
                "feature-x",
                "team-lead",
                "researcher",
                json!({"q": "ready?"}),
                Some("corr-42".into()),
            )
            .await
            .unwrap();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.correlation_id.as_deref(), Some("corr-42"));
        cancel.cancel();
    }
}
