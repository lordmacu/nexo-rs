//! `nexo/admin/plugins/restart` wire types.
//!
//! Operator-driven plugin restart. Distinct from the auto-respawn
//! loop (which fires on crash detection); this is an intentional
//! verb that publishes its own `plugin.lifecycle.<id>.restarted_manually`
//! event instead of `crashed`+`respawned`. Used by the admin UI's
//! Plugins module to recover from `gave_up` state without restarting
//! the whole daemon.
//!
//! See `docs/src/plugins/supervisor.md` § Manual restart for the
//! operator-facing flow.

use serde::{Deserialize, Serialize};

/// Params for `nexo/admin/plugins/restart`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsRestartParams {
    /// Plugin id from the manifest's `[plugin] id` field. Must
    /// match a discovered subprocess plugin or the daemon
    /// returns `InvalidParams { "plugin {id} not found" }`.
    /// In-tree plugins return `InvalidParams { "plugin {id} is
    /// in-tree, restart not applicable" }` — operators restart
    /// the daemon for those.
    pub plugin_id: String,
}

/// Response for `nexo/admin/memory/restart`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsRestartResponse {
    /// Echo of the plugin id for client-side correlation.
    pub plugin_id: String,
    /// Wallclock duration the previous `Inner` survived between
    /// last spawn (initial init or last respawn) and the manual
    /// restart. Operator forensics value: "this child lived 3
    /// minutes before I had to restart".
    pub previous_uptime_ms: u64,
    /// Wallclock millis since epoch when the new `Inner` was
    /// installed. Audit log correlation.
    pub restarted_at_ms: i64,
    /// PID of the freshly spawned child. Operator dashboards can
    /// correlate with `ps`/`top` output. `None` when the
    /// post-spawn shutdown race-check kicked in (very rare;
    /// indicates the daemon shut down between the spawn
    /// completing and the install).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_pid: Option<u32>,
}
