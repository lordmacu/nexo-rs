//! Wire-shape types and a thin publisher abstraction for memory
//! snapshot lifecycle events + memory mutation events.
//!
//! Two event families:
//!
//! 1. **Lifecycle** — emitted by [`crate::snapshotter::MemorySnapshotter`]
//!    when a bundle is created / restored / deleted, and by
//!    [`crate::retention::RetentionWorker`] after each tick. Subjects
//!    follow `nexo.memory.snapshot.<agent_id>.<kind>`.
//! 2. **Mutation** — emitted by the memory crates (long_term, vector,
//!    concepts, compactions, git memdir) on every write. Subject:
//!    `nexo.memory.mutated.<agent_id>`. Best-effort: a publish error
//!    must never fail the underlying write.
//!
//! The crate stays decoupled from any concrete broker. Boot wiring
//! supplies a [`EventPublisher`] impl that bridges to `async-nats`.
//! Tests use [`NoopPublisher`].

use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;

use crate::id::{AgentId, SnapshotId};
use crate::meta::{RestoreReport, SnapshotMeta};
use crate::retention::RetentionTickReport;

/// Subject template for all lifecycle events. The trailing segment is
/// `created` | `restored` | `deleted` | `gc`.
pub const LIFECYCLE_SUBJECT_PREFIX: &str = "nexo.memory.snapshot";

/// Subject for memory mutation events.
pub const MUTATION_SUBJECT_PREFIX: &str = "nexo.memory.mutated";

/// One per snapshot/restore/delete/gc transition.
///
/// Serialize-only: events flow outward (broker publish, transcript
/// log). Subscribers parse the JSON back through their own consumer
/// types. This keeps the inner reports (`RestoreReport`,
/// `RetentionTickReport`) `Serialize`-only without forcing every
/// downstream type to also derive `Deserialize`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleEvent {
    Created(SnapshotMeta),
    Restored(RestoreReport),
    Deleted {
        agent_id: AgentId,
        tenant: String,
        snapshot_id: SnapshotId,
        ts_ms: i64,
    },
    Gc {
        ts_ms: i64,
        report: RetentionTickReport,
    },
}

impl LifecycleEvent {
    /// Subject the publisher should write to. Includes the agent id so
    /// downstream subscribers can filter cheaply.
    pub fn subject(&self) -> String {
        let (agent_id, kind) = match self {
            LifecycleEvent::Created(m) => (m.agent_id.as_str(), "created"),
            LifecycleEvent::Restored(r) => (r.agent_id.as_str(), "restored"),
            LifecycleEvent::Deleted { agent_id, .. } => (agent_id.as_str(), "deleted"),
            LifecycleEvent::Gc { .. } => ("_all", "gc"),
        };
        format!("{LIFECYCLE_SUBJECT_PREFIX}.{agent_id}.{kind}")
    }
}

/// Scope of a memory mutation. Producers in the memory crate populate
/// this when they emit a [`MutationEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationScope {
    SqliteLongTerm,
    SqliteVector,
    SqliteConcepts,
    SqliteCompactions,
    Git,
    MemoryFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationOp {
    Insert,
    Update,
    Delete,
}

/// One per memory write. Best-effort publish: callers do not block on
/// it, and a publisher error never propagates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationEvent {
    pub agent_id: AgentId,
    pub tenant: String,
    pub scope: MutationScope,
    pub op: MutationOp,
    /// Identifier of the mutated row / file / oid; opaque to consumers
    /// other than as a correlation key.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_hash: Option<String>,
    pub ts_ms: i64,
}

impl MutationEvent {
    pub fn subject(&self) -> String {
        format!("{MUTATION_SUBJECT_PREFIX}.{}", self.agent_id)
    }
}

/// Bridge to the actual broker. Implementations are expected to be
/// best-effort: fire-and-forget, never block the calling write path.
#[async_trait]
pub trait EventPublisher: Send + Sync + 'static {
    async fn publish_lifecycle(&self, event: LifecycleEvent);
    async fn publish_mutation(&self, event: MutationEvent);
}

/// Drops every event silently. Default publisher when no broker is
/// wired; the snapshotter never needs to special-case the absence of a
/// publisher because of this.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopPublisher;

#[async_trait]
impl EventPublisher for NoopPublisher {
    async fn publish_lifecycle(&self, _event: LifecycleEvent) {}
    async fn publish_mutation(&self, _event: MutationEvent) {}
}

/// Test double that records every event published. Counter-style
/// assertions ride on top.
#[derive(Default)]
pub struct RecordingPublisher {
    pub lifecycle: parking_lot_compat::Mutex<Vec<LifecycleEvent>>,
    pub mutation: parking_lot_compat::Mutex<Vec<MutationEvent>>,
}

#[async_trait]
impl EventPublisher for RecordingPublisher {
    async fn publish_lifecycle(&self, event: LifecycleEvent) {
        self.lifecycle.lock().push(event);
    }
    async fn publish_mutation(&self, event: MutationEvent) {
        self.mutation.lock().push(event);
    }
}

/// Local re-export so the recording publisher does not pull
/// `parking_lot` into the crate's public dep graph. We use
/// `std::sync::Mutex` and surface a `lock()` helper so the test code
/// reads the same as the real `parking_lot::Mutex` it would use in a
/// production publisher.
mod parking_lot_compat {
    use std::sync::{LockResult, Mutex as StdMutex, MutexGuard};

    pub struct Mutex<T>(StdMutex<T>);

    impl<T: Default> Default for Mutex<T> {
        fn default() -> Self {
            Self(StdMutex::new(T::default()))
        }
    }

    impl<T> Mutex<T> {
        pub fn lock(&self) -> MutexGuard<'_, T> {
            // Test-only helper; poisoning surfaces as a clear panic.
            tolerate(self.0.lock())
        }
    }

    fn tolerate<T>(r: LockResult<T>) -> T {
        match r {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SnapshotId;
    use crate::manifest::SchemaVersions;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn sample_meta() -> SnapshotMeta {
        SnapshotMeta {
            id: SnapshotId::new(),
            agent_id: "ana".into(),
            tenant: "default".into(),
            label: None,
            created_at_ms: 1_700_000_000_000,
            bundle_path: PathBuf::from("/tmp/x.tar.zst"),
            bundle_size_bytes: 0,
            bundle_sha256: String::new(),
            git_oid: None,
            schema_versions: SchemaVersions::CURRENT,
            encrypted: false,
            redactions_applied: false,
        }
    }

    #[test]
    fn lifecycle_subjects_include_agent_id_and_kind() {
        let m = sample_meta();
        assert_eq!(
            LifecycleEvent::Created(m.clone()).subject(),
            "nexo.memory.snapshot.ana.created"
        );
        assert_eq!(
            LifecycleEvent::Deleted {
                agent_id: "ana".into(),
                tenant: "default".into(),
                snapshot_id: m.id,
                ts_ms: 0,
            }
            .subject(),
            "nexo.memory.snapshot.ana.deleted"
        );
        assert_eq!(
            LifecycleEvent::Gc {
                ts_ms: 0,
                report: Default::default(),
            }
            .subject(),
            "nexo.memory.snapshot._all.gc"
        );
    }

    #[test]
    fn mutation_subject_carries_agent_id() {
        let e = MutationEvent {
            agent_id: "ana".into(),
            tenant: "default".into(),
            scope: MutationScope::SqliteLongTerm,
            op: MutationOp::Insert,
            key: "row-1".into(),
            old_hash: None,
            new_hash: Some("ab".repeat(32)),
            ts_ms: 0,
        };
        assert_eq!(e.subject(), "nexo.memory.mutated.ana");
    }

    #[test]
    fn mutation_event_round_trips_via_json() {
        let e = MutationEvent {
            agent_id: "ana".into(),
            tenant: "default".into(),
            scope: MutationScope::Git,
            op: MutationOp::Update,
            key: "deadbeef".into(),
            old_hash: None,
            new_hash: None,
            ts_ms: 17,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"scope\":\"git\""));
        assert!(s.contains("\"op\":\"update\""));
        let back: MutationEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back.key, e.key);
        assert_eq!(back.ts_ms, e.ts_ms);
    }

    #[tokio::test]
    async fn noop_publisher_silently_drops_both_event_kinds() {
        let p: Arc<dyn EventPublisher> = Arc::new(NoopPublisher);
        p.publish_lifecycle(LifecycleEvent::Created(sample_meta())).await;
        p.publish_mutation(MutationEvent {
            agent_id: "ana".into(),
            tenant: "default".into(),
            scope: MutationScope::SqliteVector,
            op: MutationOp::Delete,
            key: "k".into(),
            old_hash: None,
            new_hash: None,
            ts_ms: 0,
        })
        .await;
    }

    #[tokio::test]
    async fn recording_publisher_keeps_every_event() {
        let p = Arc::new(RecordingPublisher::default());
        let pa: Arc<dyn EventPublisher> = p.clone();
        pa.publish_lifecycle(LifecycleEvent::Created(sample_meta())).await;
        pa.publish_lifecycle(LifecycleEvent::Gc {
            ts_ms: 0,
            report: Default::default(),
        })
        .await;
        pa.publish_mutation(MutationEvent {
            agent_id: "ana".into(),
            tenant: "default".into(),
            scope: MutationScope::SqliteConcepts,
            op: MutationOp::Insert,
            key: "k".into(),
            old_hash: None,
            new_hash: None,
            ts_ms: 0,
        })
        .await;

        assert_eq!(p.lifecycle.lock().len(), 2);
        assert_eq!(p.mutation.lock().len(), 1);
    }

    #[test]
    fn dyn_publisher_can_be_held_as_arc() {
        let _p: Arc<dyn EventPublisher> = Arc::new(NoopPublisher);
    }
}
