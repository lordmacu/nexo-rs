//! In-memory cache for `WorkspaceBundle` with `notify`-driven invalidation.
//!
//! Hot path: `llm_behavior::run_turn` builds the system prompt once per
//! turn. The legacy code re-read every workspace MD from disk on each
//! turn — fine when the agent is idle but a measurable hit on a busy
//! agent, and worse, the bundle's bytes vary across runs (read order,
//! mtime metadata) which prevents Anthropic prompt-cache hits even
//! though the *content* is stable.
//!
//! `WorkspaceCache` solves both:
//!
//! 1. **Stable bytes per workspace+scope+extras tuple** → the cache
//!    returns the same `Arc<WorkspaceBundle>` until a watch event
//!    invalidates it. Producers downstream (system prompt assembly)
//!    can emit a stable cached block to Anthropic.
//! 2. **Zero disk I/O on the steady state** → `get()` is a `DashMap`
//!    lookup; cache misses run the full `WorkspaceLoader` once and
//!    insert.
//!
//! Invalidation is per-workspace-root: a single `*.md` change drops
//! every entry under that root (across all scopes / extras
//! combinations). False-positive over-invalidation is fine — the next
//! `get()` re-loads.
//!
//! Optional max_age: notify(7) drops events on NFS/FUSE; operators on
//! exotic filesystems can set `WorkspaceCacheConfig::max_age_seconds`
//! to force a refresh after N seconds even without a notify event.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, FileIdMap};

use super::workspace::{SessionScope, WorkspaceBundle, WorkspaceLoader};

/// Cache key. `extras` is sorted at insertion so callers can pass
/// `&AgentConfig::extra_docs` in arbitrary order without splitting
/// the cache.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct CacheKey {
    root: PathBuf,
    scope: SessionScope,
    extras: Vec<String>,
}

#[derive(Debug)]
struct Entry {
    bundle: Arc<WorkspaceBundle>,
    loaded_at: Instant,
}

#[derive(Debug, Default)]
pub struct WorkspaceCacheMetrics {
    /// Hits counted per workspace root (`PathBuf::display`-stringified).
    pub hits: DashMap<String, AtomicU64>,
    pub misses: DashMap<String, AtomicU64>,
    pub invalidations: DashMap<String, AtomicU64>,
}

impl WorkspaceCacheMetrics {
    fn bump(map: &DashMap<String, AtomicU64>, key: &str) {
        if let Some(c) = map.get(key) {
            c.value().fetch_add(1, Ordering::Relaxed);
            return;
        }
        let entry = map.entry(key.to_string()).or_insert_with(|| AtomicU64::new(0));
        entry.value().fetch_add(1, Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> HashMap<String, (u64, u64, u64)> {
        let mut out: HashMap<String, (u64, u64, u64)> = HashMap::new();
        for kv in self.hits.iter() {
            out.entry(kv.key().clone()).or_default().0 = kv.value().load(Ordering::Relaxed);
        }
        for kv in self.misses.iter() {
            out.entry(kv.key().clone()).or_default().1 = kv.value().load(Ordering::Relaxed);
        }
        for kv in self.invalidations.iter() {
            out.entry(kv.key().clone()).or_default().2 = kv.value().load(Ordering::Relaxed);
        }
        out
    }
}

pub struct WorkspaceCache {
    bundles: DashMap<CacheKey, Arc<Entry>>,
    metrics: Arc<WorkspaceCacheMetrics>,
    /// Roots being watched. The watcher itself is held to keep
    /// notifications flowing for the lifetime of the cache.
    roots: Vec<PathBuf>,
    max_age: Option<Duration>,
    _watcher: Mutex<Option<Debouncer<RecommendedWatcher, FileIdMap>>>,
}

impl WorkspaceCache {
    /// Build a cache for the given workspace roots. Each root is
    /// watched recursively; any `*.md` change invalidates every entry
    /// keyed under that root.
    ///
    /// `debounce_ms`: coalesces bursts (saves from editors that write
    /// + rename + chmod). 500ms is a reasonable default.
    /// `max_age_seconds`: force a refresh after this many seconds even
    /// without a watch event. 0 = disabled.
    pub fn new(
        roots: &[PathBuf],
        debounce_ms: u32,
        max_age_seconds: u32,
    ) -> anyhow::Result<Arc<Self>> {
        let metrics = Arc::new(WorkspaceCacheMetrics::default());
        let bundles: DashMap<CacheKey, Arc<Entry>> = DashMap::new();
        let max_age = if max_age_seconds == 0 {
            None
        } else {
            Some(Duration::from_secs(max_age_seconds as u64))
        };
        let cache = Arc::new(Self {
            bundles,
            metrics,
            roots: roots.iter().map(|p| p.to_path_buf()).collect(),
            max_age,
            _watcher: Mutex::new(None),
        });
        cache.start_watcher(debounce_ms)?;
        Ok(cache)
    }

    fn start_watcher(self: &Arc<Self>, debounce_ms: u32) -> anyhow::Result<()> {
        if self.roots.is_empty() {
            return Ok(());
        }
        let weak = Arc::downgrade(self);
        let timeout = Duration::from_millis(debounce_ms.max(50) as u64);
        let mut debouncer = new_debouncer(timeout, None, move |res: DebounceEventResult| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            match res {
                Ok(events) => {
                    let mut roots_to_drop: Vec<PathBuf> = Vec::new();
                    for ev in events {
                        for path in &ev.event.paths {
                            // Only react to .md changes — extras are MDs too,
                            // so a single test covers the lot. Anything else
                            // (transient editor swap files, .DS_Store, …) is
                            // a no-op.
                            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                                continue;
                            }
                            // Find which watched root this path lives under.
                            for root in &this.roots {
                                if path.starts_with(root) && !roots_to_drop.contains(root) {
                                    roots_to_drop.push(root.clone());
                                }
                            }
                        }
                    }
                    for root in roots_to_drop {
                        this.invalidate_root(&root);
                    }
                }
                Err(errs) => {
                    for e in errs {
                        tracing::warn!(error = ?e, "workspace_cache: watcher error");
                    }
                }
            }
        })?;
        for root in &self.roots {
            // RecursiveMode::Recursive — we want subdirectory changes
            // (memory/YYYY-MM-DD.md, doc subfolders).
            if let Err(e) = debouncer.watcher().watch(root, RecursiveMode::Recursive) {
                tracing::warn!(
                    root = %root.display(),
                    error = %e,
                    "workspace_cache: failed to start watcher (cache will still serve hits, just no auto-invalidation)"
                );
            }
        }
        if let Ok(mut slot) = self._watcher.lock() {
            *slot = Some(debouncer);
        }
        Ok(())
    }

    /// Invalidate every entry under the given workspace root. Idempotent.
    pub fn invalidate_root(&self, root: &Path) {
        let key = root.display().to_string();
        let before = self.bundles.len();
        self.bundles.retain(|k, _| k.root != root);
        let dropped = before - self.bundles.len();
        if dropped > 0 {
            WorkspaceCacheMetrics::bump(&self.metrics.invalidations, &key);
            tracing::info!(
                root = %root.display(),
                dropped,
                "workspace_cache: invalidated entries"
            );
        }
    }

    /// Clear the entire cache. Test-only / admin tool.
    pub fn clear(&self) {
        self.bundles.clear();
    }

    /// Return the metrics handle (counters per workspace root).
    pub fn metrics(&self) -> Arc<WorkspaceCacheMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Get (or build) the bundle for `(root, scope, extras)`. Falls
    /// back to a fresh `WorkspaceLoader` on miss; subsequent calls hit
    /// the cache until invalidated.
    pub async fn get(
        &self,
        root: &Path,
        scope: SessionScope,
        extras: &[String],
    ) -> anyhow::Result<Arc<WorkspaceBundle>> {
        let mut key_extras: Vec<String> = extras
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        key_extras.sort();
        let key = CacheKey {
            root: root.to_path_buf(),
            scope,
            extras: key_extras,
        };
        let root_label = root.display().to_string();
        if let Some(entry) = self.bundles.get(&key) {
            // max_age guard: stale entries are dropped lazily on access
            // so an idle cache doesn't grow unbounded with expired
            // entries.
            if let Some(max_age) = self.max_age {
                if entry.value().loaded_at.elapsed() > max_age {
                    drop(entry);
                    self.bundles.remove(&key);
                    WorkspaceCacheMetrics::bump(&self.metrics.invalidations, &root_label);
                } else {
                    WorkspaceCacheMetrics::bump(&self.metrics.hits, &root_label);
                    return Ok(Arc::clone(&entry.value().bundle));
                }
            } else {
                WorkspaceCacheMetrics::bump(&self.metrics.hits, &root_label);
                return Ok(Arc::clone(&entry.value().bundle));
            }
        }
        // Miss → build via the same loader the legacy path uses.
        WorkspaceCacheMetrics::bump(&self.metrics.misses, &root_label);
        let bundle = WorkspaceLoader::new(root)
            .load_with_extras(scope, extras)
            .await?;
        let entry = Arc::new(Entry {
            bundle: Arc::new(bundle),
            loaded_at: Instant::now(),
        });
        // Race-safe insert: if another task beat us to it, return
        // theirs to keep cache identity stable for prompt-cache hashing.
        let stored = self
            .bundles
            .entry(key)
            .or_insert_with(|| Arc::clone(&entry))
            .value()
            .clone();
        Ok(Arc::clone(&stored.bundle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(p: &Path, name: &str, text: &str) {
        std::fs::write(p.join(name), text).unwrap();
    }

    #[tokio::test]
    async fn cold_then_warm() {
        let dir = tempdir().unwrap();
        write(dir.path(), "IDENTITY.md", "- **Name:** Ana");
        let cache = WorkspaceCache::new(&[dir.path().to_path_buf()], 100, 0).unwrap();
        let a = cache
            .get(dir.path(), SessionScope::Main, &[])
            .await
            .unwrap();
        let b = cache
            .get(dir.path(), SessionScope::Main, &[])
            .await
            .unwrap();
        // Same Arc → same bytes → cache hit on the prompt-cache side.
        assert!(Arc::ptr_eq(&a, &b));
        let snap = cache.metrics().snapshot();
        let (hits, misses, _) = snap
            .get(&dir.path().display().to_string())
            .copied()
            .unwrap_or_default();
        assert_eq!(misses, 1);
        assert_eq!(hits, 1);
    }

    #[tokio::test]
    async fn different_scope_separate_entry() {
        let dir = tempdir().unwrap();
        write(dir.path(), "IDENTITY.md", "- **Name:** Ana");
        let cache = WorkspaceCache::new(&[dir.path().to_path_buf()], 100, 0).unwrap();
        let main = cache
            .get(dir.path(), SessionScope::Main, &[])
            .await
            .unwrap();
        let shared = cache
            .get(dir.path(), SessionScope::Shared, &[])
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&main, &shared));
    }

    #[tokio::test]
    async fn extras_order_does_not_split_cache() {
        let dir = tempdir().unwrap();
        write(dir.path(), "A.md", "a");
        write(dir.path(), "B.md", "b");
        let cache = WorkspaceCache::new(&[dir.path().to_path_buf()], 100, 0).unwrap();
        let one = cache
            .get(
                dir.path(),
                SessionScope::Main,
                &["A.md".to_string(), "B.md".to_string()],
            )
            .await
            .unwrap();
        let two = cache
            .get(
                dir.path(),
                SessionScope::Main,
                &["B.md".to_string(), "A.md".to_string()],
            )
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&one, &two));
    }

    #[tokio::test]
    async fn manual_invalidate_drops_entries() {
        let dir = tempdir().unwrap();
        write(dir.path(), "IDENTITY.md", "- **Name:** Ana");
        let cache = WorkspaceCache::new(&[dir.path().to_path_buf()], 100, 0).unwrap();
        let _ = cache
            .get(dir.path(), SessionScope::Main, &[])
            .await
            .unwrap();
        cache.invalidate_root(dir.path());
        let snap = cache.metrics().snapshot();
        let (_, _, inv) = snap
            .get(&dir.path().display().to_string())
            .copied()
            .unwrap_or_default();
        assert_eq!(inv, 1);
    }

    #[tokio::test]
    async fn watcher_invalidates_after_md_write() {
        let dir = tempdir().unwrap();
        write(dir.path(), "IDENTITY.md", "- **Name:** Ana");
        let cache = WorkspaceCache::new(&[dir.path().to_path_buf()], 80, 0).unwrap();
        let a = cache
            .get(dir.path(), SessionScope::Main, &[])
            .await
            .unwrap();
        // Modify the watched file.
        std::fs::write(
            dir.path().join("IDENTITY.md"),
            "- **Name:** Ana\n- **Vibe:** updated",
        )
        .unwrap();
        // Wait for debounce + a margin.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let b = cache
            .get(dir.path(), SessionScope::Main, &[])
            .await
            .unwrap();
        assert!(
            !Arc::ptr_eq(&a, &b),
            "expected cache miss after MD modification"
        );
        let snap = cache.metrics().snapshot();
        let (_, _, inv) = snap
            .get(&dir.path().display().to_string())
            .copied()
            .unwrap_or_default();
        assert!(inv >= 1, "expected at least one invalidation, got {inv}");
    }

    #[tokio::test]
    async fn max_age_forces_refresh() {
        let dir = tempdir().unwrap();
        write(dir.path(), "IDENTITY.md", "- **Name:** Ana");
        // max_age 1s
        let cache = WorkspaceCache::new(&[dir.path().to_path_buf()], 100, 1).unwrap();
        let a = cache
            .get(dir.path(), SessionScope::Main, &[])
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let b = cache
            .get(dir.path(), SessionScope::Main, &[])
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn non_md_file_ignored_by_watcher() {
        let dir = tempdir().unwrap();
        write(dir.path(), "IDENTITY.md", "- **Name:** Ana");
        let cache = WorkspaceCache::new(&[dir.path().to_path_buf()], 80, 0).unwrap();
        let a = cache
            .get(dir.path(), SessionScope::Main, &[])
            .await
            .unwrap();
        std::fs::write(dir.path().join("note.txt"), "ignored").unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        let b = cache
            .get(dir.path(), SessionScope::Main, &[])
            .await
            .unwrap();
        // Same Arc — non-md change must not invalidate.
        assert!(Arc::ptr_eq(&a, &b));
    }
}
