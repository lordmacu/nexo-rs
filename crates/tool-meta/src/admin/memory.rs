//! Phase 90.x.memory — `nexo/admin/memory/*` wire types.
//!
//! v1 surface is `query` — text search over an agent's long-term
//! memory entries. Mirrors the agent-side `memory.recall` SDK
//! call but exposes it operator-side (so the admin UI can show
//! "what does the agent remember about X").
//!
//! `snapshot` operations (create / list / restore) are deferred
//! to a future sub-phase — heavyweight backups are still
//! triggered via the `agent memory snapshot` CLI.

use serde::{Deserialize, Serialize};

/// Params for `nexo/admin/memory/query`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryQueryParams {
    /// Agent whose memory to search.
    pub agent_id: String,
    /// Free-text query. Empty string returns the most recent
    /// entries (recency wins when no query terms hit the FTS5
    /// MATCH).
    #[serde(default)]
    pub query: String,
    /// Max rows to return. Server-side clamp [1, 100], default 20.
    #[serde(default)]
    pub limit: usize,
}

/// One memory entry over the wire. Mirrors
/// `nexo_memory::MemoryEntry` minus internal-only fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEntryWire {
    /// Stable UUID (string for wire stability).
    pub id: String,
    /// Owning agent.
    pub agent_id: String,
    /// Memory body (markdown / plain text).
    pub content: String,
    /// Operator-set tags (`#user`, `#feedback`, etc.).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Auto-derived concept tags (Phase 10.7 derivation).
    #[serde(default)]
    pub concept_tags: Vec<String>,
    /// ISO-8601 UTC timestamp of memory creation.
    pub created_at: String,
    /// Memory type (User / Feedback / Project / Reference) —
    /// drives per-type half-life decay in scoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_type: Option<String>,
}

/// Response for `nexo/admin/memory/query`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryQueryResponse {
    /// Matching entries newest-first within the relevance band.
    pub entries: Vec<MemoryEntryWire>,
}

/// Params for `nexo/admin/memory/list_snapshots`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySnapshotsListParams {
    /// Agent whose snapshots to list.
    pub agent_id: String,
    /// Tenant scope. Empty string = "default".
    #[serde(default)]
    pub tenant: String,
}

/// Wire-side projection of `nexo_memory_snapshot::SnapshotMeta`.
/// Internal fields (bundle_path, schema_versions, git_oid)
/// surfaced as opaque strings for forward-compat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotMetaWire {
    /// Snapshot id (UUID-shaped string).
    pub id: String,
    /// Owning agent.
    pub agent_id: String,
    /// Tenant scope.
    pub tenant: String,
    /// Optional human-readable label set at create time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Wall-clock millis since epoch when the bundle was written.
    pub created_at_ms: i64,
    /// Absolute filesystem path of the bundle archive.
    pub bundle_path: String,
    /// Bundle size in bytes (after compression / encryption).
    pub bundle_size_bytes: u64,
    /// Hex-encoded SHA-256 of the bundle.
    pub bundle_sha256: String,
    /// `Some(oid)` when git-mode snapshots captured a commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_oid: Option<String>,
    /// True when the bundle is age-encrypted (`*.tar.zst.age`).
    pub encrypted: bool,
    /// True when redaction policies stripped fields at capture time.
    pub redactions_applied: bool,
}

/// Response for `nexo/admin/memory/list_snapshots`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySnapshotsListResponse {
    /// Snapshots ordered by `created_at_ms` descending (newest first).
    pub snapshots: Vec<SnapshotMetaWire>,
}

/// Params for `nexo/admin/memory/delete_snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySnapshotsDeleteParams {
    /// Owning agent. Snapshots are partitioned by agent_id, so a
    /// rogue id can't delete another agent's bundle.
    pub agent_id: String,
    /// Tenant scope. Empty string = "default".
    #[serde(default)]
    pub tenant: String,
    /// Snapshot id (UUID-shaped) to remove.
    pub id: String,
}

/// Response for `nexo/admin/memory/delete_snapshot`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySnapshotsDeleteResponse {
    /// `true` when the bundle was removed; idempotent retry safe
    /// (returns `true` on missing id too — daemon's local-fs
    /// delete is `Ok(())` for already-gone bundles).
    pub removed: bool,
}
