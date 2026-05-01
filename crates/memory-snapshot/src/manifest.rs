//! On-disk manifest schema for a snapshot bundle.
//!
//! The manifest is the first artifact written and the first read on
//! verify/restore. It stays plaintext even when the body is age-encrypted
//! so integrity (per-artifact + bundle SHA-256) can be verified without
//! the identity. It is also forwards-compatible: a runtime accepts older
//! `manifest_version` values, refuses newer ones with
//! [`crate::error::SnapshotError::SchemaTooNew`].

use serde::{Deserialize, Serialize};

use crate::id::{AgentId, SnapshotId};

/// Current manifest schema. Bump on any field that older runtimes would
/// fail to interpret correctly; older bundles must keep loading.
pub const MANIFEST_VERSION: u32 = 1;

/// Format identifier embedded in the manifest. Distinguishes our bundle
/// from any future incompatible packing scheme.
pub const BUNDLE_FORMAT: &str = "nexo-memory-snapshot-v1";

/// Per-database schema versions captured in the manifest so a restore
/// can refuse incompatible bundles before swapping live tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaVersions {
    pub long_term: u32,
    pub vector: u32,
    pub concepts: u32,
    pub compactions: u32,
    pub manifest: u32,
}

impl SchemaVersions {
    /// Versions this runtime knows how to read + write. Update in lockstep
    /// with whichever crate owns the schema.
    pub const CURRENT: SchemaVersions = SchemaVersions {
        long_term: 1,
        vector: 1,
        concepts: 1,
        compactions: 1,
        manifest: MANIFEST_VERSION,
    };

    /// Returns true when every field of `self` is `<=` the matching field
    /// of `runtime`. Used by verify/restore as the compatibility gate.
    pub fn is_supported_by(&self, runtime: &SchemaVersions) -> bool {
        self.long_term <= runtime.long_term
            && self.vector <= runtime.vector
            && self.concepts <= runtime.concepts
            && self.compactions <= runtime.compactions
            && self.manifest <= runtime.manifest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    GitBundle,
    SqliteLongTerm,
    SqliteVector,
    SqliteConcepts,
    SqliteCompactions,
    StateExtractCursor,
    StateDreamRun,
    MemoryFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactMeta {
    /// Path inside the tar (e.g. `sqlite/long_term.sqlite`).
    pub path_in_bundle: String,
    pub kind: ArtifactKind,
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of the artifact bytes.
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitMeta {
    pub head_oid: String,
    pub head_subject: String,
    pub head_author: String,
    pub head_ts_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolVersions {
    pub nexo_version: String,
    pub git2: String,
    pub sqlx: String,
}

impl ToolVersions {
    /// Static stamp of the runtime's tool ecosystem. Embedded in every
    /// new manifest so a forensic reader can cross-check binaries.
    pub fn current() -> Self {
        Self {
            nexo_version: env!("CARGO_PKG_VERSION").to_string(),
            git2: "0.19".to_string(),
            sqlx: "0.8".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionReport {
    /// Identifier of the policy applied (e.g. `secret_guard:default`).
    pub policy: String,
    /// Rule ids that fired at least once.
    pub rules_applied: Vec<String>,
    /// Total number of redactions written into the bundle.
    pub matches_redacted: u32,
    /// `path_in_bundle` strings whose content was rewritten.
    pub artifacts_touched: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionMeta {
    /// `"age"` for the only scheme shipped today; future schemes keep
    /// the manifest forwards-compatible.
    pub scheme: String,
    /// Recipient fingerprints (age public-key short form). Lets a
    /// reader confirm which identity decrypts the body without
    /// requiring the identity itself.
    pub recipients_fingerprint: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub manifest_version: u32,
    pub bundle_format: String,
    pub snapshot_id: SnapshotId,
    pub agent_id: AgentId,
    pub tenant: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at_ms: i64,
    /// `cli` | `tool` | `auto-pre-restore` | `auto-pre-dream` | `scheduled`.
    pub created_by: String,
    pub schema_versions: SchemaVersions,
    pub git: GitMeta,
    pub artifacts: Vec<ArtifactMeta>,
    /// `None` when the body was packed without redaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redactions: Option<RedactionReport>,
    /// `None` for plaintext bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<EncryptionMeta>,
    pub tool_versions: ToolVersions,
    /// Whole-bundle SHA-256 (post-encryption when encrypted). Verifying
    /// this hash also implicitly verifies every per-artifact hash because
    /// the artifacts are streamed through the same hasher.
    pub bundle_sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SnapshotId;

    fn sample() -> Manifest {
        Manifest {
            manifest_version: MANIFEST_VERSION,
            bundle_format: BUNDLE_FORMAT.into(),
            snapshot_id: SnapshotId::new(),
            agent_id: "ana".into(),
            tenant: "default".into(),
            label: Some("smoke".into()),
            created_at_ms: 1_700_000_000_000,
            created_by: "cli".into(),
            schema_versions: SchemaVersions::CURRENT,
            git: GitMeta {
                head_oid: "deadbeef".into(),
                head_subject: "snapshot:smoke".into(),
                head_author: "operator".into(),
                head_ts_ms: 1_700_000_000_000,
            },
            artifacts: vec![ArtifactMeta {
                path_in_bundle: "sqlite/long_term.sqlite".into(),
                kind: ArtifactKind::SqliteLongTerm,
                size_bytes: 4096,
                sha256: "00".repeat(32),
            }],
            redactions: None,
            encryption: None,
            tool_versions: ToolVersions::current(),
            bundle_sha256: "ff".repeat(32),
        }
    }

    #[test]
    fn schema_current_is_supported_by_itself() {
        let v = SchemaVersions::CURRENT;
        assert!(v.is_supported_by(&v));
    }

    #[test]
    fn schema_too_new_is_unsupported() {
        let bundle = SchemaVersions {
            manifest: SchemaVersions::CURRENT.manifest + 1,
            ..SchemaVersions::CURRENT
        };
        assert!(!bundle.is_supported_by(&SchemaVersions::CURRENT));
    }

    #[test]
    fn schema_older_field_is_supported() {
        let bundle = SchemaVersions {
            long_term: 0,
            ..SchemaVersions::CURRENT
        };
        assert!(bundle.is_supported_by(&SchemaVersions::CURRENT));
    }

    #[test]
    fn manifest_round_trip_via_json() {
        let m = sample();
        let s = serde_json::to_string(&m).unwrap();
        let back: Manifest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.snapshot_id, m.snapshot_id);
        assert_eq!(back.bundle_format, BUNDLE_FORMAT);
        assert_eq!(back.tenant, "default");
    }

    #[test]
    fn manifest_omits_optional_fields_when_none() {
        let m = Manifest {
            label: None,
            ..sample()
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(!s.contains("\"label\""));
        assert!(!s.contains("\"redactions\""));
        assert!(!s.contains("\"encryption\""));
    }

    #[test]
    fn artifact_kind_serializes_snake_case() {
        let s = serde_json::to_string(&ArtifactKind::SqliteLongTerm).unwrap();
        assert_eq!(s, "\"sqlite_long_term\"");
    }
}
