#![cfg(feature = "sqlite")]

//! File-backed persistence — the binding survives a close/reopen.

use nexo_driver_claude::{SessionBinding, SessionBindingStore, SqliteBindingStore};
use nexo_driver_types::GoalId;

#[tokio::test]
async fn file_backed_persists_across_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("driver.db");
    let path_str = path.to_str().unwrap();

    let g = GoalId::new();
    {
        let store = SqliteBindingStore::open(path_str).await.unwrap();
        store
            .upsert(SessionBinding::new(g, "S1", Some("m".into()), None))
            .await
            .unwrap();
    } // store dropped → pool closed.

    let store = SqliteBindingStore::open(path_str).await.unwrap();
    let got = store.get(g).await.unwrap().unwrap();
    assert_eq!(got.session_id, "S1");
}
