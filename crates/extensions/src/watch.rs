//! Phase 11.2 follow-up — file watcher for `plugin.toml` manifests.
//!
//! Observes the extension search roots and emits structured `warn` logs
//! on add/edit/remove events. No auto-respawn — subprocess lifecycle is
//! complex (state, bundled MCP servers, hooks); operators restart the
//! agent to apply changes. Parse failures log `error`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::manifest::ExtensionManifest;

pub const PLUGIN_TOML_FILENAME: &str = "plugin.toml";

/// Known state at boot time — filled by the extension discovery pass.
#[derive(Debug, Clone, Default)]
pub struct KnownPluginSnapshot {
    /// plugin_id → canonical path to the plugin.toml.
    pub paths: HashMap<String, PathBuf>,
    /// plugin_id → sha256 hex of the manifest bytes at boot.
    pub hashes: HashMap<String, String>,
}

impl KnownPluginSnapshot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a known plugin. `path` is canonicalized best-effort so
    /// later filesystem events match.
    pub fn insert(&mut self, plugin_id: impl Into<String>, path: PathBuf) {
        let id = plugin_id.into();
        let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
        let hash = hash_file(&canonical).unwrap_or_default();
        self.paths.insert(id.clone(), canonical);
        self.hashes.insert(id, hash);
    }
}

/// Spawn a background task that watches `roots` for `plugin.toml`
/// changes. Emits tracing events; no side effects on the runtime. The
/// task stops when `shutdown` is cancelled.
pub fn spawn_extensions_watcher(
    roots: Vec<PathBuf>,
    initial: KnownPluginSnapshot,
    debounce: Duration,
    shutdown: CancellationToken,
) {
    tokio::spawn(async move {
        if let Err(e) = run_watcher(roots, initial, debounce, shutdown).await {
            tracing::warn!(error = %e, "extensions watcher terminated");
        }
    });
}

async fn run_watcher(
    roots: Vec<PathBuf>,
    initial: KnownPluginSnapshot,
    debounce: Duration,
    shutdown: CancellationToken,
) -> Result<()> {
    let state = Arc::new(Mutex::new(initial));
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<PathBuf>>();

    let mut debouncer = new_debouncer(debounce, None, move |res: DebounceEventResult| {
        let events = match res {
            Ok(events) => events,
            Err(errs) => {
                for e in errs {
                    tracing::warn!(error = %e, "extensions watcher error");
                }
                return;
            }
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        for ev in events {
            for p in &ev.paths {
                if p.file_name().and_then(|s| s.to_str()) == Some(PLUGIN_TOML_FILENAME) {
                    paths.push(p.clone());
                }
            }
        }
        if !paths.is_empty() {
            let _ = tx.send(paths);
        }
    })
    .context("failed to construct debouncer")?;

    let mut watched_any = false;
    for root in &roots {
        match debouncer
            .watcher()
            .watch(root, RecursiveMode::Recursive)
        {
            Ok(()) => {
                watched_any = true;
                tracing::info!(root = %root.display(), "watching plugin.toml under root");
            }
            Err(e) => tracing::warn!(root = %root.display(), error = %e, "watch root failed"),
        }
    }
    if !watched_any {
        tracing::warn!("extensions watcher has no roots; stopping");
        return Ok(());
    }

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            maybe = rx.recv() => {
                let Some(paths) = maybe else { break };
                for path in paths {
                    handle_change(&path, &state).await;
                }
            }
        }
    }
    tracing::info!("extensions watcher stopped");
    Ok(())
}

async fn handle_change(path: &Path, state: &Arc<Mutex<KnownPluginSnapshot>>) {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // File removed / renamed away?
    if !canonical.exists() {
        let mut guard = state.lock().await;
        if let Some(pid) = pid_by_path(&guard, &canonical) {
            guard.paths.remove(&pid);
            guard.hashes.remove(&pid);
            tracing::warn!(
                plugin_id = %pid,
                path = %canonical.display(),
                "extension removed; restart to shut down"
            );
        } else {
            tracing::debug!(path = %canonical.display(), "plugin.toml path vanished; not tracked");
        }
        return;
    }

    // Parse the manifest to detect add/edit.
    let parsed = match ExtensionManifest::from_path(&canonical) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(
                path = %canonical.display(),
                error = %e,
                "invalid plugin.toml — skipping"
            );
            return;
        }
    };
    let plugin_id = parsed.id().to_string();
    let new_hash = hash_file(&canonical).unwrap_or_default();

    let mut guard = state.lock().await;
    match guard.hashes.get(&plugin_id).cloned() {
        None => {
            guard.paths.insert(plugin_id.clone(), canonical.clone());
            guard.hashes.insert(plugin_id.clone(), new_hash);
            tracing::warn!(
                plugin_id = %plugin_id,
                path = %canonical.display(),
                "new extension detected; restart to load"
            );
        }
        Some(old_hash) if old_hash == new_hash => {
            // No content change — debouncer emits spurious writes.
            tracing::debug!(plugin_id = %plugin_id, "manifest unchanged; no-op");
        }
        Some(_) => {
            guard.hashes.insert(plugin_id.clone(), new_hash);
            guard.paths.insert(plugin_id.clone(), canonical.clone());
            tracing::warn!(
                plugin_id = %plugin_id,
                path = %canonical.display(),
                "extension manifest changed; restart to apply"
            );
        }
    }
}

fn pid_by_path(snapshot: &KnownPluginSnapshot, path: &Path) -> Option<String> {
    snapshot
        .paths
        .iter()
        .find(|(_, p)| p.as_path() == path)
        .map(|(id, _)| id.clone())
}

/// Read the file and return the hex-encoded sha256 of its bytes. Returns
/// an empty string if the file cannot be read (callers treat that as "no
/// hash known" so the next successful read shows a diff).
pub fn hash_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read {}", path.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(hex_encode(&h.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn hash_file_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("plugin.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "[plugin]\nid = \"x\"").unwrap();
        let a = hash_file(&p).unwrap();
        let b = hash_file(&p).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn hash_file_differs_on_content_change() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("plugin.toml");
        std::fs::write(&p, b"v1").unwrap();
        let h1 = hash_file(&p).unwrap();
        std::fs::write(&p, b"v2").unwrap();
        let h2 = hash_file(&p).unwrap();
        assert_ne!(h1, h2);
    }
}
