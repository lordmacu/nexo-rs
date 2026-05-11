//! `nexo/admin/memory/*` wire types.
//!
//! Surface:
//!
//! - `query` — text search over an agent's long-term memory
//!   entries. Mirrors the agent-side `memory.recall` SDK call but
//!   exposes it operator-side (so the admin UI can show "what
//!   does the agent remember about X").
//! - `list_snapshots` / `delete_snapshot` — bundle inventory +
//!   idempotent removal.
//! - `create_snapshot` / `restore_snapshot` — capture + restore
//!   bundles. Server
//!   resolves `snapshot_id → bundle_path` so clients never hold
//!   a filesystem path. `encrypt` is a bool toggle; the daemon
//!   resolves the actual age recipient from
//!   `memory.snapshot.encryption.recipients[0]`. UI exposes
//!   availability via the `encryption_available` flag carried on
//!   the list response.

use serde::{Deserialize, Serialize};

/// Params for `nexo/admin/memory/query`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
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
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
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
    /// Auto-derived concept tags.
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
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryQueryResponse {
    /// Matching entries newest-first within the relevance band.
    pub entries: Vec<MemoryEntryWire>,
}

/// Params for `nexo/admin/memory/list_snapshots`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
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
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
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
///
/// SHAPE NOTE (0.1.12):
/// the `snapshots` field is unchanged but the response gained
/// `encryption_available`. Older clients that previously
/// deserialized the response as `Vec<SnapshotMetaWire>` directly
/// will need to update — the wire is now a struct. The
/// `serde(default)` on `encryption_available` keeps newer clients
/// hitting older daemons functional (flag defaults to `false`,
/// disabling the create-modal encrypt toggle).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySnapshotsListResponse {
    /// Snapshots ordered by `created_at_ms` descending (newest first).
    pub snapshots: Vec<SnapshotMetaWire>,
    /// `true` iff `memory.snapshot.encryption.enabled` AND
    /// `recipients` non-empty at boot. Drives the encrypt toggle
    /// availability in the create-snapshot modal so operators
    /// don't try to encrypt without a recipient.
    #[serde(default)]
    pub encryption_available: bool,
}

/// Params for `nexo/admin/memory/delete_snapshot`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
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
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySnapshotsDeleteResponse {
    /// `true` when the bundle was removed; idempotent retry safe
    /// (returns `true` on missing id too — daemon's local-fs
    /// delete is `Ok(())` for already-gone bundles).
    pub removed: bool,
}

/// Params for `nexo/admin/memory/create_snapshot`.
///
/// `encrypt` is a server-resolved toggle: the daemon picks the
/// recipient from `memory.snapshot.encryption.recipients[0]`. The
/// wire never carries the recipient string itself — clients can't
/// inject keys, and operators rotate recipients via YAML.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySnapshotsCreateParams {
    /// Agent whose memory to snapshot.
    pub agent_id: String,
    /// Tenant scope. Empty string = "default" (consistent with
    /// list/delete coercion).
    #[serde(default)]
    pub tenant: String,
    /// Optional human-readable label stored in the bundle's
    /// manifest (`pre-deploy`, `before-migration`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `true` requests age encryption. Server rejects with
    /// `InvalidParams` when `true` and recipients are empty.
    #[serde(default)]
    pub encrypt: bool,
}

/// Response for `nexo/admin/memory/create_snapshot`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySnapshotsCreateResponse {
    /// Metadata of the freshly captured bundle. Same shape as
    /// `list_snapshots` entries so the SPA can append directly to
    /// its in-memory list without a refetch.
    pub snapshot: SnapshotMetaWire,
}

/// Params for `nexo/admin/memory/restore_snapshot`.
///
/// Restore by `snapshot_id` (NOT `bundle_path`): the daemon
/// resolves the absolute path via its `list()` lookup, so
/// admin endpoints can't be coerced into reading arbitrary
/// files. `tenant` is REQUIRED — the server cross-checks it
/// against the bundle manifest's recorded tenant and rejects
/// mismatches before touching disk.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySnapshotsRestoreParams {
    /// Agent whose state to restore.
    pub agent_id: String,
    /// Tenant scope. REQUIRED — server validates against the
    /// bundle manifest's recorded tenant; mismatch =
    /// `InvalidParams` (defensive intent against accidental
    /// cross-tenant restore).
    pub tenant: String,
    /// Snapshot id (UUID-shaped). Server resolves to bundle
    /// path via `list()` lookup; client never sees fs paths.
    pub snapshot_id: String,
    /// `true` returns the diff that would be applied without
    /// mutating live state. Default `false` = destructive.
    /// Even when `true`, the daemon still validates tenant +
    /// resolves the bundle so the preview reflects the real
    /// restore plan.
    #[serde(default)]
    pub dry_run: bool,
}

/// Wire-side projection of `nexo_memory_snapshot::RestoreReport`.
/// `AgentId` and `SnapshotId` flatten to strings for forward
/// compatibility (the wire stays stable if the runtime swaps
/// id encodings).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RestoreReportWire {
    /// Agent the restore targeted.
    pub agent_id: String,
    /// Snapshot id the bundle was restored from.
    pub from_snapshot_id: String,
    /// Auto-captured pre-restore snapshot id. `None` for
    /// dry-run (no pre-snapshot is taken when nothing mutates).
    /// Production restore path forces `auto_pre_snapshot=true`
    /// server-side so this is always populated for destructive
    /// runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_snapshot_id: Option<String>,
    /// HEAD oid the memdir was reset to, when git capture was
    /// active. `None` when the snapshot didn't carry a git body
    /// (small / state-only bundles).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_reset_oid: Option<String>,
    /// SQLite databases that were restored (filenames, not
    /// absolute paths — the SPA lists them in the report panel).
    pub sqlite_restored_dbs: Vec<String>,
    /// State files (`extract_cursor`, `last_dream_run`, …) that
    /// were restored.
    pub state_files_restored: Vec<String>,
    /// `true` if the daemon paused + restarted agent workers as
    /// part of the restore (always `true` for destructive runs;
    /// `false` for dry-run).
    pub workers_restarted: bool,
    /// Mirror of the request's `dry_run` for the SPA to render
    /// "Preview" vs "Applied" headers without round-tripping
    /// state.
    pub dry_run: bool,
}

/// Response for `nexo/admin/memory/restore_snapshot`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySnapshotsRestoreResponse {
    /// Per-restore report. SPA uses this to render the dry-run
    /// preview table and the post-apply confirmation toast.
    pub report: RestoreReportWire,
}
