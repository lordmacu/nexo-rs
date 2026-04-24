use std::sync::Arc;
use std::time::Duration;

use agent_core::session::{Interaction, Role, SessionManager};
use uuid::Uuid;

fn mgr_with_ttl(ttl: Duration) -> SessionManager {
    SessionManager::new(ttl, 50)
}

#[tokio::test]
async fn create_and_get() {
    let mgr = mgr_with_ttl(Duration::from_secs(60));
    let session = mgr.create("agent-kate");

    let fetched = mgr.get(session.id).expect("session should exist");
    assert_eq!(fetched.id, session.id);
    assert_eq!(fetched.agent_id, "agent-kate");
}

#[tokio::test]
async fn get_updates_last_access() {
    let mgr = mgr_with_ttl(Duration::from_secs(60));
    let session = mgr.create("agent-kate");
    let original_access = session.last_access;

    tokio::time::sleep(Duration::from_millis(5)).await;

    let fetched = mgr.get(session.id).unwrap();
    assert!(fetched.last_access > original_access);
}

#[tokio::test]
async fn get_nonexistent_returns_none() {
    let mgr = mgr_with_ttl(Duration::from_secs(60));
    assert!(mgr.get(Uuid::new_v4()).is_none());
}

#[tokio::test]
async fn push_trims_history() {
    let mgr = SessionManager::new(Duration::from_secs(60), 3);
    let mut session = mgr.create("agent");

    for i in 0..5 {
        session.push(Interaction::new(Role::User, format!("msg {i}")));
    }
    mgr.update(session.clone());

    let fetched = mgr.get(session.id).unwrap();
    assert_eq!(fetched.history.len(), 3);
    assert_eq!(fetched.history[0].content, "msg 2");
    assert_eq!(fetched.history[2].content, "msg 4");
}

#[tokio::test]
async fn ttl_expiry() {
    let mgr = mgr_with_ttl(Duration::from_millis(100));
    let session = mgr.create("agent");

    assert!(mgr.get(session.id).is_some());
    assert_eq!(mgr.active_count(), 1);

    // Wait longer than TTL + one sweep interval (ttl/4 = 25ms, wait 300ms to be safe)
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(mgr.get(session.id).is_none());
    assert_eq!(mgr.active_count(), 0);
}

#[tokio::test]
async fn get_or_create_creates_when_missing() {
    let mgr = mgr_with_ttl(Duration::from_secs(60));
    let id = Uuid::new_v4();

    let session = mgr.get_or_create(id, "agent");
    assert_eq!(session.id, id);
    assert_eq!(session.agent_id, "agent");
    assert_eq!(mgr.active_count(), 1);
}

#[tokio::test]
async fn get_or_create_returns_existing() {
    let mgr = mgr_with_ttl(Duration::from_secs(60));
    let original = mgr.create("agent");

    let fetched = mgr.get_or_create(original.id, "agent");
    assert_eq!(fetched.id, original.id);
    // Should not have created a duplicate
    assert_eq!(mgr.active_count(), 1);
}

#[tokio::test]
async fn update_returns_false_if_deleted() {
    let mgr = mgr_with_ttl(Duration::from_secs(60));
    let session = mgr.create("agent");

    mgr.delete(session.id);
    let updated = mgr.update(session);
    assert!(!updated);
}

#[tokio::test]
async fn active_count_reflects_state() {
    let mgr = mgr_with_ttl(Duration::from_secs(60));
    assert_eq!(mgr.active_count(), 0);

    let s1 = mgr.create("agent");
    let s2 = mgr.create("agent");
    assert_eq!(mgr.active_count(), 2);

    mgr.delete(s1.id);
    assert_eq!(mgr.active_count(), 1);

    mgr.delete(s2.id);
    assert_eq!(mgr.active_count(), 0);
}

// ─── on_expire callbacks ──────────────────────────────────────────────────

use std::sync::Mutex;

async fn wait_until<F>(timeout: Duration, mut pred: F)
where
    F: FnMut() -> bool,
{
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_expire_fires_from_explicit_delete() {
    let mgr = SessionManager::new(Duration::from_secs(60), 16);
    let captured: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let c = captured.clone();
        mgr.on_expire(move |sid| c.lock().unwrap().push(sid));
    }
    let s = mgr.create("agent");
    mgr.delete(s.id);

    wait_until(Duration::from_millis(500), || {
        !captured.lock().unwrap().is_empty()
    })
    .await;
    let seen = captured.lock().unwrap().clone();
    assert_eq!(seen, vec![s.id]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_expire_fires_from_sweeper() {
    let mgr = SessionManager::new(Duration::from_millis(100), 16);
    let captured: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let c = captured.clone();
        mgr.on_expire(move |sid| c.lock().unwrap().push(sid));
    }
    let s = mgr.create("agent");
    // Wait past TTL + one sweep.
    tokio::time::sleep(Duration::from_millis(400)).await;
    wait_until(Duration::from_millis(500), || {
        !captured.lock().unwrap().is_empty()
    })
    .await;
    let seen = captured.lock().unwrap().clone();
    assert!(seen.contains(&s.id), "captured: {seen:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_expire_multiple_callbacks_all_fire() {
    let mgr = SessionManager::new(Duration::from_secs(60), 16);
    let a: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));
    let b: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let a = a.clone();
        mgr.on_expire(move |sid| a.lock().unwrap().push(sid));
        let b = b.clone();
        mgr.on_expire(move |sid| b.lock().unwrap().push(sid));
    }
    let s = mgr.create("agent");
    mgr.delete(s.id);
    wait_until(Duration::from_millis(500), || {
        !a.lock().unwrap().is_empty() && !b.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(a.lock().unwrap().clone(), vec![s.id]);
    assert_eq!(b.lock().unwrap().clone(), vec![s.id]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_missing_session_does_not_fire() {
    let mgr = SessionManager::new(Duration::from_secs(60), 16);
    let captured: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let c = captured.clone();
        mgr.on_expire(move |sid| c.lock().unwrap().push(sid));
    }
    assert!(!mgr.delete(Uuid::new_v4()));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(captured.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cap_evicts_oldest_idle() {
    let mgr = SessionManager::with_cap(Duration::from_secs(60), 16, 3);
    let first = mgr.create("agent");
    tokio::time::sleep(Duration::from_millis(5)).await;
    let _second = mgr.create("agent");
    tokio::time::sleep(Duration::from_millis(5)).await;
    let _third = mgr.create("agent");
    assert_eq!(mgr.active_count(), 3);

    // Fourth create at cap evicts `first` (oldest last_access).
    let _fourth = mgr.create("agent");
    assert_eq!(mgr.active_count(), 3);
    assert!(mgr.get(first.id).is_none(), "oldest session was evicted");
}

#[tokio::test]
async fn cap_zero_is_unbounded() {
    let mgr = SessionManager::with_cap(Duration::from_secs(60), 16, 0);
    for _ in 0..10 {
        mgr.create("agent");
    }
    assert_eq!(mgr.active_count(), 10);
}
