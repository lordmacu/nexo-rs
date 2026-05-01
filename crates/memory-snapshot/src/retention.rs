//! Retention sweeper + orphan staging cleanup.
//!
//! Two responsibilities, both run by [`RetentionWorker`]:
//!
//! 1. **Periodic GC of old bundles** — apply `keep_count` and
//!    `max_age_days` to every `(tenant, agent_id)` known under the
//!    snapshotter's state root. Bundles selected for deletion go
//!    through [`crate::snapshotter::MemorySnapshotter::delete`], which
//!    refuses to drop the agent's last remaining snapshot.
//! 2. **Orphan staging sweep** — fired once at worker startup *and*
//!    on every retention tick. Removes `.staging-*` and
//!    `.restore-staging-*` directories left behind by a hard process
//!    kill (`SIGKILL`, OOM, panic outside the `tokio` runtime). The
//!    snapshot/restore happy path always cleans these on `Drop`; the
//!    sweep is the safety net.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::SnapshotError;
use crate::id::AgentId;
use crate::snapshotter::MemorySnapshotter;
use crate::tenant_path::{snapshots_dir, validate_agent_id, validate_tenant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// Hard cap on the number of bundles per agent. Once exceeded, the
    /// oldest bundles are deleted until the cap is met or only one
    /// snapshot remains (whichever comes first).
    pub keep_count: u32,
    /// Bundles older than this are deleted regardless of `keep_count`,
    /// again subject to the "never drop the last snapshot" rule.
    pub max_age_days: u32,
    /// Interval between GC ticks. The worker also runs once
    /// immediately on spawn.
    pub gc_interval_secs: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            keep_count: 30,
            max_age_days: 90,
            gc_interval_secs: 3600,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RetentionTickReport {
    pub bundles_deleted: u32,
    pub orphan_staging_dirs_removed: u32,
    pub agents_visited: u32,
    pub errors: u32,
}

/// Background task driver. Holds an `Arc<dyn MemorySnapshotter>` so the
/// real backend (or a test double) is plugged in once at boot.
pub struct RetentionWorker<S: MemorySnapshotter + ?Sized = dyn MemorySnapshotter> {
    snapshotter: Arc<S>,
    state_root: std::path::PathBuf,
    config: RetentionConfig,
}

impl RetentionWorker {
    pub fn new(
        snapshotter: Arc<dyn MemorySnapshotter>,
        state_root: impl Into<std::path::PathBuf>,
        config: RetentionConfig,
    ) -> Self {
        Self {
            snapshotter,
            state_root: state_root.into(),
            config,
        }
    }

    /// Spawn the periodic loop. Cancelling the token stops the loop on
    /// the next iteration; in-flight deletes are not interrupted.
    pub fn spawn(self, cancel: CancellationToken) -> JoinHandle<()> {
        let interval = Duration::from_secs(self.config.gc_interval_secs.max(1));
        tokio::spawn(async move {
            // Initial sweep at startup so dirty staging from a previous
            // crash does not survive into normal operation.
            let _ = self.tick().await;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(interval) => {
                        match self.tick().await {
                            Ok(report) => tracing::debug!(?report, "retention tick complete"),
                            Err(e) => tracing::warn!(error = %e, "retention tick failed"),
                        }
                    }
                }
            }
        })
    }

    /// One pass: GC bundles + sweep orphan staging. Public so callers
    /// can drive it from a CLI subcommand or a test.
    pub async fn tick(&self) -> Result<RetentionTickReport, SnapshotError> {
        let mut report = RetentionTickReport::default();

        let agents = scan_agents(&self.state_root).unwrap_or_default();
        for (tenant, agent_id) in agents {
            report.agents_visited += 1;

            // Sweep orphan staging dirs first so a half-written bundle
            // never gets confused with a real one by `list()`.
            match sweep_orphan_staging(&self.state_root, &tenant, &agent_id) {
                Ok(n) => report.orphan_staging_dirs_removed += n,
                Err(e) => {
                    report.errors += 1;
                    tracing::warn!(
                        tenant = %tenant,
                        agent_id = %agent_id,
                        error = %e,
                        "orphan staging sweep failed"
                    );
                }
            }

            match self
                .snapshotter
                .list(&agent_id, &tenant)
                .await
            {
                Ok(metas) => {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    let max_age_ms = (self.config.max_age_days as i64) * 86_400_000;

                    let mut over_count = if metas.len() as u32 > self.config.keep_count {
                        metas.len() as u32 - self.config.keep_count
                    } else {
                        0
                    };

                    // Iterate oldest-first by reversing the list-desc order.
                    let mut candidates: Vec<_> = metas.iter().rev().collect();
                    while !candidates.is_empty() {
                        let oldest = candidates[0];
                        let too_old = max_age_ms > 0
                            && now_ms.saturating_sub(oldest.created_at_ms) > max_age_ms;
                        if over_count == 0 && !too_old {
                            break;
                        }
                        match self
                            .snapshotter
                            .delete(&agent_id, &tenant, oldest.id)
                            .await
                        {
                            Ok(()) => {
                                report.bundles_deleted += 1;
                                if over_count > 0 {
                                    over_count -= 1;
                                }
                            }
                            Err(SnapshotError::Retention(_)) => {
                                // Hit the "last snapshot" floor — stop
                                // pruning this agent.
                                break;
                            }
                            Err(e) => {
                                report.errors += 1;
                                tracing::warn!(
                                    tenant = %tenant,
                                    agent_id = %agent_id,
                                    error = %e,
                                    "delete during retention failed"
                                );
                                break;
                            }
                        }
                        candidates.remove(0);
                    }
                }
                Err(e) => {
                    report.errors += 1;
                    tracing::warn!(
                        tenant = %tenant,
                        agent_id = %agent_id,
                        error = %e,
                        "list during retention failed"
                    );
                }
            }
        }
        Ok(report)
    }
}

/// Walk `<state_root>/tenants/<tenant>/snapshots/<agent_id>/` and
/// return every `(tenant, agent_id)` pair found. Tenants and agent ids
/// that fail validation are skipped silently — the worker should not
/// crash on a hostile or partially renamed directory.
pub fn scan_agents(state_root: &Path) -> std::io::Result<Vec<(String, AgentId)>> {
    let tenants_root = state_root.join("tenants");
    if !tenants_root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for tenant_entry in fs::read_dir(&tenants_root)? {
        let tenant_entry = tenant_entry?;
        if !tenant_entry.file_type()?.is_dir() {
            continue;
        }
        let Some(tenant_name) = tenant_entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        if validate_tenant(&tenant_name).is_err() {
            continue;
        }
        let agents_root = tenant_entry.path().join("snapshots");
        if !agents_root.exists() {
            continue;
        }
        for agent_entry in fs::read_dir(&agents_root)? {
            let agent_entry = agent_entry?;
            if !agent_entry.file_type()?.is_dir() {
                continue;
            }
            let Some(agent_name) = agent_entry.file_name().to_str().map(|s| s.to_string()) else {
                continue;
            };
            if validate_agent_id(&agent_name).is_err() {
                continue;
            }
            out.push((tenant_name.clone(), agent_name));
        }
    }
    Ok(out)
}

/// Remove every `.staging-*` and `.restore-staging-*` subdirectory
/// inside the snapshots dir for `(tenant, agent_id)`. Returns the
/// number of dirs removed.
pub fn sweep_orphan_staging(
    state_root: &Path,
    tenant: &str,
    agent_id: &str,
) -> Result<u32, SnapshotError> {
    let dir = snapshots_dir(state_root, tenant, agent_id)?;
    if !dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        if name.starts_with(".staging-") || name.starts_with(".restore-staging-") {
            match fs::remove_dir_all(entry.path()) {
                Ok(()) => removed += 1,
                Err(e) => {
                    tracing::warn!(
                        path = %entry.path().display(),
                        error = %e,
                        "removing orphan staging dir failed"
                    );
                }
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_fs::LocalFsSnapshotter;
    use crate::request::SnapshotRequest;
    use git2::{IndexAddOption, Repository, Signature};

    fn seed_memdir(memdir: &Path) {
        fs::create_dir_all(memdir).unwrap();
        let repo = Repository::init(memdir).unwrap();
        fs::write(memdir.join("MEMORY.md"), b"# x\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("operator", "ops@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "seed", &tree, &[])
            .unwrap();
    }

    fn build(state_root: &Path) -> LocalFsSnapshotter {
        LocalFsSnapshotter::builder()
            .state_root(state_root)
            .memdir_root(state_root.join("memdir"))
            .sqlite_root(state_root.join("sqlite"))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn keep_count_prunes_oldest() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Arc::new(build(tmp.path())) as Arc<dyn MemorySnapshotter>;
        seed_memdir(&tmp.path().join("memdir/ana"));

        for _ in 0..5u32 {
            s.snapshot(SnapshotRequest::cli("ana", "default")).await.unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let cfg = RetentionConfig {
            keep_count: 2,
            max_age_days: 0,
            gc_interval_secs: 60,
        };
        let worker = RetentionWorker::new(s.clone(), tmp.path(), cfg);
        let report = worker.tick().await.unwrap();
        assert_eq!(report.bundles_deleted, 3);
        let metas = s.list(&"ana".into(), "default").await.unwrap();
        assert_eq!(metas.len(), 2);
    }

    #[tokio::test]
    async fn never_deletes_last_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Arc::new(build(tmp.path())) as Arc<dyn MemorySnapshotter>;
        seed_memdir(&tmp.path().join("memdir/ana"));

        let only = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();
        // Aggressive policy that would otherwise sweep everything.
        let cfg = RetentionConfig {
            keep_count: 0,
            max_age_days: 0,
            gc_interval_secs: 60,
        };
        let worker = RetentionWorker::new(s.clone(), tmp.path(), cfg);
        let _ = worker.tick().await.unwrap();
        let metas = s.list(&"ana".into(), "default").await.unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].id, only.id);
    }

    #[tokio::test]
    async fn sweeps_orphan_staging_dirs_at_tick_time() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Arc::new(build(tmp.path())) as Arc<dyn MemorySnapshotter>;
        seed_memdir(&tmp.path().join("memdir/ana"));

        let _ = s.snapshot(SnapshotRequest::cli("ana", "default")).await.unwrap();
        // Simulate two crashes leaving staging dirs behind.
        let dir = snapshots_dir(tmp.path(), "default", "ana").unwrap();
        fs::create_dir(dir.join(".staging-deadbeef")).unwrap();
        fs::create_dir(dir.join(".restore-staging-cafef00d")).unwrap();
        fs::write(dir.join(".staging-deadbeef/marker"), b"x").unwrap();

        let worker = RetentionWorker::new(
            s.clone(),
            tmp.path(),
            RetentionConfig {
                keep_count: 30,
                max_age_days: 0,
                gc_interval_secs: 60,
            },
        );
        let report = worker.tick().await.unwrap();
        assert_eq!(report.orphan_staging_dirs_removed, 2);
        assert!(!dir.join(".staging-deadbeef").exists());
        assert!(!dir.join(".restore-staging-cafef00d").exists());
    }

    #[tokio::test]
    async fn scan_agents_walks_tenant_subtree() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Arc::new(build(tmp.path())) as Arc<dyn MemorySnapshotter>;
        seed_memdir(&tmp.path().join("memdir/ana"));
        seed_memdir(&tmp.path().join("memdir/otro"));
        s.snapshot(SnapshotRequest::cli("ana", "default")).await.unwrap();
        s.snapshot(SnapshotRequest::cli("otro", "acme")).await.unwrap();

        let mut found = scan_agents(tmp.path()).unwrap();
        found.sort();
        assert_eq!(
            found,
            vec![
                ("acme".to_string(), "otro".to_string()),
                ("default".to_string(), "ana".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn spawn_runs_initial_sweep_then_stops_on_cancel() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Arc::new(build(tmp.path())) as Arc<dyn MemorySnapshotter>;
        seed_memdir(&tmp.path().join("memdir/ana"));
        let _ = s.snapshot(SnapshotRequest::cli("ana", "default")).await.unwrap();
        // Drop a staging dir to verify the initial sweep removes it.
        let dir = snapshots_dir(tmp.path(), "default", "ana").unwrap();
        fs::create_dir(dir.join(".staging-orphan")).unwrap();

        let worker = RetentionWorker::new(
            s.clone(),
            tmp.path(),
            RetentionConfig {
                keep_count: 30,
                max_age_days: 0,
                gc_interval_secs: 3600, // not reached during this test
            },
        );
        let cancel = CancellationToken::new();
        let handle = worker.spawn(cancel.clone());
        // Allow the initial sweep to finish.
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
        handle.await.unwrap();
        assert!(!dir.join(".staging-orphan").exists());
    }
}
