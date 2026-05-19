//! Disk cache for the merged catalogue.
//!
//! Single JSON file at `<state_dir>/plugin-discovery/catalogue.json`.
//! Writes go to a sibling `.tmp` file then atomically rename to the
//! final path — readers either see the previous snapshot or the new
//! one, never a torn write.
//!
//! Reads return `Some(snapshot)` only when the file exists AND its
//! `fetched_at_ms` is within `ttl_ms` of the caller-supplied `now_ms`.
//! Stale snapshots are reported via [`DiskCache::read_any`] for the
//! stale-while-revalidate path in [`crate`]'s client orchestrator.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::fs;

use crate::types::CachedCatalogue;

/// Cache I/O failures. Soft errors (missing file, parse failure)
/// surface as `Ok(None)` from the read API; this enum only covers
/// the cases the caller cannot safely ignore.
#[derive(Debug, Error)]
pub enum CacheError {
    /// File-system errors. Includes the path under operation for
    /// triage.
    #[error("{op}: {source} (path: {path})")]
    Io {
        /// Logical operation that failed (`write`, `rename`, etc).
        op: &'static str,
        /// Filesystem path involved.
        path: PathBuf,
        /// Inner I/O failure.
        #[source]
        source: io::Error,
    },
    /// `serde_json::to_string` failed. Effectively unreachable for
    /// our payload but exposed in the public error enum so the
    /// caller's match is exhaustive without `_ =>` wildcards.
    #[error("serialize: {0}")]
    Serialize(#[source] serde_json::Error),
}

/// Disk persistence helper for one cached catalogue file.
#[derive(Debug, Clone)]
pub struct DiskCache {
    cache_file: PathBuf,
}

impl DiskCache {
    /// Bind a cache to a specific catalogue path. Path is created
    /// lazily on first write — readers tolerate the parent dir
    /// not existing yet.
    pub fn new(cache_file: PathBuf) -> Self {
        Self { cache_file }
    }

    /// Logical cache file path (no I/O).
    pub fn path(&self) -> &Path {
        &self.cache_file
    }

    /// Read the cached snapshot regardless of staleness. Returns:
    ///   - `Ok(Some(snap))` — file existed AND parsed cleanly.
    ///   - `Ok(None)` — file missing OR parse failed (latter
    ///     emits a `tracing::warn` for ops triage).
    ///   - `Err(CacheError::Io)` — only for non-`NotFound` I/O
    ///     errors (permission denied, EIO, etc).
    pub async fn read_any(&self) -> Result<Option<CachedCatalogue>, CacheError> {
        let bytes = match fs::read(&self.cache_file).await {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(CacheError::Io {
                    op: "read",
                    path: self.cache_file.clone(),
                    source: e,
                });
            }
        };
        match serde_json::from_slice::<CachedCatalogue>(&bytes) {
            Ok(snap) => Ok(Some(snap)),
            Err(e) => {
                tracing::warn!(
                    path = %self.cache_file.display(),
                    error = %e,
                    "plugin-discovery cache: parse failed; treating as miss"
                );
                Ok(None)
            }
        }
    }

    /// Read only when the snapshot is fresh (younger than `ttl_ms`
    /// relative to `now_ms`). Stale entries return `Ok(None)`; the
    /// caller normally falls through to a network refresh and a
    /// subsequent [`Self::write_atomic`].
    pub async fn read_fresh(
        &self,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<Option<CachedCatalogue>, CacheError> {
        match self.read_any().await? {
            Some(snap) if !snap.is_stale(now_ms, ttl_ms) => Ok(Some(snap)),
            _ => Ok(None),
        }
    }

    /// Persist a snapshot. Implementation:
    ///   1. `mkdir -p` the parent dir (idempotent).
    ///   2. Write to a sibling `.tmp` file in the same dir so
    ///      `rename` stays on one filesystem (atomic on POSIX).
    ///   3. `rename(tmp, final)` swaps the live snapshot in one
    ///      step — readers always see either the prior snapshot
    ///      or the new one, never a half-written file.
    ///   4. On error any partial `.tmp` is best-effort removed.
    pub async fn write_atomic(&self, snap: &CachedCatalogue) -> Result<(), CacheError> {
        let json = serde_json::to_vec_pretty(snap).map_err(CacheError::Serialize)?;
        if let Some(parent) = self.cache_file.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| CacheError::Io {
                    op: "create_dir_all",
                    path: parent.to_path_buf(),
                    source: e,
                })?;
        }
        let tmp = self.tmp_path();
        if let Err(e) = fs::write(&tmp, &json).await {
            let _ = fs::remove_file(&tmp).await;
            return Err(CacheError::Io {
                op: "write_tmp",
                path: tmp.clone(),
                source: e,
            });
        }
        if let Err(e) = fs::rename(&tmp, &self.cache_file).await {
            let _ = fs::remove_file(&tmp).await;
            return Err(CacheError::Io {
                op: "rename",
                path: self.cache_file.clone(),
                source: e,
            });
        }
        Ok(())
    }

    /// Forget the cache. Returns `Ok(())` when file missing
    /// (idempotent) — matches the operator-facing "refresh" RPC's
    /// expected semantics.
    pub async fn invalidate(&self) -> Result<(), CacheError> {
        match fs::remove_file(&self.cache_file).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CacheError::Io {
                op: "remove",
                path: self.cache_file.clone(),
                source: e,
            }),
        }
    }

    fn tmp_path(&self) -> PathBuf {
        let mut tmp = self.cache_file.clone();
        let file_name = tmp
            .file_name()
            .map(|n| n.to_owned())
            .unwrap_or_else(|| std::ffi::OsString::from("catalogue.json"));
        let mut fname = file_name;
        fname.push(".tmp");
        tmp.set_file_name(fname);
        tmp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CachedCatalogue;
    use tempfile::TempDir;

    fn cache_in(tmp: &TempDir) -> DiskCache {
        DiskCache::new(tmp.path().join("plugin-discovery").join("catalogue.json"))
    }

    fn snap(fetched_at: u64) -> CachedCatalogue {
        CachedCatalogue {
            fetched_at_ms: fetched_at,
            items: vec![],
        }
    }

    #[tokio::test]
    async fn miss_returns_none() {
        let tmp = TempDir::new().unwrap();
        let cache = cache_in(&tmp);
        let res = cache.read_any().await.unwrap();
        assert!(res.is_none(), "fresh dir must yield cache miss");
    }

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let cache = cache_in(&tmp);
        let s = snap(123_456);
        cache.write_atomic(&s).await.unwrap();
        let got = cache.read_any().await.unwrap().expect("snapshot present");
        assert_eq!(got, s);
    }

    #[tokio::test]
    async fn read_fresh_filters_by_ttl() {
        let tmp = TempDir::new().unwrap();
        let cache = cache_in(&tmp);
        let s = snap(1_000);
        cache.write_atomic(&s).await.unwrap();
        // Within TTL window → Some.
        let fresh = cache.read_fresh(1_050, 100).await.unwrap();
        assert!(fresh.is_some());
        // Past TTL → None.
        let stale = cache.read_fresh(1_200, 100).await.unwrap();
        assert!(stale.is_none());
    }

    #[tokio::test]
    async fn write_atomic_via_tmp_then_rename() {
        let tmp = TempDir::new().unwrap();
        let cache = cache_in(&tmp);
        cache.write_atomic(&snap(1)).await.unwrap();
        // Final exists; tmp does not (cleaned by rename).
        assert!(cache.cache_file.exists());
        let tmp_path = cache.tmp_path();
        assert!(
            !tmp_path.exists(),
            "tmp must not survive a successful write: {}",
            tmp_path.display()
        );
    }

    #[tokio::test]
    async fn invalidate_is_idempotent_on_missing() {
        let tmp = TempDir::new().unwrap();
        let cache = cache_in(&tmp);
        // Missing → Ok.
        cache.invalidate().await.unwrap();
        // Write then invalidate → file gone.
        cache.write_atomic(&snap(7)).await.unwrap();
        assert!(cache.cache_file.exists());
        cache.invalidate().await.unwrap();
        assert!(!cache.cache_file.exists());
    }

    #[tokio::test]
    async fn parse_failure_returns_miss_with_warn() {
        let tmp = TempDir::new().unwrap();
        let cache = cache_in(&tmp);
        // Create parent + write garbage that won't deserialize.
        fs::create_dir_all(cache.cache_file.parent().unwrap())
            .await
            .unwrap();
        fs::write(&cache.cache_file, b"not json").await.unwrap();
        let res = cache.read_any().await.unwrap();
        assert!(
            res.is_none(),
            "malformed JSON must surface as miss, not error"
        );
    }
}
