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
