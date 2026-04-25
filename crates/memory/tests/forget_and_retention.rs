//! Regression tests for the 2026-04-24 memory audit:
//!
//! * `forget()` now purges ALL derived rows (FTS, vec, recall_events,
//!   memory_promotions) — previously it left `vec_memories` orphaned so
//!   vector recall returned ids whose JOIN was null.
//! * `prune_recall_events()` trims the ever-growing log.

use nexo_memory::LongTermMemory;
use tempfile::tempdir;

#[tokio::test]
async fn forget_removes_all_derived_rows() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("mem.db");
    let mem = LongTermMemory::open(path.to_str().unwrap()).await.unwrap();

    let id = mem
        .remember("kate", "user likes tea", &["pref"])
        .await
        .unwrap();

    // Seed a recall event + promotion so forget() has something to purge.
    mem.record_recall_event("kate", id, "tea", 0.9)
        .await
        .unwrap();
    mem.mark_promoted("kate", id, 0.9, "deep").await.unwrap();

    assert_eq!(mem.count_memories("kate").await.unwrap(), 1);
    assert!(mem.is_promoted(id).await.unwrap());
    assert!(mem.count_recall_events_since("kate", 0).await.unwrap() >= 1);

    // forget — all three child tables should lose the row.
    assert!(mem.forget(id).await.unwrap());
    assert_eq!(mem.count_memories("kate").await.unwrap(), 0);
    assert!(!mem.is_promoted(id).await.unwrap());
    assert_eq!(mem.count_recall_events_since("kate", 0).await.unwrap(), 0);

    // Second forget() is a noop.
    assert!(!mem.forget(id).await.unwrap());
}

#[tokio::test]
async fn prune_recall_events_trims_old_rows() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("mem.db");
    let mem = LongTermMemory::open(path.to_str().unwrap()).await.unwrap();

    let id = mem.remember("kate", "content", &[]).await.unwrap();
    // Record a few fresh events (ts = now, which is > cutoff).
    for _ in 0..5 {
        mem.record_recall_event("kate", id, "q", 0.5).await.unwrap();
    }
    assert_eq!(mem.count_recall_events_since("kate", 0).await.unwrap(), 5);

    // retention_days=0 → cutoff = now, everything with ts < now dies.
    // (Race-safe because sqlite writes ts_ms at insert; pruning uses a
    // fresh Utc::now() so `ts_ms < now` catches all pre-existing rows.)
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let removed = mem.prune_recall_events(0).await.unwrap();
    assert_eq!(removed, 5, "expected 5 rows deleted, got {removed}");
    assert_eq!(mem.count_recall_events_since("kate", 0).await.unwrap(), 0);
}

#[tokio::test]
async fn wal_checkpoint_noops_cleanly() {
    // Even on an in-memory DB without WAL, the PRAGMA should not fail.
    let dir = tempdir().unwrap();
    let path = dir.path().join("mem.db");
    let mem = LongTermMemory::open(path.to_str().unwrap()).await.unwrap();
    mem.remember("a", "b", &[]).await.unwrap();
    mem.wal_checkpoint().await.unwrap();
    mem.wal_checkpoint().await.unwrap(); // idempotent
}
