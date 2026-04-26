//! Phase 67.H.2 — canonical NATS subjects emitted by the dispatch
//! subsystem. Centralised so subscribers (admin-ui A4 tile,
//! external auditors, future replay tools) can match against the
//! exact strings the producer publishes.
//!
//! The producer side ships behind a thin `DispatchTelemetry` trait
//! so the same code path works in tests (NoopTelemetry) and
//! production (NATS-backed). Wiring it into the existing tool
//! call sites stays opt-in: callers that pass `NoopTelemetry`
//! emit nothing, callers that pass the NATS impl get full
//! visibility.

use async_trait::async_trait;
use nexo_driver_types::GoalId;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Subjects owned by this crate. Stable strings — admin-ui and
/// external dashboards subscribe by exact match.
pub mod subject {
    /// `program_phase_dispatch` admitted a goal (status =
    /// Dispatched | Queued).
    pub const DISPATCH_SPAWNED: &str = "agent.dispatch.spawned";
    /// `DispatchGate::check` denied a request.
    pub const DISPATCH_DENIED: &str = "agent.dispatch.denied";
    /// A completion hook fired successfully.
    pub const HOOK_DISPATCHED: &str = "agent.tool.hook.dispatched";
    /// A completion hook attempt errored.
    pub const HOOK_FAILED: &str = "agent.tool.hook.failed";
    /// `agent.registry.snapshot.<goal_id>` — published periodically
    /// by the registry's event subscriber. The wildcard subject
    /// suffix `.*` is the subscribe-side pattern.
    pub const REGISTRY_SNAPSHOT_PREFIX: &str = "agent.registry.snapshot";
}

/// Per-goal snapshot subject: `agent.registry.snapshot.<goal_id>`.
pub fn registry_snapshot_subject(goal_id: GoalId) -> String {
    format!("{}.{}", subject::REGISTRY_SNAPSHOT_PREFIX, goal_id.0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispatchSpawnedPayload {
    pub goal_id: GoalId,
    pub phase_id: String,
    pub queued_position: Option<usize>,
    pub dispatcher_agent_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispatchDeniedPayload {
    pub phase_id: String,
    pub reason: String,
    pub dispatcher_agent_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookDispatchedPayload {
    pub goal_id: GoalId,
    pub transition: String,
    pub kind: String,
    #[serde(with = "humantime_serde")]
    pub latency: Duration,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookFailedPayload {
    pub goal_id: GoalId,
    pub transition: String,
    pub kind: String,
    pub error: String,
}

/// Trait the runtime implements with a NATS-backed publisher; tests
/// inject a `NoopTelemetry` so call sites stay infrastructure-free.
#[async_trait]
pub trait DispatchTelemetry: Send + Sync + 'static {
    async fn dispatch_spawned(&self, payload: DispatchSpawnedPayload);
    async fn dispatch_denied(&self, payload: DispatchDeniedPayload);
    async fn hook_dispatched(&self, payload: HookDispatchedPayload);
    async fn hook_failed(&self, payload: HookFailedPayload);
}

#[derive(Default, Clone, Copy)]
pub struct NoopTelemetry;

/// PT-7 — NATS-backed implementation. Holds an `async_nats::Client`
/// and publishes each event to its canonical subject as JSON.
/// Failures are logged via `tracing` but never bubble up — telemetry
/// must never crash the dispatch hot path.
#[cfg(feature = "nats")]
pub struct NatsDispatchTelemetry {
    client: async_nats::Client,
}

#[cfg(feature = "nats")]
impl NatsDispatchTelemetry {
    pub fn new(client: async_nats::Client) -> Self {
        Self { client }
    }

    async fn publish_json<T: Serialize>(&self, subject: &str, payload: &T) {
        let body = match serde_json::to_vec(payload) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(target: "dispatch.telemetry", "serialize failed: {e}");
                return;
            }
        };
        if let Err(e) = self.client.publish(subject.to_string(), body.into()).await {
            tracing::warn!(target: "dispatch.telemetry", "publish {subject} failed: {e}");
        }
    }
}

#[cfg(feature = "nats")]
#[async_trait]
impl DispatchTelemetry for NatsDispatchTelemetry {
    async fn dispatch_spawned(&self, payload: DispatchSpawnedPayload) {
        self.publish_json(subject::DISPATCH_SPAWNED, &payload).await;
    }
    async fn dispatch_denied(&self, payload: DispatchDeniedPayload) {
        self.publish_json(subject::DISPATCH_DENIED, &payload).await;
    }
    async fn hook_dispatched(&self, payload: HookDispatchedPayload) {
        self.publish_json(subject::HOOK_DISPATCHED, &payload).await;
    }
    async fn hook_failed(&self, payload: HookFailedPayload) {
        self.publish_json(subject::HOOK_FAILED, &payload).await;
    }
}

#[async_trait]
impl DispatchTelemetry for NoopTelemetry {
    async fn dispatch_spawned(&self, _: DispatchSpawnedPayload) {}
    async fn dispatch_denied(&self, _: DispatchDeniedPayload) {}
    async fn hook_dispatched(&self, _: HookDispatchedPayload) {}
    async fn hook_failed(&self, _: HookFailedPayload) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn subjects_are_stable() {
        assert_eq!(subject::DISPATCH_SPAWNED, "agent.dispatch.spawned");
        assert_eq!(subject::DISPATCH_DENIED, "agent.dispatch.denied");
        assert_eq!(subject::HOOK_DISPATCHED, "agent.tool.hook.dispatched");
        assert_eq!(subject::HOOK_FAILED, "agent.tool.hook.failed");
        assert_eq!(subject::REGISTRY_SNAPSHOT_PREFIX, "agent.registry.snapshot");
    }

    #[test]
    fn registry_snapshot_subject_includes_goal_uuid() {
        let g = GoalId(Uuid::nil());
        let s = registry_snapshot_subject(g);
        assert!(s.starts_with("agent.registry.snapshot."));
        assert!(s.ends_with(&Uuid::nil().to_string()));
    }

    #[test]
    fn payloads_round_trip_json() {
        let p = DispatchSpawnedPayload {
            goal_id: GoalId(Uuid::nil()),
            phase_id: "67.10".into(),
            queued_position: Some(1),
            dispatcher_agent_id: "asistente".into(),
        };
        let s = serde_json::to_string(&p).unwrap();
        let _: DispatchSpawnedPayload = serde_json::from_str(&s).unwrap();
    }

    #[tokio::test]
    async fn noop_telemetry_does_not_panic() {
        let t = NoopTelemetry;
        t.dispatch_spawned(DispatchSpawnedPayload {
            goal_id: GoalId(Uuid::nil()),
            phase_id: "x".into(),
            queued_position: None,
            dispatcher_agent_id: "x".into(),
        })
        .await;
        t.dispatch_denied(DispatchDeniedPayload {
            phase_id: "x".into(),
            reason: "x".into(),
            dispatcher_agent_id: "x".into(),
        })
        .await;
        t.hook_dispatched(HookDispatchedPayload {
            goal_id: GoalId(Uuid::nil()),
            transition: "done".into(),
            kind: "notify_origin".into(),
            latency: Duration::from_millis(5),
        })
        .await;
        t.hook_failed(HookFailedPayload {
            goal_id: GoalId(Uuid::nil()),
            transition: "done".into(),
            kind: "shell".into(),
            error: "x".into(),
        })
        .await;
    }
}
