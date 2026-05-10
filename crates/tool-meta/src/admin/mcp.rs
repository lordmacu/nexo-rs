//! Phase 90.x.mcp — `nexo/admin/mcp/*` wire types.
//!
//! Operates on `config/mcp.yaml.mcp.servers.<name>`. Mirrors the
//! daemon's [`nexo_config::types::mcp::McpServerYaml`] enum but
//! flattens it into a transport-tagged struct so the operator UI
//! gets a single editable shape.
//!
//! Transports:
//!   - `stdio`            command + args + env + cwd
//!   - `streamable_http`  url + headers
//!   - `sse`              url + headers
//!   - `auto`             url + headers (probes streamable_http
//!                        first, falls back to sse)

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Compact summary used by `mcp/list` responses.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerSummary {
    /// Stable identifier (key in `mcp.servers`).
    pub name: String,
    /// Transport kind (`"stdio"` | `"streamable_http"` | `"sse"` | `"auto"`).
    pub transport: String,
    /// Optional log level override (passed to the server via
    /// `logging/setLevel` post-init when supported).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_level: Option<String>,
}

/// Full detail used by `mcp/get` + `mcp/upsert` payloads. Fields
/// not relevant to a given transport are `None` / empty.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerDetail {
    /// Stable identifier (key in `mcp.servers`).
    pub name: String,
    /// Transport kind (`"stdio"` | `"streamable_http"` | `"sse"` | `"auto"`).
    pub transport: String,

    /// Stdio transport — executable path / shell command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Stdio transport — argv tail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Stdio transport — env vars forwarded to the child.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Stdio transport — working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// HTTP / SSE / Auto transport — server URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// HTTP / SSE / Auto transport — extra headers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,

    /// All transports — log level override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_level: Option<String>,
    /// All transports — per-server override of the global
    /// `mcp.context.passthrough` flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_passthrough: Option<bool>,
}

/// Response for `nexo/admin/mcp/list`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServersListResponse {
    /// Servers in stable alpha order by name.
    pub servers: Vec<McpServerSummary>,
}

/// Params for `nexo/admin/mcp/get`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServersGetParams {
    /// Server name to fetch.
    pub name: String,
}

/// Response for `nexo/admin/mcp/get`. `server: None` is the
/// not-found case (daemon does NOT return an error so callers
/// can probe for existence cheaply).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServersGetResponse {
    /// Matching server, or `None` when the name is unknown.
    pub server: Option<McpServerDetail>,
}

/// Params for `nexo/admin/mcp/upsert`. Create-or-update — daemon
/// decides based on whether the name already exists.
pub type McpServersUpsertInput = McpServerDetail;

/// Response for `nexo/admin/mcp/upsert`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServersUpsertResponse {
    /// Final server record after the write.
    pub server: McpServerDetail,
    /// `true` when this call created a new server, `false` when
    /// it updated an existing one (idempotent retry).
    pub created: bool,
}

/// Params for `nexo/admin/mcp/delete`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServersDeleteParams {
    /// Server name to remove.
    pub name: String,
}

/// Response for `nexo/admin/mcp/delete`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServersDeleteResponse {
    /// `true` when the entry was removed, `false` when the name
    /// had no record (idempotent retry safe).
    pub removed: bool,
}
