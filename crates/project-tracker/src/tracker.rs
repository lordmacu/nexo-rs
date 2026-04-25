//! `ProjectTracker` trait + `FsProjectTracker` filesystem
//! implementation. The filesystem variant caches parsed state and
//! invalidates the cache on `notify` file events; the next read
//! triggers a re-parse. Reads always fall back to a direct parse if
//! the cache is empty, so a missed event can never serve stale data
//! indefinitely.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;

use crate::parser::{followups, phases};
use crate::types::{FollowUp, Phase, PhaseStatus, SubPhase, TrackerError};

#[async_trait]
pub trait ProjectTracker: Send + Sync + 'static {
    async fn phases(&self) -> Result<Vec<Phase>, TrackerError>;
    async fn followups(&self) -> Result<Vec<FollowUp>, TrackerError>;

    /// First sub-phase whose status is `InProgress`, else first
    /// `Pending`. Returns `None` only if every sub-phase is `Done`.
    async fn current_phase(&self) -> Result<Option<SubPhase>, TrackerError> {
        let phases = self.phases().await?;
        for p in &phases {
            for s in &p.sub_phases {
                if s.status == PhaseStatus::InProgress {
                    return Ok(Some(s.clone()));
                }
            }
        }
        for p in &phases {
            for s in &p.sub_phases {
                if s.status == PhaseStatus::Pending {
                    return Ok(Some(s.clone()));
                }
            }
        }
        Ok(None)
    }

    /// First `Pending` sub-phase ignoring InProgress (i.e. "what
    /// comes after the active one").
    async fn next_phase(&self) -> Result<Option<SubPhase>, TrackerError> {
        let phases = self.phases().await?;
        let mut seen_in_progress = false;
        for p in &phases {
            for s in &p.sub_phases {
                if s.status == PhaseStatus::InProgress {
                    seen_in_progress = true;
                } else if s.status == PhaseStatus::Pending {
                    if seen_in_progress {
                        return Ok(Some(s.clone()));
                    }
                    // No active phase — first pending counts.
                    return Ok(Some(s.clone()));
                }
            }
        }
        Ok(None)
    }

    /// Top N `Done` sub-phases by document order, last-first.
    async fn last_shipped(&self, n: usize) -> Result<Vec<SubPhase>, TrackerError> {
        let phases = self.phases().await?;
        let mut done: Vec<SubPhase> = Vec::new();
        for p in &phases {
            for s in &p.sub_phases {
                if s.status == PhaseStatus::Done {
                    done.push(s.clone());
                }
            }
        }
        done.reverse();
        done.truncate(n);
        Ok(done)
    }

    async fn phase_detail(&self, id: &str) -> Result<Option<SubPhase>, TrackerError> {
        let phases = self.phases().await?;
        for p in &phases {
            for s in &p.sub_phases {
                if s.id == id {
                    return Ok(Some(s.clone()));
                }
            }
        }
        Ok(None)
    }
}

#[derive(Default)]
struct CacheState {
    phases: Option<(Instant, Vec<Phase>)>,
    followups: Option<(Instant, Vec<FollowUp>)>,
}

pub struct FsProjectTracker {
    phases_path: PathBuf,
    followups_path: Option<PathBuf>,
    cache: Arc<RwLock<CacheState>>,
    ttl: Duration,
    /// Held to keep the watcher thread alive. `None` if watching is
    /// disabled.
    _watcher: Option<RecommendedWatcher>,
}

impl std::fmt::Debug for FsProjectTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsProjectTracker")
            .field("phases_path", &self.phases_path)
            .field("followups_path", &self.followups_path)
            .field("ttl", &self.ttl)
            .field("watching", &self._watcher.is_some())
            .finish()
    }
}

impl FsProjectTracker {
    /// Open a tracker rooted at `root`. Reads `<root>/PHASES.md` and
    /// optionally `<root>/FOLLOWUPS.md` (if present). PHASES.md must
    /// exist — its absence flips the project into `NotTracked` and
    /// every read returns that error.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, TrackerError> {
        let root = root.into();
        let phases_path = root.join("PHASES.md");
        if !phases_path.exists() {
            return Err(TrackerError::NotTracked(phases_path));
        }
        let followups_path = {
            let p = root.join("FOLLOWUPS.md");
            p.exists().then_some(p)
        };
        Ok(Self {
            phases_path,
            followups_path,
            cache: Arc::new(RwLock::new(CacheState::default())),
            ttl: Duration::from_secs(60),
            _watcher: None,
        })
    }

    /// Override the soft TTL used as a safety-net if the watcher
    /// misses an event. Default 60s.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Start a `notify` watcher. Failures fall back to TTL-only mode
    /// — the tracker is still usable, just less responsive.
    pub fn with_watch(mut self) -> Self {
        let cache = Arc::clone(&self.cache);
        let phases_path = self.phases_path.clone();
        let followups_path = self.followups_path.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let Ok(ev) = res else { return };
            let touches_phases = ev.paths.iter().any(|p| p == &phases_path);
            let touches_followups = followups_path
                .as_ref()
                .map(|f| ev.paths.iter().any(|p| p == f))
                .unwrap_or(false);
            if !touches_phases && !touches_followups {
                return;
            }
            // We only care about content-changing events; everything
            // else (access, metadata) is a false positive.
            if !matches!(
                ev.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            ) {
                return;
            }
            let mut g = cache.write();
            if touches_phases {
                g.phases = None;
            }
            if touches_followups {
                g.followups = None;
            }
        });
        if let Ok(mut w) = watcher {
            // Watch the parent dir of PHASES.md non-recursively;
            // FOLLOWUPS.md must live in the same directory.
            if let Some(parent) = self.phases_path.parent() {
                if w.watch(parent, RecursiveMode::NonRecursive).is_ok() {
                    self._watcher = Some(w);
                }
            }
        }
        self
    }

    fn read_phases_uncached(&self) -> Result<Vec<Phase>, TrackerError> {
        phases::parse_file(&self.phases_path)
    }

    fn read_followups_uncached(&self) -> Result<Vec<FollowUp>, TrackerError> {
        match &self.followups_path {
            Some(p) => followups::parse_file(p),
            None => Ok(Vec::new()),
        }
    }

    fn phases_fresh(&self) -> Option<Vec<Phase>> {
        let g = self.cache.read();
        g.phases
            .as_ref()
            .filter(|(t, _)| t.elapsed() < self.ttl)
            .map(|(_, v)| v.clone())
    }

    fn followups_fresh(&self) -> Option<Vec<FollowUp>> {
        let g = self.cache.read();
        g.followups
            .as_ref()
            .filter(|(t, _)| t.elapsed() < self.ttl)
            .map(|(_, v)| v.clone())
    }
}

#[async_trait]
impl ProjectTracker for FsProjectTracker {
    async fn phases(&self) -> Result<Vec<Phase>, TrackerError> {
        if let Some(v) = self.phases_fresh() {
            return Ok(v);
        }
        let parsed = self.read_phases_uncached()?;
        self.cache.write().phases = Some((Instant::now(), parsed.clone()));
        Ok(parsed)
    }

    async fn followups(&self) -> Result<Vec<FollowUp>, TrackerError> {
        if let Some(v) = self.followups_fresh() {
            return Ok(v);
        }
        let parsed = self.read_followups_uncached()?;
        self.cache.write().followups = Some((Instant::now(), parsed.clone()));
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_errors_when_phases_missing() {
        let dir = tempdir();
        match FsProjectTracker::open(dir.path()) {
            Err(TrackerError::NotTracked(_)) => {}
            other => panic!("expected NotTracked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_uses_cache_after_first_call() {
        let dir = tempdir();
        std::fs::write(
            dir.path().join("PHASES.md"),
            "## Phase 1 — X\n\n#### 1.1 — A   ✅\n",
        )
        .unwrap();
        let t = FsProjectTracker::open(dir.path()).unwrap();
        let a = t.phases().await.unwrap();
        let b = t.phases().await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a.len(), b.len());
    }

    #[tokio::test]
    async fn current_phase_prefers_in_progress() {
        let dir = tempdir();
        std::fs::write(
            dir.path().join("PHASES.md"),
            "## Phase 1 — X\n\n#### 1.1 — Done   ✅\n#### 1.2 — Active   🔄\n#### 1.3 — Pending   ⬜\n",
        )
        .unwrap();
        let t = FsProjectTracker::open(dir.path()).unwrap();
        let cur = t.current_phase().await.unwrap().unwrap();
        assert_eq!(cur.id, "1.2");
    }

    #[tokio::test]
    async fn last_shipped_returns_done_in_reverse_order() {
        let dir = tempdir();
        std::fs::write(
            dir.path().join("PHASES.md"),
            "## Phase 1 — X\n\n#### 1.1 — A   ✅\n#### 1.2 — B   ✅\n#### 1.3 — C   ⬜\n",
        )
        .unwrap();
        let t = FsProjectTracker::open(dir.path()).unwrap();
        let last = t.last_shipped(5).await.unwrap();
        assert_eq!(last.len(), 2);
        assert_eq!(last[0].id, "1.2");
        assert_eq!(last[1].id, "1.1");
    }

    /// Minimal in-process tempdir helper. We avoid pulling `tempfile`
    /// for one helper — the directory naming uses pid+ns to stay
    /// unique under parallel `cargo test`.
    use std::path::Path;

    fn tempdir() -> TempDir {
        let p = std::env::temp_dir().join(format!(
            "nexo-tracker-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
