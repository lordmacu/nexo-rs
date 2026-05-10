//! Phase 90.x.plugins — `nexo/admin/plugins/*` wire types.
//!
//! Operator-facing snapshot of plugin discovery + spawn status.
//! v1 returns the full `PluginDiscoveryReport` from the daemon's
//! `nexo_core::agent::nexo_plugin_registry::doctor_render::render_json`
//! verbatim as a `serde_json::Value` so the wire surface stays in
//! lockstep with the `agent doctor plugins` CLI without a parallel
//! type hierarchy. Frontend parses the known fields it cares about
//! and renders the rest as a tree view.
//!
//! Future v2 (Phase 90.x.plugins.b): mirror the typed shape here
//! once the field set stabilises — gives the frontend ts-rs
//! codegen for free.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Response for `nexo/admin/plugins/doctor`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PluginsDoctorResponse {
    /// Full `PluginDiscoveryReport` JSON. Identical to what the
    /// `agent doctor plugins --json` CLI emits — a stable wire
    /// shape since Phase 81.11 (`doctor_render::render_json`).
    pub report: Value,
    /// Server-side wall-clock when the report was built (ms
    /// since epoch). Operators see this as a "as of …" hint.
    pub generated_at_ms: u64,
}
