//! Debounced file watcher for config hot-reload.
//!
//! Mirrors the pattern established in `nexo-extensions::watch` but
//! emits plain `()` notifications on a tokio channel instead of parsing
//! the content — the coordinator does the reload after debounced
//! events settle.
//!
//! Watched by default (relative to the config directory):
//!   - `agents.yaml`
//!   - `agents.d/` (recursive)
//!   - `llm.yaml`
//!   - `runtime.yaml`
//!
//! Extras can be listed under `runtime.reload.extra_watch_paths` in
//! `runtime.yaml`. The watcher attaches a single recursive watch on
//! the config directory (not the individual files), so files that
//! don't exist yet at boot — the setup wizard writes `agents.yaml` /
//! `llm.yaml` after first launch — and atomic-rename writes (admin RPC
//! writes a temp file then renames over the target) are both caught.
//! Events are filtered to the watched paths above.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use notify::{RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Built-in watch targets — the coordinator always adds these even if
/// `runtime.yaml` leaves `extra_watch_paths` empty.
pub const DEFAULT_WATCH_PATHS: &[&str] = &["agents.yaml", "agents.d", "llm.yaml", "runtime.yaml"];

/// Spawn a background task that watches the config directory and
/// sends a `()` notification every time the debouncer settles. The
/// caller picks up from the returned `mpsc::Receiver` and drives the
/// reload pipeline from there. Task terminates when `shutdown` is
/// cancelled.
pub fn spawn_config_watcher(
    config_dir: PathBuf,
    extra_paths: Vec<String>,
    debounce: Duration,
    shutdown: CancellationToken,
) -> anyhow::Result<mpsc::Receiver<()>> {
    let (tx, rx) = mpsc::channel::<()>(16);
    let notify_tx = tx.clone();

    // Run the debouncer on a dedicated tokio task so we can keep the
    // `Debouncer` alive for the watcher's lifetime without pinning it
    // to the spawning future.
    tokio::task::spawn_blocking(move || {
        let result = (|| -> anyhow::Result<()> {
            // Absolute paths we care about (the config files +
            // `agents.d/`). We watch the PARENT directory recursively
            // (below) and filter events to these. Watching the files
            // directly is wrong twice over: (a) on a fresh install the
            // files don't exist at boot — the setup wizard writes them
            // later, so a per-file watch finds no targets and disables
            // itself permanently; (b) admin-RPC + editors write via
            // atomic rename (temp file + rename over the target), which
            // replaces the inode a per-file `NonRecursive` watch is
            // bound to, so the Modify never fires. A recursive dir watch
            // catches both create and rename.
            let interesting: Vec<PathBuf> = DEFAULT_WATCH_PATHS
                .iter()
                .map(|p| config_dir.join(p))
                .chain(extra_paths.iter().map(|p| config_dir.join(p)))
                .collect();
            let filter = interesting.clone();

            let mut debouncer = new_debouncer(debounce, None, move |res: DebounceEventResult| {
                match res {
                    Ok(events) if !events.is_empty() => {
                        // Only fire when a watched file (or something
                        // under `agents.d/`) changed — ignore unrelated
                        // writes in the same dir (broker.yaml, plugins/,
                        // editor temp files). The coordinator dedupes
                        // subsequent reloads via its serial mutex.
                        let relevant = events.iter().any(|ev| {
                            ev.paths
                                .iter()
                                .any(|p| filter.iter().any(|t| p == t || p.starts_with(t)))
                        });
                        if relevant {
                            let _ = notify_tx.try_send(());
                        }
                    }
                    Ok(_) => {}
                    Err(errs) => {
                        for e in errs {
                            tracing::warn!(error = %e, "config watcher error");
                        }
                    }
                }
            })
            .context("spawn notify-debouncer-full")?;

            // One recursive watch on the config dir → creates + atomic
            // renames of the interesting files are caught even when they
            // don't exist yet at boot.
            let watched_any = if config_dir.is_dir() {
                match debouncer
                    .watcher()
                    .watch(&config_dir, RecursiveMode::Recursive)
                {
                    Ok(_) => {
                        tracing::info!(
                            config_dir = %config_dir.display(),
                            "config watcher attached (recursive dir watch)"
                        );
                        true
                    }
                    Err(e) => {
                        tracing::warn!(
                            config_dir = %config_dir.display(),
                            error = %e,
                            "config watcher failed to attach to config dir",
                        );
                        false
                    }
                }
            } else {
                false
            };

            if !watched_any {
                tracing::warn!(
                    config_dir = %config_dir.display(),
                    "config watcher has no live config dir — auto reload disabled until it appears and the process restarts"
                );
                // Keep the task alive until shutdown so the Receiver
                // end doesn't race; the coordinator can still take
                // manual reloads via `control.reload`.
            }

            // Keep the debouncer alive until shutdown fires. We use
            // blocking recv because this is a spawn_blocking task.
            loop {
                if shutdown.is_cancelled() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            drop(debouncer);
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!(error = %e, "config watcher terminated");
        }
    });

    let _ = tx; // keep sender alive paired with rx
    Ok(rx)
}

/// Returns the absolute config paths that a fresh watcher would
/// attach to, for diagnostics / tests. Does not observe the
/// filesystem.
pub fn planned_watch_paths(config_dir: &Path, extra: &[String]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = DEFAULT_WATCH_PATHS
        .iter()
        .map(|p| config_dir.join(p))
        .collect();
    for p in extra {
        out.push(config_dir.join(p));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn planned_paths_include_defaults_and_extras() {
        let dir = tempfile::tempdir().unwrap();
        let paths = planned_watch_paths(
            dir.path(),
            &["custom.yaml".to_string(), "nested/file.yaml".to_string()],
        );
        assert_eq!(paths.len(), DEFAULT_WATCH_PATHS.len() + 2);
        assert!(paths.iter().any(|p| p.ends_with("agents.yaml")));
        assert!(paths.iter().any(|p| p.ends_with("custom.yaml")));
        assert!(paths.iter().any(|p| p.ends_with("nested/file.yaml")));
    }

    #[tokio::test]
    async fn watcher_fires_on_file_write() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-create agents.yaml so the watcher actually attaches.
        fs::write(dir.path().join("agents.yaml"), "agents: []\n").unwrap();
        let shutdown = CancellationToken::new();
        let mut rx = spawn_config_watcher(
            dir.path().to_path_buf(),
            Vec::new(),
            Duration::from_millis(100),
            shutdown.clone(),
        )
        .unwrap();

        // Give the watcher a beat to attach.
        tokio::time::sleep(Duration::from_millis(200)).await;
        fs::write(dir.path().join("agents.yaml"), "agents: [{}]\n").unwrap();

        let fired = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("watcher must fire within 2s");
        assert_eq!(fired, Some(()));

        shutdown.cancel();
    }

    #[tokio::test]
    async fn watcher_fires_on_file_created_after_start() {
        // The fresh-install / wizard case: llm.yaml does NOT exist when
        // the daemon boots, the wizard creates it later. A per-file
        // watch would have found no target + disabled itself; the
        // recursive dir watch catches the create.
        let dir = tempfile::tempdir().unwrap();
        let shutdown = CancellationToken::new();
        let mut rx = spawn_config_watcher(
            dir.path().to_path_buf(),
            Vec::new(),
            Duration::from_millis(100),
            shutdown.clone(),
        )
        .unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;
        // Create llm.yaml AFTER the watcher started.
        fs::write(dir.path().join("llm.yaml"), "providers: {}\n").unwrap();

        let fired = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("watcher must fire on a file created after start");
        assert_eq!(fired, Some(()));

        shutdown.cancel();
    }

    #[tokio::test]
    async fn watcher_ignores_unrelated_files() {
        // A write to a file we don't track (e.g. broker.yaml) must not
        // trigger an agent reload.
        let dir = tempfile::tempdir().unwrap();
        let shutdown = CancellationToken::new();
        let mut rx = spawn_config_watcher(
            dir.path().to_path_buf(),
            Vec::new(),
            Duration::from_millis(100),
            shutdown.clone(),
        )
        .unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;
        fs::write(dir.path().join("broker.yaml"), "broker: {}\n").unwrap();

        let fired = tokio::time::timeout(Duration::from_millis(800), rx.recv()).await;
        assert!(
            fired.is_err(),
            "unrelated file write must not fire a reload"
        );

        shutdown.cancel();
    }
}
