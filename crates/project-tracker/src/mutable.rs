//! Hot-swappable wrapper around `ProjectTracker` so an agent can
//! point at a new workspace mid-conversation (Cody flow:
//! "find /path/X", "create folder Y and scaffold").
//!
//! Internally holds an `ArcSwap<Arc<dyn ProjectTracker>>` so reads
//! never block writers; switching is one Arc swap.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use parking_lot::RwLock;

use crate::tracker::{FsProjectTracker, ProjectTracker};
use crate::types::{FollowUp, Phase, SubPhase, TrackerError};

pub struct MutableTracker {
    inner: ArcSwap<Box<dyn ProjectTracker>>,
    /// Last `switch_to` path so callers can introspect the active
    /// workspace (Cody's `current_workspace` reply).
    root: RwLock<PathBuf>,
}

impl MutableTracker {
    /// Wrap an existing tracker; `root` is the path it points at,
    /// used only for display.
    pub fn new(initial: Box<dyn ProjectTracker>, root: PathBuf) -> Self {
        Self {
            inner: ArcSwap::from_pointee(initial),
            root: RwLock::new(root),
        }
    }

    /// Convenience: open an `FsProjectTracker` and wrap it.
    pub fn open_fs(root: impl Into<PathBuf>) -> Result<Self, TrackerError> {
        let root = root.into();
        let fs = FsProjectTracker::open(&root)?;
        Ok(Self::new(Box::new(fs), root))
    }

    /// Switch to a new path. The old tracker is dropped after the
    /// next read finishes (ArcSwap semantics). Returns the previous
    /// path for telemetry.
    pub fn switch_to(&self, new_root: impl Into<PathBuf>) -> Result<PathBuf, TrackerError> {
        let new_root = new_root.into();
        let fs = FsProjectTracker::open(&new_root)?;
        self.inner.store(Arc::new(Box::new(fs)));
        let prev = std::mem::replace(&mut *self.root.write(), new_root);
        Ok(prev)
    }

    pub fn root(&self) -> PathBuf {
        self.root.read().clone()
    }

    fn load(&self) -> Arc<Box<dyn ProjectTracker>> {
        self.inner.load_full()
    }
}

impl std::fmt::Debug for MutableTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MutableTracker")
            .field("root", &*self.root.read())
            .finish()
    }
}

#[async_trait]
impl ProjectTracker for MutableTracker {
    fn root(&self) -> Option<PathBuf> {
        Some(self.root.read().clone())
    }

    async fn phases(&self) -> Result<Vec<Phase>, TrackerError> {
        self.load().phases().await
    }
    async fn followups(&self) -> Result<Vec<FollowUp>, TrackerError> {
        self.load().followups().await
    }
    async fn current_phase(&self) -> Result<Option<SubPhase>, TrackerError> {
        self.load().current_phase().await
    }
    async fn next_phase(&self) -> Result<Option<SubPhase>, TrackerError> {
        self.load().next_phase().await
    }
    async fn last_shipped(&self, n: usize) -> Result<Vec<SubPhase>, TrackerError> {
        self.load().last_shipped(n).await
    }
    async fn phase_detail(&self, id: &str) -> Result<Option<SubPhase>, TrackerError> {
        self.load().phase_detail(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PhaseStatus;
    use std::path::Path;

    fn write_phases(dir: &Path, body: &str) {
        std::fs::write(dir.join("PHASES.md"), body).unwrap();
    }

    #[tokio::test]
    async fn switch_to_changes_active_workspace() {
        let tmp = std::env::temp_dir().join(format!(
            "nexo-mut-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let a = tmp.join("a");
        let b = tmp.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        write_phases(&a, "## Phase 1 — A\n\n#### 1.1 — Done   ✅\n");
        write_phases(&b, "## Phase 2 — B\n\n#### 2.1 — Pending   ⬜\n");

        let mt = MutableTracker::open_fs(&a).unwrap();
        let cur = mt.current_phase().await.unwrap();
        // a has 1.1 Done → no Pending → current_phase returns None.
        assert!(cur.is_none());

        mt.switch_to(&b).unwrap();
        let cur2 = mt.current_phase().await.unwrap().unwrap();
        assert_eq!(cur2.id, "2.1");
        assert_eq!(cur2.status, PhaseStatus::Pending);
        assert_eq!(mt.root(), b);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
