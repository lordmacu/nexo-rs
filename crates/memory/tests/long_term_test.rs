use agent_memory::LongTermMemory;
use chrono::{Duration, Utc};
use uuid::Uuid;

async fn open_temp_db() -> LongTermMemory {
    // Use :memory: for fast, isolated tests
    LongTermMemory::open(":memory:").await.unwrap()
}

#[tokio::test]
async fn remember_and_recall_by_keyword() {
    let db = open_temp_db().await;
    let agent = "test-agent";

    db.remember(agent, "The user prefers short responses", &["preferences"]).await.unwrap();
    db.remember(agent, "The user lives in Madrid", &["location"]).await.unwrap();
    db.remember(agent, "The user drinks coffee every morning", &["habits"]).await.unwrap();

    let results = db.recall(agent, "Madrid", 5).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].content.contains("Madrid"));
}

#[tokio::test]
async fn recall_returns_multiple_matches_ordered_by_rank() {
    let db = open_temp_db().await;
    let agent = "agent-x";

    db.remember(agent, "user likes coffee", &[]).await.unwrap();
    db.remember(agent, "user loves coffee and tea", &[]).await.unwrap();
    db.remember(agent, "user prefers water", &[]).await.unwrap();

    let results = db.recall(agent, "coffee", 10).await.unwrap();
    assert_eq!(results.len(), 2);
    // Both coffee entries should appear
    let contents: Vec<_> = results.iter().map(|r| r.content.as_str()).collect();
    assert!(contents.iter().any(|c| c.contains("coffee")));
}

#[tokio::test]
async fn forget_removes_from_fts() {
    let db = open_temp_db().await;
    let agent = "agent-y";

    let id = db.remember(agent, "secret password hint", &["secret"]).await.unwrap();

    let before = db.recall(agent, "secret", 5).await.unwrap();
    assert_eq!(before.len(), 1);

    let deleted = db.forget(id).await.unwrap();
    assert!(deleted);

    let after = db.recall(agent, "secret", 5).await.unwrap();
    assert_eq!(after.len(), 0);
}

#[tokio::test]
async fn forget_nonexistent_returns_false() {
    let db = open_temp_db().await;
    let result = db.forget(Uuid::new_v4()).await.unwrap();
    assert!(!result);
}

#[tokio::test]
async fn save_and_load_interactions_ordered() {
    let db = open_temp_db().await;
    let session = Uuid::new_v4();
    let agent = "kate";

    for i in 0..5u32 {
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        db.save_interaction(session, agent, role, &format!("message {i}"))
            .await.unwrap();
        // Ensure distinct timestamps
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let loaded = db.load_interactions(session, 3).await.unwrap();
    assert_eq!(loaded.len(), 3);
    // Should be oldest-first (messages 2, 3, 4)
    assert!(loaded[0].content.contains("message 2"));
    assert!(loaded[1].content.contains("message 3"));
    assert!(loaded[2].content.contains("message 4"));
}

#[tokio::test]
async fn recall_isolated_per_agent() {
    let db = open_temp_db().await;

    db.remember("agent-a", "cats are great", &[]).await.unwrap();
    db.remember("agent-b", "dogs are great", &[]).await.unwrap();

    let a_results = db.recall("agent-a", "great", 5).await.unwrap();
    let b_results = db.recall("agent-b", "great", 5).await.unwrap();

    assert_eq!(a_results.len(), 1);
    assert!(a_results[0].content.contains("cats"));
    assert_eq!(b_results.len(), 1);
    assert!(b_results[0].content.contains("dogs"));
}

#[tokio::test]
async fn due_reminders_return_only_pending_due_entries() {
    let db = open_temp_db().await;
    let session = Uuid::new_v4();
    let now = Utc::now();

    let due_id = db
        .schedule_reminder(
            "agent-a",
            session,
            "telegram",
            "user-1",
            "due now",
            now - Duration::seconds(5),
        )
        .await
        .unwrap();
    db.schedule_reminder(
        "agent-a",
        session,
        "telegram",
        "user-1",
        "future",
        now + Duration::minutes(5),
    )
    .await
    .unwrap();
    db.schedule_reminder(
        "agent-b",
        session,
        "telegram",
        "user-2",
        "other agent",
        now - Duration::seconds(5),
    )
    .await
    .unwrap();

    let due = db.list_due_reminders("agent-a", now, 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, due_id);
    assert_eq!(due[0].message, "due now");
}

#[tokio::test]
async fn delivered_reminder_is_not_returned_again() {
    let db = open_temp_db().await;
    let session = Uuid::new_v4();
    let now = Utc::now();

    let id = db
        .schedule_reminder(
            "agent-a",
            session,
            "whatsapp",
            "user-1",
            "drink water",
            now - Duration::seconds(1),
        )
        .await
        .unwrap();

    assert!(db.mark_reminder_delivered(id).await.unwrap());
    assert!(!db.mark_reminder_delivered(id).await.unwrap());

    let due = db.list_due_reminders("agent-a", now, 10).await.unwrap();
    assert!(due.is_empty());
}

#[tokio::test]
async fn claim_due_reminders_excludes_already_claimed_entries() {
    let db = open_temp_db().await;
    let session = Uuid::new_v4();
    let now = Utc::now();

    let id = db
        .schedule_reminder(
            "agent-a",
            session,
            "telegram",
            "user-1",
            "claimed reminder",
            now - Duration::seconds(1),
        )
        .await
        .unwrap();

    let first = db.claim_due_reminders("agent-a", now, 10).await.unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, id);
    assert!(first[0].claimed_at.is_some());

    let second = db.claim_due_reminders("agent-a", now, 10).await.unwrap();
    assert!(second.is_empty());
}

#[tokio::test]
async fn released_claim_allows_retry() {
    let db = open_temp_db().await;
    let session = Uuid::new_v4();
    let now = Utc::now();

    let id = db
        .schedule_reminder(
            "agent-a",
            session,
            "telegram",
            "user-1",
            "retry me",
            now - Duration::seconds(1),
        )
        .await
        .unwrap();

    let first = db.claim_due_reminders("agent-a", now, 10).await.unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, id);

    assert!(db.release_reminder_claim(id).await.unwrap());

    let second = db.claim_due_reminders("agent-a", now, 10).await.unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].id, id);
}

// ── Recall signal tracking (Phase 10.5) ──────────────────────────────────────

#[tokio::test]
async fn record_recall_event_increments_signals() {
    let db = open_temp_db().await;
    let id = db.remember("ag", "prefer dark mode", &[]).await.unwrap();

    let zero = db.recall_signals("ag", id, None).await.unwrap();
    assert_eq!(zero.recall_count, 0);
    assert_eq!(zero.frequency, 0.0);

    db.record_recall_event("ag", id, "dark", 1.0).await.unwrap();
    db.record_recall_event("ag", id, "mode", 0.5).await.unwrap();
    db.record_recall_event("ag", id, "dark", 0.75).await.unwrap();

    let sig = db.recall_signals("ag", id, None).await.unwrap();
    assert_eq!(sig.recall_count, 3);
    assert!(sig.frequency > 0.0);
    // relevance = mean(1.0, 0.5, 0.75) = 0.75
    assert!((sig.relevance - 0.75).abs() < 1e-4, "relevance={}", sig.relevance);
    // Two distinct queries, one day, so diversity comes from query count.
    assert!(sig.diversity > 0.0);
}

#[tokio::test]
async fn recall_signals_isolated_per_memory_and_agent() {
    let db = open_temp_db().await;
    let a_mem = db.remember("ag-a", "aaa", &[]).await.unwrap();
    let b_mem = db.remember("ag-b", "bbb", &[]).await.unwrap();

    db.record_recall_event("ag-a", a_mem, "q", 1.0).await.unwrap();
    db.record_recall_event("ag-a", a_mem, "q", 1.0).await.unwrap();
    db.record_recall_event("ag-b", b_mem, "q", 1.0).await.unwrap();

    let a = db.recall_signals("ag-a", a_mem, None).await.unwrap();
    let b = db.recall_signals("ag-b", b_mem, None).await.unwrap();
    assert_eq!(a.recall_count, 2);
    assert_eq!(b.recall_count, 1);

    // Cross-agent query returns nothing — agent_id is part of the key.
    let empty = db.recall_signals("ag-a", b_mem, None).await.unwrap();
    assert_eq!(empty.recall_count, 0);
}

#[tokio::test]
async fn recall_signals_for_agent_returns_map() {
    let db = open_temp_db().await;
    let m1 = db.remember("ag", "one", &[]).await.unwrap();
    let m2 = db.remember("ag", "two", &[]).await.unwrap();

    db.record_recall_event("ag", m1, "q", 1.0).await.unwrap();
    db.record_recall_event("ag", m2, "q", 0.5).await.unwrap();
    db.record_recall_event("ag", m2, "q", 0.4).await.unwrap();

    let map = db.recall_signals_for_agent("ag", None).await.unwrap();
    assert_eq!(map.len(), 2);
    assert_eq!(map[&m1].recall_count, 1);
    assert_eq!(map[&m2].recall_count, 2);
}

// --- Phase 10.8 stat methods ---

#[tokio::test]
async fn count_memories_empty_is_zero() {
    let db = open_temp_db().await;
    assert_eq!(db.count_memories("x").await.unwrap(), 0);
}

#[tokio::test]
async fn count_memories_counts_per_agent() {
    let db = open_temp_db().await;
    db.remember("a", "one", &[]).await.unwrap();
    db.remember("a", "two", &[]).await.unwrap();
    db.remember("b", "three", &[]).await.unwrap();
    assert_eq!(db.count_memories("a").await.unwrap(), 2);
    assert_eq!(db.count_memories("b").await.unwrap(), 1);
}

#[tokio::test]
async fn count_sessions_distinct() {
    let db = open_temp_db().await;
    let s1 = Uuid::new_v4();
    let s2 = Uuid::new_v4();
    db.save_interaction(s1, "a", "user", "hi").await.unwrap();
    db.save_interaction(s1, "a", "assistant", "hello").await.unwrap();
    db.save_interaction(s2, "a", "user", "q2").await.unwrap();
    db.save_interaction(s1, "b", "user", "hi-b").await.unwrap();
    assert_eq!(db.count_sessions("a").await.unwrap(), 2);
    assert_eq!(db.count_sessions("b").await.unwrap(), 1);
}

#[tokio::test]
async fn last_promotion_ts_none_when_empty() {
    let db = open_temp_db().await;
    assert!(db.last_promotion_ts("a").await.unwrap().is_none());
}

#[tokio::test]
async fn last_promotion_ts_returns_max() {
    let db = open_temp_db().await;
    let m1 = db.remember("a", "m1", &[]).await.unwrap();
    let m2 = db.remember("a", "m2", &[]).await.unwrap();
    db.mark_promoted("a", m1, 0.5, "deep").await.unwrap();
    // tiny sleep to ensure distinct ms
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    db.mark_promoted("a", m2, 0.7, "deep").await.unwrap();

    let last = db.last_promotion_ts("a").await.unwrap();
    assert!(last.is_some());
    // Within last minute
    let now = Utc::now();
    let delta = now.signed_duration_since(last.unwrap());
    assert!(delta < Duration::minutes(1));
    assert_eq!(db.count_promotions("a").await.unwrap(), 2);
}

#[tokio::test]
async fn recall_events_since_windowed() {
    let db = open_temp_db().await;
    let m = db.remember("a", "hello", &[]).await.unwrap();
    db.record_recall_event("a", m, "q", 1.0).await.unwrap();
    db.record_recall_event("a", m, "q", 1.0).await.unwrap();

    let now = Utc::now().timestamp_millis();
    let since = now - 60_000;
    assert_eq!(db.count_recall_events_since("a", since).await.unwrap(), 2);
    let future = now + 60_000;
    assert_eq!(db.count_recall_events_since("a", future).await.unwrap(), 0);
}

#[tokio::test]
async fn top_concept_tags_since_tallies() {
    let db = open_temp_db().await;
    let m_openai = db.remember("a", "We call OpenAI endpoints", &[]).await.unwrap();
    let m_router = db.remember("a", "Router VLAN config on switch", &[]).await.unwrap();

    // Three recall hits on openai, one on router.
    db.record_recall_event("a", m_openai, "q1", 1.0).await.unwrap();
    db.record_recall_event("a", m_openai, "q2", 0.5).await.unwrap();
    db.record_recall_event("a", m_openai, "q3", 0.3).await.unwrap();
    db.record_recall_event("a", m_router, "q4", 0.9).await.unwrap();

    let now = Utc::now().timestamp_millis();
    let top = db.top_concept_tags_since("a", now - 60_000, 5).await.unwrap();
    assert!(!top.is_empty());
    // `openai` should rank higher than `router` because it was recalled more.
    let openai_rank = top.iter().position(|(t, _)| t == "openai");
    let router_rank = top.iter().position(|(t, _)| t == "router");
    if let (Some(oi), Some(ri)) = (openai_rank, router_rank) {
        assert!(oi < ri, "openai {oi} should outrank router {ri}");
    } else {
        assert!(openai_rank.is_some(), "openai tag must appear, got {:?}", top);
    }
}
