//! Phase 18 — debounced file watcher for config hot-reload.
//!
//! Mirrors the pattern established in `agent-extensions::watch` but
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
//! `runtime.yaml`. Paths that don't exist at spawn time are skipped
//! with a `warn` — they're re-considered on the next restart (file
//! watchers can't retroactively attach to a path that appears later
//! without platform-specific tricks we don't need yet).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use notify::{RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Built-in watch targets — the coordinator always adds these even if
/// `runtime.yaml` leaves `extra_watch_paths` empty.
pub const DEFAULT_WATCH_PATHS: &[&str] =
    &["agents.yaml", "agents.d", "llm.yaml", "runtime.yaml"];

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
            let mut debouncer = new_debouncer(debounce, None, move |res: DebounceEventResult| {
                match res {
                    Ok(events) if !events.is_empty() => {
                        // Any debounced batch → one notification. The
                        // coordinator dedupes subsequent reloads via its
                        // serial mutex.
                        let _ = notify_tx.try_send(());
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

            let mut targets: Vec<PathBuf> = DEFAULT_WATCH_PATHS
                .iter()
                .map(|p| config_dir.join(p))
                .collect();
            for p in &extra_paths {
                targets.push(config_dir.join(p));
            }

            let mut watched_any = false;
            for target in &targets {
                if !target.exists() {
                    tracing::debug!(
                        path = %target.display(),
                        "config watch target does not exist — skipping"
                    );
                    continue;
                }
                let mode = if target.is_dir() {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                };
                match debouncer.watcher().watch(target, mode) {
                    Ok(_) => {
                        watched_any = true;
                        tracing::info!(path = %target.display(), mode = ?mode, "config watcher attached");
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %target.display(),
                            error = %e,
                            "config watcher failed to attach — skipping",
                        );
                    }
                }
            }

            if !watched_any {
                tracing::warn!(
                    config_dir = %config_dir.display(),
                    "config watcher has no live targets — auto reload disabled until the files appear and the process restarts"
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
}
