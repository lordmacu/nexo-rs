//! [`LocalFsSnapshotter`]: filesystem-backed [`MemorySnapshotter`].
//!
//! This file holds the struct + builder. The async trait methods are
//! split across sibling modules in `crate::local_fs` (one file per
//! method) so they can grow independently without ballooning a single
//! `impl` block.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::SnapshotError;
use crate::id::{AgentId, SnapshotId};
use crate::meta::{RestoreReport, SnapshotDiff, SnapshotMeta, VerifyReport};
use crate::path_resolver::{DefaultPathResolver, PathResolver};
use crate::request::{RestoreRequest, SnapshotRequest};
use crate::snapshotter::MemorySnapshotter;
use crate::state_capture::{NoopStateProvider, StateProvider};

use super::lock::AgentLockMap;

/// How long a `snapshot()` or `restore()` waits on the per-agent lock
/// before returning [`SnapshotError::Concurrent`]. Mirrors
/// `memory.snapshot.lock_timeout_secs` from the operator YAML.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(60);

#[allow(dead_code)]
pub struct LocalFsSnapshotter {
    state_root: PathBuf,
    /// Where each agent's `memdir` (the git-backed memory dir) lives.
    /// Resolved as `<memdir_root>/<agent_id>` for now; richer routing
    /// (per-tenant memdir, per-binding memdir) lands when boot wiring
    /// in `src/main.rs` decides the layout.
    memdir_root: PathBuf,
    /// Where each agent's SQLite stores live (long_term, vector,
    /// concepts, compactions). Same `<sqlite_root>/<agent_id>/...`
    /// convention as the memory crate.
    sqlite_root: PathBuf,
    state_provider: Arc<dyn StateProvider>,
    /// Strategy for `(agent_id, tenant) → (memdir, sqlite_dir)`.
    /// Defaults to a `DefaultPathResolver` over `memdir_root` /
    /// `sqlite_root`; boot wire injects a richer resolver for
    /// multi-tenant SaaS deployments.
    path_resolver: Arc<dyn PathResolver>,
    locks: AgentLockMap,
    lock_timeout: Duration,
}

impl LocalFsSnapshotter {
    pub fn builder() -> LocalFsSnapshotterBuilder {
        LocalFsSnapshotterBuilder::default()
    }

    // The accessors below are consumed by sibling modules (snapshot,
    // verify, list, …). `#[allow(dead_code)]` is kept while the
    // surface is still expanding (encryption + redaction layers
    // tie in next).
    #[allow(dead_code)]
    pub(crate) fn state_root(&self) -> &Path {
        &self.state_root
    }

    #[allow(dead_code)]
    pub(crate) fn memdir_root(&self) -> &Path {
        &self.memdir_root
    }

    #[allow(dead_code)]
    pub(crate) fn sqlite_root(&self) -> &Path {
        &self.sqlite_root
    }

    #[allow(dead_code)]
    pub(crate) fn state_provider(&self) -> &Arc<dyn StateProvider> {
        &self.state_provider
    }

    #[allow(dead_code)]
    pub(crate) fn locks(&self) -> &AgentLockMap {
        &self.locks
    }

    #[allow(dead_code)]
    pub(crate) fn lock_timeout(&self) -> Duration {
        self.lock_timeout
    }

    pub(crate) fn path_resolver(&self) -> &Arc<dyn PathResolver> {
        &self.path_resolver
    }
}

#[derive(Default)]
pub struct LocalFsSnapshotterBuilder {
    state_root: Option<PathBuf>,
    memdir_root: Option<PathBuf>,
    sqlite_root: Option<PathBuf>,
    state_provider: Option<Arc<dyn StateProvider>>,
    lock_timeout: Option<Duration>,
    path_resolver: Option<Arc<dyn PathResolver>>,
}

impl LocalFsSnapshotterBuilder {
    pub fn state_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_root = Some(path.into());
        self
    }

    pub fn memdir_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.memdir_root = Some(path.into());
        self
    }

    pub fn sqlite_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.sqlite_root = Some(path.into());
        self
    }

    pub fn state_provider(mut self, provider: Arc<dyn StateProvider>) -> Self {
        self.state_provider = Some(provider);
        self
    }

    pub fn lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = Some(timeout);
        self
    }

    /// Inject a `PathResolver` so the snapshotter resolves per-agent
    /// memdir / sqlite paths through the operator-supplied strategy.
    /// When omitted, falls back to a `DefaultPathResolver` over
    /// `memdir_root` / `sqlite_root`.
    pub fn path_resolver(mut self, resolver: Arc<dyn PathResolver>) -> Self {
        self.path_resolver = Some(resolver);
        self
    }

    pub fn build(self) -> Result<LocalFsSnapshotter, SnapshotError> {
        let state_root = self.state_root.ok_or_else(|| {
            SnapshotError::RestoreRefused("LocalFsSnapshotter requires state_root".into())
        })?;
        let memdir_root = self.memdir_root.unwrap_or_else(|| state_root.clone());
        let sqlite_root = self.sqlite_root.unwrap_or_else(|| state_root.clone());
        let state_provider = self
            .state_provider
            .unwrap_or_else(|| Arc::new(NoopStateProvider) as Arc<dyn StateProvider>);
        let lock_timeout = self.lock_timeout.unwrap_or(DEFAULT_LOCK_TIMEOUT);
        let path_resolver = self.path_resolver.unwrap_or_else(|| {
            Arc::new(DefaultPathResolver::new(
                memdir_root.clone(),
                sqlite_root.clone(),
            )) as Arc<dyn PathResolver>
        });
        Ok(LocalFsSnapshotter {
            state_root,
            memdir_root,
            sqlite_root,
            state_provider,
            path_resolver,
            locks: AgentLockMap::new(),
            lock_timeout,
        })
    }
}

// Each trait method delegates to a sibling module so the impl block
// stays a routing layer. Bodies live in `snapshot.rs`, `restore.rs`,
// `verify.rs`, `list.rs`, `diff.rs`, `delete.rs`, `export.rs`.
#[async_trait]
impl MemorySnapshotter for LocalFsSnapshotter {
    async fn snapshot(&self, req: SnapshotRequest) -> Result<SnapshotMeta, SnapshotError> {
        super::snapshot::run_snapshot(self, req).await
    }

    async fn restore(&self, req: RestoreRequest) -> Result<RestoreReport, SnapshotError> {
        super::restore::run_restore(self, req).await
    }

    async fn list(
        &self,
        agent_id: &AgentId,
        tenant: &str,
    ) -> Result<Vec<SnapshotMeta>, SnapshotError> {
        super::list::run_list(self, agent_id, tenant).await
    }

    async fn diff(
        &self,
        agent_id: &AgentId,
        tenant: &str,
        a: SnapshotId,
        b: SnapshotId,
    ) -> Result<SnapshotDiff, SnapshotError> {
        super::diff::run_diff(self, agent_id, tenant, a, b).await
    }

    async fn verify(&self, bundle: &Path) -> Result<VerifyReport, SnapshotError> {
        super::verify::run_verify(bundle).await
    }

    async fn delete(
        &self,
        agent_id: &AgentId,
        tenant: &str,
        id: SnapshotId,
    ) -> Result<(), SnapshotError> {
        super::delete::run_delete(self, agent_id, tenant, id).await
    }

    async fn export(
        &self,
        agent_id: &AgentId,
        tenant: &str,
        id: SnapshotId,
        target: &Path,
    ) -> Result<PathBuf, SnapshotError> {
        super::export::run_export(self, agent_id, tenant, id, target).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_requires_state_root() {
        match LocalFsSnapshotter::builder().build() {
            Ok(_) => panic!("expected error"),
            Err(e) => assert!(format!("{e}").contains("state_root")),
        }
    }

    #[test]
    fn builder_defaults_memdir_and_sqlite_to_state_root() {
        let s = LocalFsSnapshotter::builder()
            .state_root("/tmp/x")
            .build()
            .unwrap();
        assert_eq!(s.state_root(), Path::new("/tmp/x"));
        assert_eq!(s.memdir_root(), Path::new("/tmp/x"));
        assert_eq!(s.sqlite_root(), Path::new("/tmp/x"));
        assert_eq!(s.lock_timeout(), DEFAULT_LOCK_TIMEOUT);
    }

    #[test]
    fn builder_overrides_individual_fields() {
        let s = LocalFsSnapshotter::builder()
            .state_root("/tmp/x")
            .memdir_root("/tmp/m")
            .sqlite_root("/tmp/s")
            .lock_timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        assert_eq!(s.memdir_root(), Path::new("/tmp/m"));
        assert_eq!(s.sqlite_root(), Path::new("/tmp/s"));
        assert_eq!(s.lock_timeout(), Duration::from_secs(5));
    }

    // All trait methods now have real bodies; behavior is exercised in
    // each sibling module's `tests` (snapshot, restore, verify, …).

    #[tokio::test]
    async fn dyn_local_fs_snapshotter_can_be_held_as_arc() {
        let tmp_arc = tempfile::tempdir().unwrap();
        let s: Arc<dyn MemorySnapshotter> = Arc::new(
            LocalFsSnapshotter::builder()
                .state_root(tmp_arc.path())
                .build()
                .unwrap(),
        );
        // list() returns Ok([]) when the snapshots dir is empty.
        let metas = s.list(&"ana".into(), "default").await.unwrap();
        assert!(metas.is_empty());
    }

    #[test]
    fn state_provider_defaults_to_noop() {
        let s = LocalFsSnapshotter::builder()
            .state_root("/tmp/x")
            .build()
            .unwrap();
        // We only check the Arc is populated; the trait method behavior
        // is exercised in `state_capture` tests.
        assert!(Arc::strong_count(s.state_provider()) >= 1);
    }
}
