//! Atomic point-in-time agent memory snapshots.
//!
//! Captures a verifiable bundle of:
//!
//! - Git-backed memory dir (`memdir/`) as a `git bundle`
//! - SQLite tables produced by the memory crate: `long_term`, `vector`,
//!   `concepts`, `compactions`
//! - Extractor cursor + last `dream_run` registry row
//!
//! The bundle is a `tar.zst` archive sealed with a SHA-256 manifest. An
//! optional `age` encryption layer is available behind the
//! `snapshot-encryption` Cargo feature; the manifest itself stays
//! plaintext so integrity can be verified without the identity.
//!
//! The crate ships:
//!
//! - [`MemorySnapshotter`] — trait every backend implements.
//! - [`SnapshotRequest`] / [`RestoreRequest`] — call inputs.
//! - [`SnapshotMeta`] / [`RestoreReport`] / [`SnapshotDiff`] /
//!   [`VerifyReport`] — public output shapes (also wire-shape for
//!   NATS lifecycle events and CLI JSON).
//! - [`Manifest`] — on-disk bundle header.
//! - [`SnapshotError`] — typed error class with stable variants.
//!
//! See `docs/src/ops/memory-snapshot.md` for the operator surface.

#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod codec;
pub mod config;
pub mod dream_adapter;
pub mod error;
pub mod events;
pub mod git_capture;
pub mod id;
pub mod local_fs;
pub mod manifest;
pub mod memdir;
pub mod meta;
pub mod metrics;
pub mod redaction;
pub mod request;
pub mod retention;
pub mod snapshotter;
pub mod sqlite_backup;
pub mod state_capture;
pub mod tenant_path;

pub use error::SnapshotError;
pub use id::{AgentId, SnapshotId};
pub use manifest::{
    ArtifactKind, ArtifactMeta, EncryptionMeta, GitMeta, Manifest, RedactionReport,
    SchemaVersions, ToolVersions, BUNDLE_FORMAT, MANIFEST_VERSION,
};
pub use meta::{
    GitDiffSummary, RestoreReport, SnapshotDiff, SnapshotMeta, SqliteDiffSummary,
    StateDiffSummary, VerifyReport,
};
pub use config::{
    EncryptionSection, EventsSection, MemorySnapshotConfig, RetentionSection,
};
pub use events::{
    EventPublisher, LifecycleEvent, MutationEvent, MutationOp, MutationScope, NoopPublisher,
    LIFECYCLE_SUBJECT_PREFIX, MUTATION_SUBJECT_PREFIX,
};
pub use dream_adapter::{MemoryMutationPublisher, PreDreamSnapshotAdapter};
pub use redaction::{DefaultRedactionPolicy, RedactionPolicy, RedactionPass};
pub use request::{DecryptionIdentity, EncryptionKey, RestoreRequest, SnapshotRequest};
pub use retention::{RetentionConfig, RetentionTickReport, RetentionWorker};
pub use snapshotter::MemorySnapshotter;
