//! Phase 67.B.3 — `admit` enforces the global cap; beyond it,
//! goals are queued (FIFO) and `release` returns the next-up so
//! the caller can promote it.

use std::sync::Arc;

use chrono::Utc;
use nexo_agent_registry::{
    AdmitOutcome, AgentHandle, AgentRegistry, AgentRunStatus, AgentSnapshot,
    MemoryAgentRegistryStore,
};
use nexo_driver_types::GoalId;
use uuid::Uuid;

fn handle(phase: &str) -> AgentHandle {
    AgentHandle {
        goal_id: GoalId(Uuid::new_v4()),
        phase_id: phase.into(),
        status: AgentRunStatus::Running, // overwritten by admit()
        origin: None,
        dispatcher: None,
        started_at: Utc::now(),
        finished_at: None,
        snapshot: AgentSnapshot::default(),
    plan_mode: None,
    }
}

#[tokio::test]
async fn cap_2_third_admit_queues() {
    let store = Arc::new(MemoryAgentRegistryStore::default());
    let reg = AgentRegistry::new(store, 2);

    let a = handle("67.10");
    let b = handle("67.11");
    let c = handle("67.12");
    let id_a = a.goal_id;
    let id_b = b.goal_id;
    let id_c = c.goal_id;

    assert_eq!(reg.admit(a, true).await.unwrap(), AdmitOutcome::Admitted);
    assert_eq!(reg.admit(b, true).await.unwrap(), AdmitOutcome::Admitted);
    assert_eq!(
        reg.admit(c, true).await.unwrap(),
        AdmitOutcome::Queued { position: 1 }
    );

    assert_eq!(reg.count_running(), 2);

    // Release the first, the queue should hand back C (popped).
    let next = reg
        .release(id_a, AgentRunStatus::Done)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next, id_c);

    // B12 — release pops; a second release after another goal
    // ends MUST NOT return the same id again (queue is empty
    // now since C was popped above and there were only 3
    // entries originally).
    let next2 = reg.release(id_b, AgentRunStatus::Done).await.unwrap();
    assert!(next2.is_none(), "queue should be empty after pop");

    // promote_queued requires the id to still be in the queue.
    // Since release popped it, promote_queued returns false.
    assert!(!reg.promote_queued(id_c).await.unwrap());
}

#[tokio::test]
async fn admit_with_no_enqueue_rejects_when_cap_reached() {
    let store = Arc::new(MemoryAgentRegistryStore::default());
    let reg = AgentRegistry::new(store, 1);
    reg.admit(handle("67.10"), false).await.unwrap();
    let outcome = reg.admit(handle("67.11"), false).await.unwrap();
    assert_eq!(outcome, AdmitOutcome::Rejected);
    assert_eq!(reg.count_running(), 1);
}

#[tokio::test]
async fn release_with_non_terminal_status_errors() {
    let store = Arc::new(MemoryAgentRegistryStore::default());
    let reg = AgentRegistry::new(store, 5);
    let h = handle("67.10");
    let id = h.goal_id;
    reg.admit(h, true).await.unwrap();
    let err = reg.release(id, AgentRunStatus::Running).await.unwrap_err();
    assert!(matches!(
        err,
        nexo_agent_registry::RegistryError::InvalidTransition { .. }
    ));
}

#[tokio::test]
async fn concurrent_admits_do_not_overshoot_cap() {
    // PT-5 — 10 concurrent admits with cap=3 must produce exactly
    // 3 Admitted and 7 Queued. Without the admit_lock the sum of
    // running could exceed the cap by N.
    let store = Arc::new(MemoryAgentRegistryStore::default());
    let reg = Arc::new(AgentRegistry::new(store, 3));
    let mut tasks = Vec::new();
    for i in 0..10 {
        let r = reg.clone();
        let h = handle(&format!("99.{i}"));
        tasks.push(tokio::spawn(async move { r.admit(h, true).await.unwrap() }));
    }
    let mut admitted = 0;
    let mut queued = 0;
    for t in tasks {
        match t.await.unwrap() {
            AdmitOutcome::Admitted => admitted += 1,
            AdmitOutcome::Queued { .. } => queued += 1,
            AdmitOutcome::Rejected => panic!("rejected with enqueue=true"),
        }
    }
    assert_eq!(admitted, 3, "exactly cap goals must run");
    assert_eq!(queued, 7);
    assert_eq!(reg.count_running(), 3);
}

#[tokio::test]
async fn list_includes_running_and_terminal() {
    let store = Arc::new(MemoryAgentRegistryStore::default());
    let reg = AgentRegistry::new(store.clone(), 5);
    let h1 = handle("67.10");
    let h2 = handle("67.11");
    let id1 = h1.goal_id;
    reg.admit(h1, true).await.unwrap();
    reg.admit(h2, true).await.unwrap();
    reg.release(id1, AgentRunStatus::Done).await.unwrap();
    let rows = reg.list().await.unwrap();
    assert_eq!(rows.len(), 2);
}
