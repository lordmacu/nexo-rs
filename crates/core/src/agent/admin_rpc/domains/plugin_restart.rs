//! Phase 81.21.b.b follow-up — `nexo/admin/plugins/restart` handler.
//!
//! Operator verb that drives `SubprocessNexoPlugin::force_restart`
//! via the [`PluginRestarter`] trait. Distinct from the auto-respawn
//! loop's `crashed`+`respawned` cycle: this publishes
//! `plugin.lifecycle.<id>.restarted_manually` because the kill was
//! intentional.
//!
//! Error mapping follows the established admin-rpc convention:
//!   - "plugin {id} not found" → `InvalidParams` (stale list)
//!   - "plugin {id} is in-tree" → `InvalidParams` (use daemon restart)
//!   - "not yet populated" → `InvalidParams` (boot-window race —
//!     daemon still finishing wire_plugin_registry; SPA should
//!     retry in 1-2s rather than show a generic 500)
//!   - "restart timed out" → `Internal` (degraded state)
//!   - other anyhow → `Internal`

use async_trait::async_trait;
use serde_json::Value;

use nexo_tool_meta::admin::plugin_restart::{
    PluginsRestartParams, PluginsRestartResponse,
};

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Adapter abstraction over the daemon's plugin registry +
/// `SubprocessNexoPlugin::force_restart`. Production wires
/// `nexo_setup::admin_adapters::LivePluginRestarter`; tests
/// inject in-memory fakes.
#[async_trait]
pub trait PluginRestarter: Send + Sync + std::fmt::Debug {
    /// Force-restart the named plugin. See module-level docs for
    /// the error → `AdminRpcError` mapping the handler applies.
    async fn restart(
        &self,
        plugin_id: &str,
    ) -> anyhow::Result<PluginsRestartResponse>;
}

/// `nexo/admin/plugins/restart` — operator-driven subprocess
/// plugin restart. Empty `plugin_id` is rejected before reaching
/// the adapter so a typo doesn't tear down the wrong plugin.
pub async fn restart_plugin(
    reader: &dyn PluginRestarter,
    params: Value,
) -> AdminRpcResult {
    let p: PluginsRestartParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if p.plugin_id.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "plugin_id is empty".into(),
        ));
    }
    match reader.restart(&p.plugin_id).await {
        Ok(resp) => AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null)),
        Err(e) => {
            let msg = e.to_string();
            // User-recoverable substrings (operator can fix
            // without escalation): map back to InvalidParams so
            // the SPA renders inline error instead of generic 500.
            // "not yet populated" covers the brief boot-window
            // race where the admin RPC arrives BEFORE main.rs
            // writes `wire.plugin_handles` into the
            // SharedPluginHandles cell — operator should retry
            // after 1-2s, not see a hard failure.
            if msg.contains("not found")
                || msg.contains("is in-tree")
                || msg.contains("not yet populated")
            {
                AdminRpcResult::err(AdminRpcError::InvalidParams(msg))
            } else {
                AdminRpcResult::err(AdminRpcError::Internal(format!(
                    "plugins.restart: {msg}"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct StubRestarter {
        calls: Mutex<Vec<String>>,
        next_err: Mutex<Option<String>>,
    }

    #[async_trait]
    impl PluginRestarter for StubRestarter {
        async fn restart(
            &self,
            plugin_id: &str,
        ) -> anyhow::Result<PluginsRestartResponse> {
            self.calls.lock().unwrap().push(plugin_id.to_string());
            if let Some(msg) = self.next_err.lock().unwrap().take() {
                anyhow::bail!(msg);
            }
            Ok(PluginsRestartResponse {
                plugin_id: plugin_id.into(),
                previous_uptime_ms: 12345,
                restarted_at_ms: 1_700_000_000_000,
                new_pid: Some(54321),
            })
        }
    }

    #[tokio::test]
    async fn restart_plugin_records_call() {
        let r = StubRestarter::default();
        let res = restart_plugin(
            &r,
            serde_json::json!({"plugin_id": "browser"}),
        )
        .await;
        let payload = res.result.expect("ok");
        assert_eq!(payload["plugin_id"], "browser");
        assert_eq!(payload["previous_uptime_ms"], 12345);
        assert_eq!(payload["new_pid"], 54321);
        let recorded = r.calls.lock().unwrap().clone();
        assert_eq!(recorded, vec!["browser".to_string()]);
    }

    #[tokio::test]
    async fn restart_plugin_rejects_empty_plugin_id() {
        let r = StubRestarter::default();
        let res = restart_plugin(
            &r,
            serde_json::json!({"plugin_id": ""}),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("plugin_id is empty"));
        // Adapter never called.
        assert!(r.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn restart_plugin_maps_not_found_and_in_tree_to_invalid_params() {
        let r = StubRestarter::default();
        // not_found → InvalidParams
        *r.next_err.lock().unwrap() =
            Some("plugin xyz not found in registry".into());
        let res = restart_plugin(
            &r,
            serde_json::json!({"plugin_id": "xyz"}),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("not found"));

        // in-tree → InvalidParams
        *r.next_err.lock().unwrap() =
            Some("plugin assistant is in-tree, restart not applicable".into());
        let res = restart_plugin(
            &r,
            serde_json::json!({"plugin_id": "assistant"}),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("is in-tree"));

        // timeout → Internal (NOT InvalidParams)
        *r.next_err.lock().unwrap() =
            Some("restart timed out, plugin may be in degraded state".into());
        let res = restart_plugin(
            &r,
            serde_json::json!({"plugin_id": "browser"}),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32603); // Internal
    }

    /// Phase 90 audit follow-up — boot-window race surfaces from
    /// `LivePluginRestarter` as "plugin handles not yet populated;
    /// daemon still booting". Genuinely user-recoverable (retry
    /// 1-2s), so the handler must classify it as InvalidParams not
    /// Internal so the SPA renders a transient toast instead of
    /// the generic 500 modal.
    #[tokio::test]
    async fn restart_plugin_maps_not_yet_populated_to_invalid_params() {
        let r = StubRestarter::default();
        *r.next_err.lock().unwrap() = Some(
            "plugin handles not yet populated; daemon still booting".into(),
        );
        let res = restart_plugin(
            &r,
            serde_json::json!({"plugin_id": "browser"}),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602, "boot-window error must be user-recoverable");
        assert!(err.to_string().contains("not yet populated"));
    }
}
