//! Phase 77.3 — PostCompactCleanup.
//!
//! Called after compact summary persistence. Phase 77.5 adds memory
//! extraction support.

use std::path::PathBuf;
use std::sync::Arc;

use crate::extract_memories::ExtractMemories;

/// Runs post-compact housekeeping. When constructed with an
/// `ExtractMemories` instance, also triggers memory extraction.
pub struct PostCompactCleanup {
    extract_memories: Option<Arc<ExtractMemories>>,
    memory_dir: Option<PathBuf>,
}

impl PostCompactCleanup {
    pub fn new() -> Self {
        Self {
            extract_memories: None,
            memory_dir: None,
        }
    }

    /// Attach memory extraction so it fires after compact persistence.
    pub fn with_extract_memories(
        mut self,
        extract: Arc<ExtractMemories>,
        memory_dir: PathBuf,
    ) -> Self {
        self.extract_memories = Some(extract);
        self.memory_dir = Some(memory_dir);
        self
    }

    /// Called after a compact summary is persisted.
    pub async fn run(&self) {
        if let (Some(ref extract), Some(ref memory_dir)) =
            (&self.extract_memories, &self.memory_dir)
        {
            extract.tick();
            // Compact turns don't produce conversation text, so
            // extraction has nothing to work with here. Just tick
            // the counter so the throttle stays accurate.
            let _ = memory_dir;
        }
    }
}

impl Default for PostCompactCleanup {
    fn default() -> Self {
        Self::new()
    }
}
