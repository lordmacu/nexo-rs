#![cfg(feature = "sqlite")]

use std::time::Duration;

use chrono::Utc;
use nexo_driver_claude::{SessionBinding, SessionBindingStore, SqliteBindingStore};
use nexo_driver_types::GoalId;

fn binding(g: GoalId, sid: &str) -> SessionBinding {
    SessionBinding::new(g, sid, Some("claude-sonnet-4-6".into()), None)
}

async fn open() -> SqliteBindingStore {
    SqliteBindingStore::open_memory().await.unwrap()
}

#[tokio::test]
async fn get_missing_returns_none() {
    let s = open().await;
    assert!(s.get(GoalId::new()).await.unwrap().is_none());
}

#[tokio::test]
async fn upsert_then_get_returns_binding() {
    let s = open().await;
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    let got = s.get(g).await.unwrap().unwrap();
    assert_eq!(got.session_id, "S1");
    assert_eq!(got.model.as_deref(), Some("claude-sonnet-4-6"));
}

#[tokio::test]
async fn clear_removes_row() {
    let s = open().await;
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    s.clear(g).await.unwrap();
    assert!(s.get(g).await.unwrap().is_none());
}

#[tokio::test]
async fn upsert_preserves_created_at_across_calls() {
    let s = open().await;
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    let first = s.get(g).await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await; // unix-second granularity
    s.upsert(binding(g, "S2")).await.unwrap();
    let second = s.get(g).await.unwrap().unwrap();
    assert_eq!(second.session_id, "S2");
    assert_eq!(second.created_at, first.created_at, "created_at preserved");
    assert!(second.updated_at >= first.updated_at, "updated_at advances");
}

#[tokio::test]
async fn idle_ttl_filters_stale_bindings() {
    let s = SqliteBindingStore::open_memory()
        .await
        .unwrap()
        .with_idle_ttl(Duration::from_secs(1));
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    assert!(s.get(g).await.unwrap().is_some(), "fresh binding visible");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(s.get(g).await.unwrap().is_none(), "stale binding filtered");
}

#[tokio::test]
async fn idle_ttl_zero_treated_as_no_filter() {
    let s = SqliteBindingStore::open_memory()
        .await
        .unwrap()
        .with_idle_ttl(Duration::ZERO);
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    // Sleep then read — should still return the binding because ZERO
    // means "no filter".
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(s.get(g).await.unwrap().is_some());
}

#[tokio::test]
async fn max_age_filters_even_when_recently_touched() {
    let s = SqliteBindingStore::open_memory()
        .await
        .unwrap()
        .with_max_age(Duration::from_secs(1));
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    s.touch(g).await.unwrap(); // last_active_at refreshed…
                               // …but max_age is keyed off `created_at`, so still filtered.
    assert!(s.get(g).await.unwrap().is_none());
}

#[tokio::test]
async fn mark_invalid_keeps_row_but_get_returns_none() {
    let s = open().await;
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    s.mark_invalid(g).await.unwrap();

    // get() filters out invalidated rows.
    assert!(s.get(g).await.unwrap().is_none());

    // …but the row is still in the table for forensics.
    let row: (i64,) = sqlx::query_as(
        "SELECT last_session_invalid FROM claude_session_bindings WHERE goal_id = ?",
    )
    .bind(g.0.to_string())
    .fetch_one(s.pool_for_test())
    .await
    .unwrap();
    assert_eq!(row.0, 1);
}

#[tokio::test]
async fn mark_invalid_then_upsert_resets_flag() {
    let s = open().await;
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    s.mark_invalid(g).await.unwrap();
    s.upsert(binding(g, "S2")).await.unwrap();

    let got = s.get(g).await.unwrap().unwrap();
    assert_eq!(got.session_id, "S2");

    let row: (i64,) = sqlx::query_as(
        "SELECT last_session_invalid FROM claude_session_bindings WHERE goal_id = ?",
    )
    .bind(g.0.to_string())
    .fetch_one(s.pool_for_test())
    .await
    .unwrap();
    assert_eq!(row.0, 0, "upsert resets the invalid flag");
}

#[tokio::test]
async fn touch_advances_last_active_at_but_not_updated_at() {
    let s = open().await;
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    let before = s.get(g).await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    s.touch(g).await.unwrap();
    let after = s.get(g).await.unwrap().unwrap();
    assert!(
        after.last_active_at > before.last_active_at,
        "last_active_at advanced: {:?} -> {:?}",
        before.last_active_at,
        after.last_active_at,
    );
    assert_eq!(after.updated_at, before.updated_at, "updated_at untouched");
}

#[tokio::test]
async fn touch_skips_invalidated_rows() {
    let s = open().await;
    let g = GoalId::new();
    s.upsert(binding(g, "S1")).await.unwrap();
    s.mark_invalid(g).await.unwrap();
    let before: i64 =
        sqlx::query_scalar("SELECT last_active_at FROM claude_session_bindings WHERE goal_id = ?")
            .bind(g.0.to_string())
            .fetch_one(s.pool_for_test())
            .await
            .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    s.touch(g).await.unwrap();
    let after: i64 =
        sqlx::query_scalar("SELECT last_active_at FROM claude_session_bindings WHERE goal_id = ?")
            .bind(g.0.to_string())
            .fetch_one(s.pool_for_test())
            .await
            .unwrap();
    assert_eq!(before, after, "touch must not refresh invalidated rows");
}

#[tokio::test]
async fn purge_older_than_removes_only_old_rows() {
    let s = open().await;
    let recent = GoalId::new();
    let ancient = GoalId::new();
    s.upsert(binding(recent, "R")).await.unwrap();
    s.upsert(binding(ancient, "A")).await.unwrap();
    // Backdate the ancient row's last_active_at by hand.
    sqlx::query("UPDATE claude_session_bindings SET last_active_at = 0 WHERE goal_id = ?")
        .bind(ancient.0.to_string())
        .execute(s.pool_for_test())
        .await
        .unwrap();

    let cutoff = Utc::now() - chrono::Duration::seconds(60);
    let n = s.purge_older_than(cutoff).await.unwrap();
    assert_eq!(n, 1);
    assert!(s.get(recent).await.unwrap().is_some());
    assert!(s.get(ancient).await.unwrap().is_none());
}

#[tokio::test]
async fn list_active_excludes_invalid_and_orders_desc() {
    let s = open().await;
    let g1 = GoalId::new();
    let g2 = GoalId::new();
    let g3 = GoalId::new();
    s.upsert(binding(g1, "S1")).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    s.upsert(binding(g2, "S2")).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    s.upsert(binding(g3, "S3")).await.unwrap();
    s.mark_invalid(g2).await.unwrap();

    let active = s.list_active().await.unwrap();
    let ids: Vec<_> = active.iter().map(|b| b.goal_id).collect();
    // Newest first; invalidated row excluded.
    assert_eq!(ids, vec![g3, g1]);
}

#[tokio::test]
async fn concurrent_upserts_on_distinct_goals() {
    use std::sync::Arc;
    let s = Arc::new(open().await);
    let mut handles = Vec::new();
    for i in 0..16 {
        let s = s.clone();
        handles.push(tokio::spawn(async move {
            let g = GoalId::new();
            s.upsert(binding(g, &format!("S-{i}"))).await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let active = s.list_active().await.unwrap();
    assert_eq!(active.len(), 16);
}
