//! Operator-facing summaries returned by [`crate::snapshotter::MemorySnapshotter`].
//!
//! These types appear in CLI JSON output, in NATS lifecycle event payloads,
//! and (for [`SnapshotMeta`]) in admin-ui responses. They are kept small,
//! `Serialize`-only on the report side, and free of internal handles.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::{AgentId, SnapshotId};
use crate::manifest::SchemaVersions;

/// Summary of a stored bundle. Built from the manifest at `list()` /
/// `snapshot()` time so callers do not have to unpack the body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub id: SnapshotId,
    pub agent_id: AgentId,
    pub tenant: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at_ms: i64,
    pub bundle_path: PathBuf,
    pub bundle_size_bytes: u64,
    pub bundle_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_oid: Option<String>,
    pub schema_versions: SchemaVersions,
    pub encrypted: bool,
    pub redactions_applied: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    pub agent_id: AgentId,
    pub from: SnapshotId,
    /// `Some` whenever an auto-pre-snapshot was captured (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_snapshot: Option<SnapshotId>,
    /// HEAD oid the memdir was reset to, if git restore happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_reset_oid: Option<String>,
    pub sqlite_restored_dbs: Vec<String>,
    pub state_files_restored: Vec<String>,
    pub workers_restarted: bool,
    /// `true` when the call exited without mutating live state.
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitDiffSummary {
    pub commits_between: u32,
    pub files_changed: u32,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SqliteDiffSummary {
    pub long_term_rows_added: i64,
    pub long_term_rows_removed: i64,
    pub vector_rows_added: i64,
    pub vector_rows_removed: i64,
    pub concepts_rows_added: i64,
    pub concepts_rows_removed: i64,
    pub compactions_added: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateDiffSummary {
    pub extract_cursor_changed: bool,
    pub last_dream_run_changed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotDiff {
    pub a: SnapshotId,
    pub b: SnapshotId,
    pub git_summary: GitDiffSummary,
    pub sqlite_summary: SqliteDiffSummary,
    pub state_summary: StateDiffSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub bundle: PathBuf,
    pub manifest_ok: bool,
    pub bundle_sha256_ok: bool,
    pub per_artifact_ok: bool,
    pub schema_versions: SchemaVersions,
    pub age_protected: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SnapshotId;

    #[test]
    fn snapshot_meta_round_trips_via_json() {
        let m = SnapshotMeta {
            id: SnapshotId::new(),
            agent_id: "ana".into(),
            tenant: "default".into(),
            label: Some("manual".into()),
            created_at_ms: 1_700_000_000_000,
            bundle_path: PathBuf::from("/tmp/x.tar.zst"),
            bundle_size_bytes: 1024,
            bundle_sha256: "ab".repeat(32),
            git_oid: Some("deadbeef".into()),
            schema_versions: SchemaVersions::CURRENT,
            encrypted: false,
            redactions_applied: true,
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: SnapshotMeta = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, m.id);
        assert_eq!(back.bundle_size_bytes, 1024);
        assert!(back.redactions_applied);
    }

    #[test]
    fn restore_report_omits_none_fields() {
        let r = RestoreReport {
            agent_id: "ana".into(),
            from: SnapshotId::new(),
            pre_snapshot: None,
            git_reset_oid: None,
            sqlite_restored_dbs: vec![],
            state_files_restored: vec![],
            workers_restarted: true,
            dry_run: false,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("\"pre_snapshot\""));
        assert!(!s.contains("\"git_reset_oid\""));
    }

    #[test]
    fn diff_summary_serializes_zero_deltas() {
        let d = SnapshotDiff {
            a: SnapshotId::new(),
            b: SnapshotId::new(),
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
        };
        let s = serde_json::to_string(&d).unwrap();
        assert!(s.contains("\"commits_between\":0"));
    }

    #[test]
    fn verify_report_round_trip_serialize_only() {
        let v = VerifyReport {
            bundle: PathBuf::from("x"),
            manifest_ok: true,
            bundle_sha256_ok: true,
            per_artifact_ok: true,
            schema_versions: SchemaVersions::CURRENT,
            age_protected: false,
        };
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"manifest_ok\":true"));
    }
}
