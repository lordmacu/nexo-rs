//! Adapters that bridge `nexo-driver-types` traits to the snapshot
//! subsystem.
//!
//! - [`PreDreamSnapshotAdapter`] turns any
//!   [`crate::snapshotter::MemorySnapshotter`] into a
//!   [`nexo_driver_types::PreDreamSnapshotHook`] so `nexo-dream`'s
//!   `AutoDreamRunner` can fire a snapshot before every fork-pass
//!   without depending on this crate directly.
//! - [`MemoryMutationPublisher`] takes a
//!   [`crate::events::EventPublisher`] and exposes it as a
//!   [`nexo_driver_types::MemoryMutationHook`] so the memory crate can
//!   stream every write into the
//!   `nexo.memory.mutated.<agent_id>` NATS subject without depending
//!   on broker details.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use nexo_driver_types::{
    MemoryMutationHook, MemoryMutationOp as DtOp, MemoryMutationScope as DtScope,
    PreDreamSnapshotHook,
};

use crate::events::{EventPublisher, MutationEvent, MutationOp, MutationScope};
use crate::request::SnapshotRequest;
use crate::snapshotter::MemorySnapshotter;

/// Boot-time adapter: take any `Arc<dyn MemorySnapshotter>` and
/// expose it as a `PreDreamSnapshotHook` ready to plug into
/// `AutoDreamRunner::with_pre_dream_snapshot`.
pub struct PreDreamSnapshotAdapter {
    snapshotter: Arc<dyn MemorySnapshotter>,
}

impl PreDreamSnapshotAdapter {
    pub fn new(snapshotter: Arc<dyn MemorySnapshotter>) -> Self {
        Self { snapshotter }
    }

    pub fn into_arc(self) -> Arc<dyn PreDreamSnapshotHook> {
        Arc::new(self)
    }
}

/// Bridge a [`crate::events::EventPublisher`] to the
/// [`nexo_driver_types::MemoryMutationHook`] surface the memory crate
/// fires on every write. Best-effort: a publish failure logs in the
/// publisher impl and never poisons the writer's transaction.
pub struct MemoryMutationPublisher {
    publisher: Arc<dyn EventPublisher>,
}

impl MemoryMutationPublisher {
    pub fn new(publisher: Arc<dyn EventPublisher>) -> Self {
        Self { publisher }
    }

    pub fn into_arc(self) -> Arc<dyn MemoryMutationHook> {
        Arc::new(self)
    }
}

fn map_scope(s: DtScope) -> MutationScope {
    match s {
        DtScope::SqliteLongTerm => MutationScope::SqliteLongTerm,
        DtScope::SqliteVector => MutationScope::SqliteVector,
        DtScope::SqliteConcepts => MutationScope::SqliteConcepts,
        DtScope::SqliteCompactions => MutationScope::SqliteCompactions,
        DtScope::Git => MutationScope::Git,
        DtScope::MemoryFile => MutationScope::MemoryFile,
    }
}

fn map_op(o: DtOp) -> MutationOp {
    match o {
        DtOp::Insert => MutationOp::Insert,
        DtOp::Update => MutationOp::Update,
        DtOp::Delete => MutationOp::Delete,
    }
}

#[async_trait]
impl MemoryMutationHook for MemoryMutationPublisher {
    async fn on_mutation(
        &self,
        agent_id: &str,
        tenant: &str,
        scope: DtScope,
        op: DtOp,
        key: &str,
    ) {
        let event = MutationEvent {
            agent_id: agent_id.to_string(),
            tenant: tenant.to_string(),
            scope: map_scope(scope),
            op: map_op(op),
            key: key.to_string(),
            old_hash: None,
            new_hash: None,
            ts_ms: Utc::now().timestamp_millis(),
        };
        self.publisher.publish_mutation(event).await;
    }
}

#[async_trait]
impl PreDreamSnapshotHook for PreDreamSnapshotAdapter {
    async fn snapshot_before_dream(
        &self,
        agent_id: &str,
        tenant: &str,
        run_id: &str,
    ) -> Result<(), String> {
        let req = SnapshotRequest {
            agent_id: agent_id.to_string(),
            tenant: tenant.to_string(),
            label: Some(format!("auto:pre-dream-{run_id}")),
            // Pre-dream snapshots are private operator-side bundles;
            // we keep the redaction default off so the operator gets
            // a verbatim view if they later compare what changed
            // during the dream. Operators who want redaction can
            // wrap this adapter in their own that flips the flag.
            redact_secrets: false,
            encrypt: None,
            created_by: "auto-pre-dream".into(),
        };
        match self.snapshotter.snapshot(req).await {
            Ok(_meta) => Ok(()),
            Err(e) => Err(format!("{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SnapshotError;
    use crate::id::{AgentId, SnapshotId};
    use crate::manifest::SchemaVersions;
    use crate::meta::{
        GitDiffSummary, RestoreReport, SnapshotDiff, SnapshotMeta, SqliteDiffSummary,
        StateDiffSummary, VerifyReport,
    };
    use crate::request::RestoreRequest;

    struct CapturingSnapshotter {
        last_label: tokio::sync::Mutex<Option<String>>,
    }

    #[async_trait]
    impl MemorySnapshotter for CapturingSnapshotter {
        async fn snapshot(
            &self,
            req: SnapshotRequest,
        ) -> Result<SnapshotMeta, SnapshotError> {
            *self.last_label.lock().await = req.label.clone();
            Ok(SnapshotMeta {
                id: SnapshotId::new(),
                agent_id: req.agent_id,
                tenant: req.tenant,
                label: req.label,
                created_at_ms: 0,
                bundle_path: std::path::PathBuf::from("/tmp/x.tar.zst"),
                bundle_size_bytes: 0,
                bundle_sha256: String::new(),
                git_oid: None,
                schema_versions: SchemaVersions::CURRENT,
                encrypted: false,
                redactions_applied: false,
            })
        }
        async fn restore(
            &self,
            _req: RestoreRequest,
        ) -> Result<RestoreReport, SnapshotError> {
            unimplemented!()
        }
        async fn list(
            &self,
            _agent_id: &AgentId,
            _tenant: &str,
        ) -> Result<Vec<SnapshotMeta>, SnapshotError> {
            Ok(Vec::new())
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
        async fn verify(&self, bundle: &std::path::Path) -> Result<VerifyReport, SnapshotError> {
            Ok(VerifyReport {
                bundle: bundle.to_path_buf(),
                manifest_ok: true,
                bundle_sha256_ok: true,
                per_artifact_ok: true,
                schema_versions: SchemaVersions::CURRENT,
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
            target: &std::path::Path,
        ) -> Result<std::path::PathBuf, SnapshotError> {
            Ok(target.to_path_buf())
        }
    }

    #[tokio::test]
    async fn adapter_dispatches_snapshot_with_run_id_label() {
        let inner = Arc::new(CapturingSnapshotter {
            last_label: tokio::sync::Mutex::new(None),
        });
        let inner_dyn: Arc<dyn MemorySnapshotter> = inner.clone();
        let adapter = PreDreamSnapshotAdapter::new(inner_dyn);
        adapter
            .snapshot_before_dream("ana", "default", "run-42")
            .await
            .unwrap();
        let label = inner.last_label.lock().await.clone();
        assert_eq!(label, Some("auto:pre-dream-run-42".to_string()));
    }

    #[tokio::test]
    async fn adapter_translates_snapshot_error_to_string() {
        struct FailingSnapshotter;
        #[async_trait]
        impl MemorySnapshotter for FailingSnapshotter {
            async fn snapshot(
                &self,
                _req: SnapshotRequest,
            ) -> Result<SnapshotMeta, SnapshotError> {
                Err(SnapshotError::Concurrent("ana".into()))
            }
            async fn restore(
                &self,
                _req: RestoreRequest,
            ) -> Result<RestoreReport, SnapshotError> {
                unimplemented!()
            }
            async fn list(
                &self,
                _agent_id: &AgentId,
                _tenant: &str,
            ) -> Result<Vec<SnapshotMeta>, SnapshotError> {
                Ok(Vec::new())
            }
            async fn diff(
                &self,
                _agent_id: &AgentId,
                _tenant: &str,
                _a: SnapshotId,
                _b: SnapshotId,
            ) -> Result<SnapshotDiff, SnapshotError> {
                unimplemented!()
            }
            async fn verify(
                &self,
                _bundle: &std::path::Path,
            ) -> Result<VerifyReport, SnapshotError> {
                unimplemented!()
            }
            async fn delete(
                &self,
                _agent_id: &AgentId,
                _tenant: &str,
                _id: SnapshotId,
            ) -> Result<(), SnapshotError> {
                unimplemented!()
            }
            async fn export(
                &self,
                _agent_id: &AgentId,
                _tenant: &str,
                _id: SnapshotId,
                _target: &std::path::Path,
            ) -> Result<std::path::PathBuf, SnapshotError> {
                unimplemented!()
            }
        }
        let inner: Arc<dyn MemorySnapshotter> = Arc::new(FailingSnapshotter);
        let adapter = PreDreamSnapshotAdapter::new(inner);
        let err = adapter
            .snapshot_before_dream("ana", "default", "run-1")
            .await
            .unwrap_err();
        assert!(err.contains("ana"));
    }

    #[tokio::test]
    async fn mutation_publisher_translates_scope_and_op() {
        use crate::events::{EventPublisher, NoopPublisher};
        use std::sync::Mutex;

        #[derive(Default)]
        struct Recorder {
            events: Mutex<Vec<MutationEvent>>,
        }
        #[async_trait]
        impl EventPublisher for Recorder {
            async fn publish_lifecycle(&self, _event: crate::events::LifecycleEvent) {}
            async fn publish_mutation(&self, event: MutationEvent) {
                self.events.lock().unwrap().push(event);
            }
        }
        let rec = Arc::new(Recorder::default());
        let publisher: Arc<dyn EventPublisher> = rec.clone();
        let hook = MemoryMutationPublisher::new(publisher);

        hook.on_mutation(
            "ana",
            "default",
            DtScope::SqliteLongTerm,
            DtOp::Insert,
            "row-1",
        )
        .await;
        hook.on_mutation("ana", "default", DtScope::Git, DtOp::Update, "deadbeef")
            .await;

        let evs = rec.events.lock().unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].agent_id, "ana");
        assert_eq!(evs[0].tenant, "default");
        assert!(matches!(evs[0].scope, MutationScope::SqliteLongTerm));
        assert!(matches!(evs[0].op, MutationOp::Insert));
        assert_eq!(evs[0].key, "row-1");
        assert!(matches!(evs[1].scope, MutationScope::Git));
        assert!(matches!(evs[1].op, MutationOp::Update));

        // Smoke: NoopPublisher works as a sink too.
        let noop_hook = MemoryMutationPublisher::new(Arc::new(NoopPublisher) as _);
        noop_hook
            .on_mutation("ana", "default", DtScope::Git, DtOp::Delete, "x")
            .await;
    }

    #[test]
    fn into_arc_yields_dyn_pre_dream_snapshot_hook() {
        struct Stub;
        #[async_trait]
        impl MemorySnapshotter for Stub {
            async fn snapshot(
                &self,
                _req: SnapshotRequest,
            ) -> Result<SnapshotMeta, SnapshotError> {
                unimplemented!()
            }
            async fn restore(
                &self,
                _req: RestoreRequest,
            ) -> Result<RestoreReport, SnapshotError> {
                unimplemented!()
            }
            async fn list(
                &self,
                _agent_id: &AgentId,
                _tenant: &str,
            ) -> Result<Vec<SnapshotMeta>, SnapshotError> {
                unimplemented!()
            }
            async fn diff(
                &self,
                _agent_id: &AgentId,
                _tenant: &str,
                _a: SnapshotId,
                _b: SnapshotId,
            ) -> Result<SnapshotDiff, SnapshotError> {
                unimplemented!()
            }
            async fn verify(
                &self,
                _bundle: &std::path::Path,
            ) -> Result<VerifyReport, SnapshotError> {
                unimplemented!()
            }
            async fn delete(
                &self,
                _agent_id: &AgentId,
                _tenant: &str,
                _id: SnapshotId,
            ) -> Result<(), SnapshotError> {
                unimplemented!()
            }
            async fn export(
                &self,
                _agent_id: &AgentId,
                _tenant: &str,
                _id: SnapshotId,
                _target: &std::path::Path,
            ) -> Result<std::path::PathBuf, SnapshotError> {
                unimplemented!()
            }
        }
        let inner: Arc<dyn MemorySnapshotter> = Arc::new(Stub);
        let adapter = PreDreamSnapshotAdapter::new(inner);
        let _hook: Arc<dyn PreDreamSnapshotHook> = adapter.into_arc();
    }
}
