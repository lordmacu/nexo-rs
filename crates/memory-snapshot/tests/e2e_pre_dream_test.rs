//! End-to-end coverage of the dream → pre-snapshot → restore chain.
//!
//! The unit suites cover each layer in isolation:
//!
//! - `local_fs::snapshot::tests` — bundle production
//! - `local_fs::restore::tests` — bundle replay
//! - `local_fs::verify::tests` — integrity checks
//! - `dream_adapter::tests` — adapter wiring
//!
//! This test glues them together: drive
//! `PreDreamSnapshotHook::snapshot_before_dream` against a real
//! `LocalFsSnapshotter` over real on-disk SQLite + git memdir,
//! verify the produced bundle, and replay it through `restore`. If
//! `AutoDreamRunner` ever calls the hook with a real adapter, the
//! same machinery runs.

use std::path::Path;
use std::sync::Arc;

use git2::{IndexAddOption, Repository, Signature};
use nexo_driver_types::PreDreamSnapshotHook;
use nexo_memory_snapshot::local_fs::LocalFsSnapshotter;
use nexo_memory_snapshot::request::RestoreRequest;
use nexo_memory_snapshot::snapshotter::MemorySnapshotter;
use nexo_memory_snapshot::PreDreamSnapshotAdapter;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};
use std::str::FromStr;

async fn seed_sqlite(path: &Path, marker: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let opts = SqliteConnectOptions::from_str(&format!("sqlite:{}", path.display()))
        .unwrap()
        .create_if_missing(true);
    let mut conn = opts.connect().await.unwrap();
    sqlx::query("CREATE TABLE memories (id INTEGER PRIMARY KEY, body TEXT)")
        .execute(&mut conn)
        .await
        .unwrap();
    for i in 0..5 {
        sqlx::query("INSERT INTO memories (id, body) VALUES (?, ?)")
            .bind(i)
            .bind(format!("{marker}-{i}"))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    conn.close().await.unwrap();
}

async fn read_marker(db: &Path) -> String {
    let opts =
        SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=ro", db.display())).unwrap();
    let mut conn = opts.connect().await.unwrap();
    let v: String = sqlx::query_scalar("SELECT body FROM memories WHERE id = 0")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
    v
}

fn seed_memdir(memdir: &Path, marker: &str) {
    std::fs::create_dir_all(memdir).unwrap();
    let repo = Repository::init(memdir).unwrap();
    std::fs::write(
        memdir.join("MEMORY.md"),
        format!("# index\n- live state {marker}\n").as_bytes(),
    )
    .unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = Signature::now("operator", "ops@example.com").unwrap();
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &format!("seed {marker}"),
        &tree,
        &[],
    )
    .unwrap();
}

#[tokio::test]
async fn pre_dream_adapter_produces_usable_bundle_then_restore_recovers_state() {
    let tmp = tempfile::tempdir().unwrap();
    let s = LocalFsSnapshotter::builder()
        .state_root(tmp.path())
        .memdir_root(tmp.path().join("agents-memdir"))
        .sqlite_root(tmp.path().join("agents-sqlite"))
        .build()
        .unwrap();
    let s_arc: Arc<dyn MemorySnapshotter> = Arc::new(s);

    // Real agent state.
    let memdir = tmp.path().join("agents-memdir/ana");
    seed_memdir(&memdir, "v1");
    seed_sqlite(
        &tmp.path().join("agents-sqlite/ana/long_term.sqlite"),
        "v1",
    )
    .await;

    // Wire the adapter the way `AutoDreamRunner::with_pre_dream_snapshot`
    // would consume it.
    let adapter = PreDreamSnapshotAdapter::new(s_arc.clone());
    let hook: Arc<dyn PreDreamSnapshotHook> = adapter.into_arc();

    // Fire the hook the way the dream runner would.
    let run_id = "00000000-0000-0000-0000-000000000abc";
    hook.snapshot_before_dream("ana", "default", run_id)
        .await
        .expect("pre-dream snapshot must succeed on a real snapshotter");

    // Locate the bundle the adapter wrote.
    let metas = s_arc.list(&"ana".into(), "default").await.unwrap();
    let pre_dream = metas
        .iter()
        .find(|m| {
            m.label
                .as_deref()
                .map(|l| l == &format!("auto:pre-dream-{run_id}"))
                .unwrap_or(false)
        })
        .expect("a snapshot with the auto:pre-dream-<run_id> label must exist");
    assert!(pre_dream.bundle_path.exists());
    assert!(pre_dream.bundle_size_bytes > 0);

    // Verify reports clean integrity.
    let report = s_arc.verify(&pre_dream.bundle_path).await.unwrap();
    assert!(report.manifest_ok, "manifest seal must validate");
    assert!(report.bundle_sha256_ok, "sibling sha256 must match");
    assert!(report.per_artifact_ok, "every artifact hash must match");

    // Mutate live state — simulates the dream's fork-pass corrupting
    // memory or simply moving forward.
    std::fs::remove_file(tmp.path().join("agents-sqlite/ana/long_term.sqlite")).unwrap();
    seed_sqlite(
        &tmp.path().join("agents-sqlite/ana/long_term.sqlite"),
        "v2-corrupt",
    )
    .await;
    std::fs::write(memdir.join("MEMORY.md"), b"# WRECKED\n").unwrap();

    // Restore from the pre-dream bundle (the rollback the operator
    // would invoke after a bad dream).
    let mut req = RestoreRequest::new("ana", "default", &pre_dream.bundle_path);
    req.auto_pre_snapshot = false; // keep the assertion deterministic
    let r = s_arc.restore(req).await.unwrap();
    assert!(!r.dry_run);
    assert!(r.workers_restarted);
    assert!(r.sqlite_restored_dbs.contains(&"long_term".to_string()));

    // SQLite restored back to v1.
    let after = read_marker(&tmp.path().join("agents-sqlite/ana/long_term.sqlite")).await;
    assert_eq!(after, "v1-0", "SQLite must roll back to the pre-dream value");

    // Memdir restored back to v1.
    let memory = std::fs::read_to_string(memdir.join("MEMORY.md")).unwrap();
    assert!(
        memory.contains("live state v1"),
        "memdir must roll back to v1 contents (got: {memory:?})"
    );
}

#[tokio::test]
async fn pre_dream_adapter_label_correlates_to_run_id() {
    let tmp = tempfile::tempdir().unwrap();
    let s = LocalFsSnapshotter::builder()
        .state_root(tmp.path())
        .memdir_root(tmp.path().join("memdir"))
        .sqlite_root(tmp.path().join("sqlite"))
        .build()
        .unwrap();
    let s_arc: Arc<dyn MemorySnapshotter> = Arc::new(s);
    seed_memdir(&tmp.path().join("memdir/ana"), "x");

    let hook: Arc<dyn PreDreamSnapshotHook> =
        PreDreamSnapshotAdapter::new(s_arc.clone()).into_arc();

    for run_id in ["alpha", "bravo", "charlie"] {
        hook.snapshot_before_dream("ana", "default", run_id)
            .await
            .unwrap();
    }
    let metas = s_arc.list(&"ana".into(), "default").await.unwrap();
    let labels: Vec<_> = metas
        .iter()
        .filter_map(|m| m.label.clone())
        .collect();
    for run_id in ["alpha", "bravo", "charlie"] {
        let expected = format!("auto:pre-dream-{run_id}");
        assert!(
            labels.iter().any(|l| l == &expected),
            "label for run_id {run_id} not found in {labels:?}"
        );
    }
}
