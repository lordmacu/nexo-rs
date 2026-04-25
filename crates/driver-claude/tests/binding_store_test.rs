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
    // 67.2 — `upsert` must seed `last_active_at` so future idle-TTL
    // filters compare against a real timestamp, not epoch.
    assert!(got.last_active_at >= got.created_at);
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
    assert!(
        second.last_active_at >= first.last_active_at,
        "last_active_at advances on upsert"
    );
}

#[tokio::test]
async fn default_mark_invalid_on_memory_store_clears_via_default_impl() {
    // 67.2 — the trait default for `mark_invalid` delegates to
    // `clear`. `MemoryBindingStore` keeps that default; this test
    // pins it.
    let s = MemoryBindingStore::new();
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    s.mark_invalid(g).await.unwrap();
    assert!(s.get(g).await.unwrap().is_none());
}
