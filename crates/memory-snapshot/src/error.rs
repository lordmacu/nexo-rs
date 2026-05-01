//! Typed errors for snapshot operations.

use crate::id::{AgentId, SnapshotId};

/// Failure modes surfaced by every public method on
/// [`crate::snapshotter::MemorySnapshotter`]. Each variant maps to a stable
/// CLI exit code + Prometheus `outcome` label so operators and tests can
/// branch on the failure class without parsing strings.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    /// Another snapshot is already being captured for this agent. Holders
    /// release the lock on completion or after the configured timeout.
    #[error("snapshot already in progress for agent {0}")]
    Concurrent(AgentId),

    /// The agent id has no on-disk state under this tenant root.
    #[error("agent not found: {0}")]
    UnknownAgent(AgentId),

    /// A bundle path resolved outside its tenant root. Always a
    /// programming error or a hostile bundle name; never a transient
    /// failure.
    #[error("path escapes tenant root")]
    CrossTenant,

    /// Either the manifest's whole-bundle hash or one of the
    /// per-artifact hashes did not match the recomputed value.
    #[error("manifest checksum mismatch")]
    ChecksumMismatch,

    /// Bundle's `manifest_version` is newer than this binary supports.
    /// Operators must upgrade the runtime; older bundles are accepted
    /// because the codec is forwards-compatible.
    #[error("bundle schema version newer than runtime ({bundle} > {runtime})")]
    SchemaTooNew { bundle: u32, runtime: u32 },

    /// A required artifact named in the manifest is missing from the
    /// bundle body.
    #[error("bundle missing artifact: {0}")]
    MissingArtifact(String),

    /// Restore preconditions failed (e.g. workers refused to pause,
    /// pre-snapshot failed, target dir not empty without `--force`).
    #[error("restore refused: {0}")]
    RestoreRefused(String),

    /// Encryption layer failed (key rejected, ciphertext malformed,
    /// `snapshot-encryption` feature off when bundle is age-wrapped).
    #[error("encryption: {0}")]
    Encryption(String),

    /// Retention sweep refused to delete (e.g. `keep_count` would drop
    /// below 1, or restore-in-progress on the candidate).
    #[error("retention violation: {0}")]
    Retention(String),

    /// A snapshot id exists in the manifest layout but the file under
    /// the tenant root is gone.
    #[error("snapshot {0} not found")]
    NotFound(SnapshotId),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("git: {0}")]
    Git(#[from] git2::Error),

    #[error("serde_json: {0}")]
    SerdeJson(#[from] serde_json::Error),

    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SnapshotId;
    use uuid::Uuid;

    #[test]
    fn display_concurrent_includes_agent() {
        let err = SnapshotError::Concurrent("ana".into());
        assert!(format!("{err}").contains("ana"));
    }

    #[test]
    fn display_schema_too_new_includes_versions() {
        let err = SnapshotError::SchemaTooNew {
            bundle: 5,
            runtime: 2,
        };
        let msg = format!("{err}");
        assert!(msg.contains("5"));
        assert!(msg.contains("2"));
    }

    #[test]
    fn display_not_found_includes_id() {
        let id = SnapshotId(Uuid::nil());
        let err = SnapshotError::NotFound(id);
        assert!(format!("{err}").contains("00000000"));
    }

    #[test]
    fn io_error_round_trip_via_from() {
        let raw = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        let err: SnapshotError = raw.into();
        assert!(matches!(err, SnapshotError::Io(_)));
    }
}
