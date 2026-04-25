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
    let id_c = c.goal_id;

    assert_eq!(reg.admit(a, true).await.unwrap(), AdmitOutcome::Admitted);
    assert_eq!(reg.admit(b, true).await.unwrap(), AdmitOutcome::Admitted);
    assert_eq!(
        reg.admit(c, true).await.unwrap(),
        AdmitOutcome::Queued { position: 1 }
    );

    assert_eq!(reg.count_running(), 2);

    // Release the first, the queue should hand back C.
    let next = reg
        .release(id_a, AgentRunStatus::Done)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next, id_c);

    // Promote C — now there are 2 running again (B + C).
    assert!(reg.promote_queued(id_c).await.unwrap());
    assert_eq!(reg.count_running(), 2);
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
