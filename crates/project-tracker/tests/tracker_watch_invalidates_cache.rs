//! End-to-end check: editing PHASES.md after the tracker has cached
//! it must produce updated state on the next read. We don't depend
//! on the watcher firing within a tight window — TTL fallback is
//! deliberately part of the contract — so we set a 50 ms TTL to
//! exercise the cache-expiry path even on systems where the OS file
//! event arrives late or not at all.

use std::path::Path;
use std::time::Duration;

use nexo_project_tracker::{FsProjectTracker, ProjectTracker};

fn write(p: &Path, body: &str) {
    std::fs::write(p, body).unwrap();
}

#[tokio::test]
async fn cache_invalidates_on_file_change() {
    let dir = std::env::temp_dir().join(format!(
        "nexo-tracker-watch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let phases = dir.join("PHASES.md");
    write(&phases, "## Phase 1 — X\n\n#### 1.1 — A   ⬜\n");

    let tracker = FsProjectTracker::open(&dir)
        .unwrap()
        .with_ttl(Duration::from_millis(50))
        .with_watch();

    let first = tracker.phases().await.unwrap();
    assert_eq!(first[0].sub_phases[0].title, "A");

    // Edit the file. Sleep past TTL so the cache is forced to
    // re-read regardless of whether the OS event landed.
    write(&phases, "## Phase 1 — X\n\n#### 1.1 — A renamed   ✅\n");
    tokio::time::sleep(Duration::from_millis(120)).await;

    let after = tracker.phases().await.unwrap();
    assert_eq!(after[0].sub_phases[0].title, "A renamed");
    assert!(after[0].sub_phases[0].status.is_done());

    let _ = std::fs::remove_dir_all(&dir);
}
