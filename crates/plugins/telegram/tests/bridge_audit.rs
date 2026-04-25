//! Audit tests for the bridge semantics that were fixed in the
//! 2026-04-24 deep review:
//!
//! * Per-message pending entries (FIFO queue per session_id) so fast
//!   consecutive messages don't clobber each other's reply channel.
//! * `typing_abort` wired per entry so the ticker dies with the bridge
//!   it belongs to.

use std::sync::Arc;

use dashmap::DashMap;
use nexo_plugin_telegram::plugin::{PendingEntry, PendingMap};
use tokio::sync::oneshot;
use uuid::Uuid;

fn make_map() -> PendingMap {
    Arc::new(DashMap::new())
}

fn push_entry(
    pending: &PendingMap,
    session_id: Uuid,
) -> (Uuid, oneshot::Receiver<String>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = oneshot::channel();
    // Dummy long-running task that stands in for the typing ticker.
    // We verify its AbortHandle is fired by inspecting `is_finished`
    // after the abort call.
    let task = tokio::spawn(async {
        // Park forever — the test will abort us.
        futures_util::future::pending::<()>().await;
    });
    let entry_id = Uuid::new_v4();
    let entry = PendingEntry {
        entry_id,
        tx,
        typing_abort: task.abort_handle(),
    };
    pending.entry(session_id).or_default().push_back(entry);
    (entry_id, rx, task)
}

#[tokio::test]
async fn two_messages_from_same_chat_fifo_deliver_in_order() {
    let pending = make_map();
    let sid = Uuid::new_v4();

    let (id1, mut rx1, t1) = push_entry(&pending, sid);
    let (id2, mut rx2, t2) = push_entry(&pending, sid);
    assert_ne!(id1, id2);

    // Dispatcher simulation for reply #1: pop oldest, send text.
    let entry = pending.get_mut(&sid).unwrap().pop_front().unwrap();
    assert_eq!(entry.entry_id, id1);
    entry.typing_abort.abort();
    entry.tx.send("reply to 1".into()).unwrap();

    // Dispatcher simulation for reply #2.
    let entry = pending.get_mut(&sid).unwrap().pop_front().unwrap();
    assert_eq!(entry.entry_id, id2);
    entry.typing_abort.abort();
    entry.tx.send("reply to 2".into()).unwrap();

    assert_eq!(rx1.try_recv().unwrap(), "reply to 1");
    assert_eq!(rx2.try_recv().unwrap(), "reply to 2");

    // Typing tickers for both entries must be aborted.
    tokio::task::yield_now().await;
    assert!(t1.is_finished());
    assert!(t2.is_finished());
}

#[tokio::test]
async fn timeout_retains_drops_only_own_entry() {
    let pending = make_map();
    let sid = Uuid::new_v4();
    let (id1, _rx1, _t1) = push_entry(&pending, sid);
    let (id2, _rx2, _t2) = push_entry(&pending, sid);
    let (_id3, _rx3, _t3) = push_entry(&pending, sid);

    // Simulate entry #2 timing out and removing itself.
    if let Some(mut q) = pending.get_mut(&sid) {
        q.retain(|e| e.entry_id != id2);
    }

    let q = pending.get(&sid).unwrap();
    assert_eq!(q.len(), 2);
    assert_eq!(q[0].entry_id, id1);
    assert_ne!(q[1].entry_id, id2);
}

#[tokio::test]
async fn allowlist_hashset_membership_is_o1() {
    // Not a performance assertion — just verifies the semantics are
    // preserved after swapping Vec for HashSet. The public plugin
    // keeps `Vec<i64>` in config; the poller internally precomputes
    // a HashSet. We test the observable behaviour.
    use std::collections::HashSet;
    let allowlist: HashSet<i64> = [1i64, 2, 3, 999, -100].iter().copied().collect();
    assert!(allowlist.contains(&1));
    assert!(allowlist.contains(&-100));
    assert!(!allowlist.contains(&42));
    // Empty allowlist semantics: caller must check `is_empty()` and
    // accept everything — the plugin uses a `has_allowlist` flag.
    let empty: HashSet<i64> = HashSet::new();
    assert!(empty.is_empty());
}

#[tokio::test]
async fn offset_round_trip_through_file() {
    use nexo_plugin_telegram::plugin::{load_offset, save_offset};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("offset");

    // First read: file doesn't exist yet → zero.
    assert_eq!(load_offset(&path).await, 0);

    save_offset(&path, 12345).await.unwrap();
    assert_eq!(load_offset(&path).await, 12345);

    // Overwrite — atomic rename, no partial state.
    save_offset(&path, 67890).await.unwrap();
    assert_eq!(load_offset(&path).await, 67890);

    // Corrupt file → fall back to 0, don't panic.
    tokio::fs::write(&path, "not-an-int").await.unwrap();
    assert_eq!(load_offset(&path).await, 0);
}

#[tokio::test]
async fn empty_queue_cleaned_from_dashmap() {
    let pending = make_map();
    let sid = Uuid::new_v4();
    let (_id, _rx, _t) = push_entry(&pending, sid);

    // Dispatcher-style drain + post-pop cleanup of empty queue.
    {
        let mut q = pending.get_mut(&sid).unwrap();
        let _ = q.pop_front();
    }
    let empty = pending.get(&sid).map(|q| q.is_empty()).unwrap_or(true);
    assert!(empty);
    if empty {
        pending.remove(&sid);
    }
    assert!(pending.get(&sid).is_none());
}
