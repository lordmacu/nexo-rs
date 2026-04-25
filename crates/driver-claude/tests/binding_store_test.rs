use nexo_driver_claude::{MemoryBindingStore, SessionBinding, SessionBindingStore};
use nexo_driver_types::GoalId;

fn binding(goal: GoalId, sid: &str) -> SessionBinding {
    SessionBinding::new(goal, sid, Some("claude-sonnet-4-6".into()), None)
}

#[tokio::test]
async fn get_missing_returns_none() {
    let s = MemoryBindingStore::new();
    assert!(s.get(GoalId::new()).await.unwrap().is_none());
}

#[tokio::test]
async fn upsert_then_get_returns_binding() {
    let s = MemoryBindingStore::new();
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    let got = s.get(g).await.unwrap().unwrap();
    assert_eq!(got.session_id, "S1");
}

#[tokio::test]
async fn clear_removes_binding() {
    let s = MemoryBindingStore::new();
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    s.clear(g).await.unwrap();
    assert!(s.get(g).await.unwrap().is_none());
}

#[tokio::test]
async fn double_upsert_preserves_created_at_and_updates_updated_at() {
    let s = MemoryBindingStore::new();
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    let first = s.get(g).await.unwrap().unwrap();

    // Force a measurable gap so timestamps differ.
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;

    s.upsert(binding(g, "S2")).await.unwrap();
    let second = s.get(g).await.unwrap().unwrap();

    assert_eq!(second.session_id, "S2");
    assert_eq!(second.created_at, first.created_at, "created_at preserved");
    assert!(second.updated_at >= first.updated_at, "updated_at advances");
}
