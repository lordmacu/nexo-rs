use std::time::Duration;

#[cfg(test)]
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::manager::FlowManager;
use crate::types::{Flow, FlowError, FlowStatus};

/// Typed view of `Flow.wait_json`. The engine evaluates this at every tick
/// to decide whether a `Waiting` flow can be resumed.
///
/// Host code (e.g. NATS bridge) is intentionally not coupled here — for
/// `ExternalEvent` the host calls `WaitEngine::try_resume_external` when a
/// matching message arrives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitCondition {
    /// Resume when wall-clock time reaches `at`.
    Timer { at: DateTime<Utc> },
    /// Resume when an external event for `(topic, correlation_id)` arrives.
    ExternalEvent {
        topic: String,
        correlation_id: String,
    },
    /// Only resumed by an explicit `manager.resume(...)` call.
    Manual,
}

impl WaitCondition {
    pub fn into_value(self) -> Value {
        serde_json::to_value(self).expect("WaitCondition is always serializable")
    }

    pub fn from_value(v: &Value) -> Option<Self> {
        serde_json::from_value(v.clone()).ok()
    }
}

/// Drives `Waiting` flows back into `Running` (or `Cancelled`) based on
/// their `WaitCondition`.
///
/// The engine is intentionally minimal — no broker, no internal scheduler.
/// `tick()` is pull-based and idempotent; the host (or Phase 7 heartbeat)
/// invokes it on whatever cadence is appropriate. `run(interval, shutdown)`
/// is provided as a convenience for the common case.
#[derive(Clone)]
pub struct WaitEngine {
    manager: FlowManager,
}

#[derive(Debug, Default, Clone)]
pub struct TickReport {
    pub scanned: usize,
    pub resumed: usize,
    pub cancelled: usize,
    pub still_waiting: usize,
    pub errors: usize,
}

impl WaitEngine {
    pub fn new(manager: FlowManager) -> Self {
        Self { manager }
    }

    pub fn manager(&self) -> &FlowManager {
        &self.manager
    }

    /// Single pass over all `Waiting` flows. Returns counters for telemetry.
    /// `now` is injected for deterministic testing — callers in production
    /// should pass `Utc::now()`.
    pub async fn tick_at(&self, now: DateTime<Utc>) -> TickReport {
        let mut report = TickReport::default();
        let waiting = match self.manager.list_by_status(FlowStatus::Waiting).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "wait engine: failed to list waiting flows");
                report.errors += 1;
                return report;
            }
        };
        report.scanned = waiting.len();

        for flow in waiting {
            match self.evaluate(&flow, now).await {
                Outcome::Resume => match self.manager.resume(flow.id, None).await {
                    Ok(_) => report.resumed += 1,
                    Err(FlowError::CancelPending { .. }) => {
                        // Flow was cancelled between list and resume.
                        match self.manager.cancel(flow.id).await {
                            Ok(_) => report.cancelled += 1,
                            Err(e) => {
                                tracing::warn!(flow_id = %flow.id, error = %e, "wait engine: cancel after CancelPending failed");
                                report.errors += 1;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(flow_id = %flow.id, error = %e, "wait engine: resume failed");
                        report.errors += 1;
                    }
                },
                Outcome::Cancel => match self.manager.cancel(flow.id).await {
                    Ok(_) => report.cancelled += 1,
                    Err(e) => {
                        tracing::warn!(flow_id = %flow.id, error = %e, "wait engine: cancel failed");
                        report.errors += 1;
                    }
                },
                Outcome::Wait => {
                    report.still_waiting += 1;
                }
                Outcome::Skip(reason) => {
                    tracing::debug!(flow_id = %flow.id, reason, "wait engine: skipping flow");
                }
            }
        }
        report
    }

    pub async fn tick(&self) -> TickReport {
        self.tick_at(Utc::now()).await
    }

    /// Host-driven resume for `ExternalEvent` waits. Invoked when (e.g.) a
    /// NATS subscriber receives a message and resolves it to a flow.
    /// Returns:
    /// - `Ok(Some(flow))` when the flow matched and was resumed
    /// - `Ok(None)` when no flow matched (no-op for caller)
    /// - `Err(_)` for store/manager errors that the caller should log
    pub async fn try_resume_external(
        &self,
        flow_id: Uuid,
        topic: &str,
        correlation_id: &str,
        payload: Option<Value>,
    ) -> Result<Option<Flow>, FlowError> {
        let Some(flow) = self.manager.get(flow_id).await? else {
            return Ok(None);
        };
        if flow.status != FlowStatus::Waiting {
            return Ok(None);
        }
        let cond = match flow.wait_json.as_ref().and_then(WaitCondition::from_value) {
            Some(c) => c,
            None => return Ok(None),
        };
        let matches = matches!(
            &cond,
            WaitCondition::ExternalEvent { topic: t, correlation_id: c }
                if t == topic && c == correlation_id
        );
        if !matches {
            return Ok(None);
        }
        let patch = payload.map(|p| json!({ "resume_event": p }));
        self.manager.resume(flow.id, patch).await.map(Some)
    }

    /// Long-running tick loop. Stops cleanly when `shutdown.cancelled()`
    /// fires. Logs each tick at debug level.
    pub async fn run(&self, interval: Duration, shutdown: tokio_util::sync::CancellationToken) {
        let mut interval_timer = tokio::time::interval(interval);
        interval_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::info!("wait engine: shutdown requested");
                    return;
                }
                _ = interval_timer.tick() => {
                    let report = self.tick().await;
                    if report.scanned > 0 {
                        tracing::debug!(
                            scanned = report.scanned,
                            resumed = report.resumed,
                            cancelled = report.cancelled,
                            still_waiting = report.still_waiting,
                            errors = report.errors,
                            "wait engine tick"
                        );
                    }
                }
            }
        }
    }

    async fn evaluate(&self, flow: &Flow, now: DateTime<Utc>) -> Outcome {
        // Sticky cancel takes priority over wait condition.
        if flow.cancel_requested {
            return Outcome::Cancel;
        }
        let Some(wait_value) = flow.wait_json.as_ref() else {
            return Outcome::Skip("missing wait_json");
        };
        let Some(cond) = WaitCondition::from_value(wait_value) else {
            return Outcome::Skip("unparseable wait_json");
        };
        match cond {
            WaitCondition::Timer { at } => {
                if now >= at {
                    Outcome::Resume
                } else {
                    Outcome::Wait
                }
            }
            // External and Manual conditions are not advanced by ticks —
            // they need an explicit signal (`try_resume_external` or `manager.resume`).
            WaitCondition::ExternalEvent { .. } | WaitCondition::Manual => Outcome::Wait,
        }
    }
}

enum Outcome {
    Resume,
    Cancel,
    Wait,
    Skip(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::CreateManagedInput;
    use crate::store::SqliteFlowStore;
    use chrono::Duration as ChronoDuration;

    async fn engine() -> WaitEngine {
        let store = Arc::new(SqliteFlowStore::open(":memory:").await.unwrap());
        WaitEngine::new(FlowManager::new(store))
    }

    fn input() -> CreateManagedInput {
        CreateManagedInput {
            controller_id: "test".into(),
            goal: "test".into(),
            owner_session_key: "owner".into(),
            requester_origin: "user".into(),
            current_step: "init".into(),
            state_json: json!({}),
        }
    }

    async fn put_into_waiting(eng: &WaitEngine, cond: WaitCondition) -> Flow {
        let m = eng.manager();
        let f = m.create_managed(input()).await.unwrap();
        let f = m.start_running(f.id).await.unwrap();
        m.set_waiting(f.id, cond.into_value()).await.unwrap()
    }

    #[tokio::test]
    async fn timer_fires_when_now_past_deadline() {
        let eng = engine().await;
        let past = Utc::now() - ChronoDuration::seconds(60);
        let f = put_into_waiting(&eng, WaitCondition::Timer { at: past }).await;

        let report = eng.tick().await;
        assert_eq!(report.scanned, 1);
        assert_eq!(report.resumed, 1);

        let after = eng.manager().get(f.id).await.unwrap().unwrap();
        assert_eq!(after.status, FlowStatus::Running);
        assert!(after.wait_json.is_none());
    }

    #[tokio::test]
    async fn timer_does_not_fire_before_deadline() {
        let eng = engine().await;
        let future = Utc::now() + ChronoDuration::seconds(60);
        let f = put_into_waiting(&eng, WaitCondition::Timer { at: future }).await;

        let report = eng.tick().await;
        assert_eq!(report.scanned, 1);
        assert_eq!(report.resumed, 0);
        assert_eq!(report.still_waiting, 1);

        let after = eng.manager().get(f.id).await.unwrap().unwrap();
        assert_eq!(after.status, FlowStatus::Waiting);
    }

    #[tokio::test]
    async fn external_event_matches_resumes() {
        let eng = engine().await;
        let f = put_into_waiting(
            &eng,
            WaitCondition::ExternalEvent {
                topic: "agent.delegate.reply".into(),
                correlation_id: "corr-42".into(),
            },
        )
        .await;

        // Tick alone does not resume external waits.
        let report = eng.tick().await;
        assert_eq!(report.resumed, 0);
        assert_eq!(report.still_waiting, 1);

        let resumed = eng
            .try_resume_external(
                f.id,
                "agent.delegate.reply",
                "corr-42",
                Some(json!({"answer": 42})),
            )
            .await
            .unwrap()
            .expect("resumed");
        assert_eq!(resumed.status, FlowStatus::Running);
        assert!(resumed.wait_json.is_none());
        assert_eq!(resumed.state_json["resume_event"]["answer"], 42);
    }

    #[tokio::test]
    async fn external_event_with_wrong_topic_or_id_is_noop() {
        let eng = engine().await;
        let f = put_into_waiting(
            &eng,
            WaitCondition::ExternalEvent {
                topic: "topic-A".into(),
                correlation_id: "id-1".into(),
            },
        )
        .await;

        // Wrong topic.
        let r1 = eng
            .try_resume_external(f.id, "topic-B", "id-1", None)
            .await
            .unwrap();
        assert!(r1.is_none());

        // Right topic, wrong id.
        let r2 = eng
            .try_resume_external(f.id, "topic-A", "id-99", None)
            .await
            .unwrap();
        assert!(r2.is_none());

        let after = eng.manager().get(f.id).await.unwrap().unwrap();
        assert_eq!(after.status, FlowStatus::Waiting);
    }

    #[tokio::test]
    async fn manual_wait_ignored_by_tick() {
        let eng = engine().await;
        let f = put_into_waiting(&eng, WaitCondition::Manual).await;
        let report = eng.tick().await;
        assert_eq!(report.scanned, 1);
        assert_eq!(report.resumed, 0);
        assert_eq!(report.still_waiting, 1);
        let after = eng.manager().get(f.id).await.unwrap().unwrap();
        assert_eq!(after.status, FlowStatus::Waiting);
    }

    #[tokio::test]
    async fn cancel_requested_waiting_flips_to_cancelled_on_tick() {
        let eng = engine().await;
        let future = Utc::now() + ChronoDuration::seconds(60);
        let f = put_into_waiting(&eng, WaitCondition::Timer { at: future }).await;
        eng.manager().request_cancel(f.id).await.unwrap();

        let report = eng.tick().await;
        assert_eq!(report.cancelled, 1);
        assert_eq!(report.resumed, 0);

        let after = eng.manager().get(f.id).await.unwrap().unwrap();
        assert_eq!(after.status, FlowStatus::Cancelled);
    }

    #[tokio::test]
    async fn run_loop_can_be_shut_down() {
        let eng = engine().await;
        let token = tokio_util::sync::CancellationToken::new();
        let token_clone = token.clone();
        let eng_clone = eng.clone();
        let handle = tokio::spawn(async move {
            eng_clone.run(Duration::from_millis(20), token_clone).await;
        });
        // Let it tick a few times, then cancel.
        tokio::time::sleep(Duration::from_millis(60)).await;
        token.cancel();
        // Should exit quickly.
        let r = tokio::time::timeout(Duration::from_millis(200), handle).await;
        assert!(r.is_ok(), "engine did not shut down promptly");
    }

    #[tokio::test]
    async fn try_resume_external_on_unknown_flow_is_noop() {
        let eng = engine().await;
        let r = eng
            .try_resume_external(Uuid::new_v4(), "t", "c", None)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn try_resume_external_on_running_flow_is_noop() {
        let eng = engine().await;
        let m = eng.manager();
        let f = m.create_managed(input()).await.unwrap();
        let f = m.start_running(f.id).await.unwrap();
        let r = eng
            .try_resume_external(f.id, "t", "c", None)
            .await
            .unwrap();
        assert!(r.is_none(), "should ignore non-waiting flows");
    }

    #[test]
    fn wait_condition_round_trip() {
        let original = WaitCondition::Timer {
            at: Utc::now(),
        };
        let v = original.clone().into_value();
        let parsed = WaitCondition::from_value(&v).expect("round trip");
        match parsed {
            WaitCondition::Timer { .. } => {}
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
