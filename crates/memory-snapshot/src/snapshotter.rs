//! The main port: anything that can capture, restore, and inspect bundles.
//!
//! The trait stays narrow on purpose. CLI/tool wiring depends on
//! [`MemorySnapshotter`], not on the concrete `LocalFsSnapshotter`, so
//! tests and remote backends (S3, GCS, …) can be plugged in later
//! without touching every consumer.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::SnapshotError;
use crate::id::{AgentId, SnapshotId};
use crate::meta::{RestoreReport, SnapshotDiff, SnapshotMeta, VerifyReport};
use crate::request::{RestoreRequest, SnapshotRequest};

#[async_trait]
pub trait MemorySnapshotter: Send + Sync + 'static {
    async fn snapshot(&self, req: SnapshotRequest) -> Result<SnapshotMeta, SnapshotError>;

    async fn restore(&self, req: RestoreRequest) -> Result<RestoreReport, SnapshotError>;

    async fn list(
        &self,
        agent_id: &AgentId,
        tenant: &str,
    ) -> Result<Vec<SnapshotMeta>, SnapshotError>;

    async fn diff(
        &self,
        agent_id: &AgentId,
        tenant: &str,
        a: SnapshotId,
        b: SnapshotId,
    ) -> Result<SnapshotDiff, SnapshotError>;

    async fn verify(&self, bundle: &Path) -> Result<VerifyReport, SnapshotError>;

    async fn delete(
        &self,
        agent_id: &AgentId,
        tenant: &str,
        id: SnapshotId,
    ) -> Result<(), SnapshotError>;

    async fn export(
        &self,
        agent_id: &AgentId,
        tenant: &str,
        id: SnapshotId,
        target: &Path,
    ) -> Result<std::path::PathBuf, SnapshotError>;
}

// Compile-time proof that the trait stays object-safe so consumers can
// hold `Arc<dyn MemorySnapshotter>` without lifetimes leaking. Pattern
// mirrors `nexo-core::agent::plugin_host::NexoPlugin` (Phase 81.2).
#[allow(dead_code)]
static _OBJECT_SAFE_CHECK: std::sync::OnceLock<Arc<dyn MemorySnapshotter>> =
    std::sync::OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{
        GitDiffSummary, RestoreReport, SnapshotDiff, SnapshotMeta, SqliteDiffSummary,
        StateDiffSummary, VerifyReport,
    };

    struct DummySnapshotter;

    #[async_trait]
    impl MemorySnapshotter for DummySnapshotter {
        async fn snapshot(&self, _req: SnapshotRequest) -> Result<SnapshotMeta, SnapshotError> {
            Err(SnapshotError::UnknownAgent("dummy".into()))
        }

        async fn restore(&self, _req: RestoreRequest) -> Result<RestoreReport, SnapshotError> {
            Err(SnapshotError::RestoreRefused("dummy".into()))
        }

        async fn list(
            &self,
            _agent_id: &AgentId,
            _tenant: &str,
        ) -> Result<Vec<SnapshotMeta>, SnapshotError> {
            Ok(vec![])
        }

        async fn diff(
            &self,
            _agent_id: &AgentId,
            _tenant: &str,
            a: SnapshotId,
            b: SnapshotId,
        ) -> Result<SnapshotDiff, SnapshotError> {
            Ok(SnapshotDiff {
                a,
                b,
                git_summary: GitDiffSummary {
                    commits_between: 0,
                    files_changed: 0,
                    insertions: 0,
                    deletions: 0,
                },
                sqlite_summary: SqliteDiffSummary {
                    long_term_rows_added: 0,
                    long_term_rows_removed: 0,
                    vector_rows_added: 0,
                    vector_rows_removed: 0,
                    concepts_rows_added: 0,
                    concepts_rows_removed: 0,
                    compactions_added: 0,
                },
                state_summary: StateDiffSummary {
                    extract_cursor_changed: false,
                    last_dream_run_changed: false,
                },
            })
        }

        async fn verify(&self, bundle: &Path) -> Result<VerifyReport, SnapshotError> {
            Ok(VerifyReport {
                bundle: bundle.to_path_buf(),
                manifest_ok: true,
                bundle_sha256_ok: true,
                per_artifact_ok: true,
                schema_versions: crate::manifest::SchemaVersions::CURRENT,
                age_protected: false,
            })
        }

        async fn delete(
            &self,
            _agent_id: &AgentId,
            _tenant: &str,
            _id: SnapshotId,
        ) -> Result<(), SnapshotError> {
            Ok(())
        }

        async fn export(
            &self,
            _agent_id: &AgentId,
            _tenant: &str,
            _id: SnapshotId,
            target: &Path,
        ) -> Result<std::path::PathBuf, SnapshotError> {
            Ok(target.to_path_buf())
        }
    }

    #[tokio::test]
    async fn dyn_snapshotter_can_be_held_as_arc() {
        let s: Arc<dyn MemorySnapshotter> = Arc::new(DummySnapshotter);
        let listed = s.list(&"ana".to_string(), "default").await.unwrap();
        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn dummy_snapshot_propagates_unknown_agent() {
        let s = DummySnapshotter;
        let err = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap_err();
        assert!(matches!(err, SnapshotError::UnknownAgent(_)));
    }
}
