//! `nexo/admin/plugins/{scan,install,uninstall}` handlers (Phase 97.1).
//!
//! Trio of operator verbs that wire runtime plugin lifecycle from
//! the admin UI:
//!   - `scan`: re-run discovery walker, hot-spawn any binary found
//!     that the live registry doesn't already track. Operator uses
//!     this when they `cargo install` outside the daemon or copy
//!     a pre-built binary into `~/.cargo/bin/`.
//!   - `install`: fetch + register a new plugin in one call. Two
//!     delivery paths (see `nexo_tool_meta::admin::plugin_install::InstallSource`):
//!     prebuilt release tarball download (`Release`, default — zero
//!     toolchain on client) OR `cargo install` (compiles from
//!     source — dev/sysadmin fallback).
//!   - `uninstall`: stop subprocess, drop handle from registry,
//!     optionally `cargo uninstall` the binary.
//!
//! Error mapping follows the established admin-rpc convention:
//!   - bad params → `InvalidParams` (-32602)
//!   - "plugin {id} not found" / boot-window race → `InvalidParams`
//!   - everything else → `Internal` (-32603)

use async_trait::async_trait;
use serde_json::Value;

use nexo_tool_meta::admin::plugin_install::{
    PluginsInstallParams, PluginsInstallResponse, PluginsScanParams, PluginsScanResponse,
    PluginsSetEnabledParams, PluginsSetEnabledResponse, PluginsUninstallParams,
    PluginsUninstallResponse,
};

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Adapter abstraction over the daemon's plugin discovery + factory
/// pipeline + supervisor. Production wires
/// `nexo_setup::admin_adapters::LivePluginInstaller`; tests inject
/// in-memory fakes.
#[async_trait]
pub trait PluginInstaller: Send + Sync + std::fmt::Debug {
    /// Re-run discovery + hot-spawn any plugins absent from the
    /// live registry. Returns the list of newly-spawned ids plus
    /// stale handles (registered but no longer discoverable) so the
    /// UI can render a "needs uninstall" badge without an extra
    /// round-trip.
    async fn scan(&self) -> anyhow::Result<PluginsScanResponse>;

    /// Fetch the plugin binary via the requested delivery channel,
    /// validate it, register it in the live runtime. Idempotent —
    /// re-installing the same crate@version without `force = true`
    /// reports the existing version and skips the download.
    async fn install(
        &self,
        params: &PluginsInstallParams,
    ) -> anyhow::Result<PluginsInstallResponse>;

    /// Stop the subprocess + remove the handle. When the caller
    /// requests `cargo_uninstall = true`, also delete the on-disk
    /// binary via `cargo uninstall`. Idempotent — missing plugins
    /// report `removed: false`.
    async fn uninstall(
        &self,
        params: &PluginsUninstallParams,
    ) -> anyhow::Result<PluginsUninstallResponse>;

    /// Phase 98 follow-up — toggle a plugin's enabled state.
    /// `enabled = false` appends the id to `plugins/discovery.yaml`'s
    /// `disabled[]` + hot-removes the live handle (binary stays on
    /// disk). `enabled = true` removes the id + hot-spawns. Persisted
    /// in the yaml so it survives a daemon restart. Idempotent —
    /// re-requesting the current state reports `config_changed: false`.
    async fn set_enabled(
        &self,
        params: &PluginsSetEnabledParams,
    ) -> anyhow::Result<PluginsSetEnabledResponse>;

    /// Reconcile the live subprocesses of a multi-instance channel
    /// against its just-persisted config: spawn newly-added instances,
    /// kill removed ones, and re-push `plugin.configure` to surviving
    /// ones (eager-start). Called after `credentials/register` so a
    /// channel instance configured at runtime comes online without a
    /// daemon restart. Default no-op for minimal embeddings / tests.
    async fn reconcile_channel_instances(&self, _channel: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

// ── handlers ────────────────────────────────────────────────────

/// `nexo/admin/plugins/scan`.
pub async fn scan_plugins(reader: &dyn PluginInstaller, params: Value) -> AdminRpcResult {
    if let Err(e) = serde_json::from_value::<PluginsScanParams>(params) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string()));
    }
    match reader.scan().await {
        Ok(resp) => AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null)),
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!("plugins.scan: {e}"))),
    }
}

/// `nexo/admin/plugins/install`. Validates `crate_name`, `version`,
/// `repo` against tight character classes BEFORE delegating to the
/// adapter — keeps shell / URL injection at the protocol boundary.
pub async fn install_plugin(reader: &dyn PluginInstaller, params: Value) -> AdminRpcResult {
    let p: PluginsInstallParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if let Err(msg) = validate_crate_name(&p.crate_name) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(msg));
    }
    if let Some(v) = p.version.as_deref() {
        if let Err(msg) = validate_version(v) {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(msg));
        }
    }
    if let Some(r) = p.repo.as_deref() {
        if let Err(msg) = validate_repo_slug(r) {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(msg));
        }
    }
    // Phase 98.4 — parity with CLI: `require_signature` and
    // `skip_signature_verify` are mutually exclusive. CLI rejects
    // this at flag-parse time; the admin handler enforces the same
    // constraint at the protocol boundary so neither caller can
    // smuggle a contradictory pair past the silent installer.
    if p.require_signature && p.skip_signature_verify {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "require_signature and skip_signature_verify are mutually exclusive".into(),
        ));
    }
    match reader.install(&p).await {
        Ok(resp) => AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null)),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not yet populated")
                || msg.contains("already installed")
                || msg.contains("manifest validation")
            {
                AdminRpcResult::err(AdminRpcError::InvalidParams(msg))
            } else {
                AdminRpcResult::err(AdminRpcError::Internal(format!("plugins.install: {msg}")))
            }
        }
    }
}

/// `nexo/admin/plugins/uninstall`.
pub async fn uninstall_plugin(reader: &dyn PluginInstaller, params: Value) -> AdminRpcResult {
    let p: PluginsUninstallParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if p.plugin_id.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("plugin_id is empty".into()));
    }
    if p.cargo_uninstall {
        if let Some(crate_name) = p.crate_name.as_deref() {
            if let Err(msg) = validate_crate_name(crate_name) {
                return AdminRpcResult::err(AdminRpcError::InvalidParams(msg));
            }
        }
    }
    match reader.uninstall(&p).await {
        Ok(resp) => AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null)),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found")
                || msg.contains("is in-tree")
                || msg.contains("not yet populated")
            {
                AdminRpcResult::err(AdminRpcError::InvalidParams(msg))
            } else {
                AdminRpcResult::err(AdminRpcError::Internal(format!("plugins.uninstall: {msg}")))
            }
        }
    }
}

/// `nexo/admin/plugins/set_enabled`.
pub async fn set_enabled_plugin(reader: &dyn PluginInstaller, params: Value) -> AdminRpcResult {
    let p: PluginsSetEnabledParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if p.plugin_id.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("plugin_id is empty".into()));
    }
    match reader.set_enabled(&p).await {
        Ok(resp) => AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null)),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found")
                || msg.contains("is in-tree")
                || msg.contains("not yet populated")
            {
                AdminRpcResult::err(AdminRpcError::InvalidParams(msg))
            } else {
                AdminRpcResult::err(AdminRpcError::Internal(format!(
                    "plugins.set_enabled: {msg}"
                )))
            }
        }
    }
}

// ── validators ──────────────────────────────────────────────────
//
// Tight character classes. The crate name + version + repo flow into
// shell / URL contexts; relax only at the cost of escape-handling.

fn validate_crate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("crate_name is empty".into());
    }
    if name.len() > 64 {
        return Err("crate_name exceeds 64 chars".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "crate_name `{name}` contains chars outside [a-z0-9_-]"
        ));
    }
    Ok(())
}

fn validate_version(v: &str) -> Result<(), String> {
    if v.is_empty() {
        return Err("version is empty".into());
    }
    if v.len() > 32 {
        return Err("version exceeds 32 chars".into());
    }
    if !v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '+' || c == '-')
    {
        return Err(format!(
            "version `{v}` contains chars outside [0-9A-Za-z.+-]"
        ));
    }
    Ok(())
}

fn validate_repo_slug(slug: &str) -> Result<(), String> {
    if slug.is_empty() {
        return Err("repo is empty".into());
    }
    let mut parts = slug.split('/');
    let (Some(org), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!("repo `{slug}` must match <org>/<name>"));
    };
    for (label, part) in [("org", org), ("name", name)] {
        if part.is_empty() || part.len() > 64 {
            return Err(format!("repo {label} `{part}` empty or > 64 chars"));
        }
        if !part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(format!(
                "repo {label} `{part}` contains chars outside [A-Za-z0-9_.-]"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::admin::plugin_install::InstallSource;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct StubInstaller {
        last_install: Mutex<Option<PluginsInstallParams>>,
        last_uninstall: Mutex<Option<PluginsUninstallParams>>,
        scans: Mutex<u32>,
        next_err: Mutex<Option<String>>,
    }

    #[async_trait]
    impl PluginInstaller for StubInstaller {
        async fn scan(&self) -> anyhow::Result<PluginsScanResponse> {
            *self.scans.lock().unwrap() += 1;
            if let Some(msg) = self.next_err.lock().unwrap().take() {
                anyhow::bail!(msg);
            }
            Ok(PluginsScanResponse {
                spawned: vec!["telegram".into()],
                stale: vec![],
                warnings: vec![],
            })
        }
        async fn install(
            &self,
            params: &PluginsInstallParams,
        ) -> anyhow::Result<PluginsInstallResponse> {
            *self.last_install.lock().unwrap() = Some(params.clone());
            if let Some(msg) = self.next_err.lock().unwrap().take() {
                anyhow::bail!(msg);
            }
            Ok(PluginsInstallResponse {
                crate_name: params.crate_name.clone(),
                installed_version: params.version.clone(),
                spawned: vec![params.crate_name.replace("nexo-plugin-", "")],
                cargo_stdout: String::new(),
                cargo_stderr: String::new(),
                trust_enforcement: "policy_applied".to_string(),
                signature_verified: false,
                signature_identity: None,
                signature_issuer: None,
                trust_mode: "ignore".to_string(),
                trust_policy_matched: None,
            })
        }
        async fn uninstall(
            &self,
            params: &PluginsUninstallParams,
        ) -> anyhow::Result<PluginsUninstallResponse> {
            *self.last_uninstall.lock().unwrap() = Some(params.clone());
            if let Some(msg) = self.next_err.lock().unwrap().take() {
                anyhow::bail!(msg);
            }
            Ok(PluginsUninstallResponse {
                plugin_id: params.plugin_id.clone(),
                removed: true,
                cargo_uninstalled: params.cargo_uninstall,
                cargo_stdout: String::new(),
                cargo_stderr: String::new(),
            })
        }
        async fn set_enabled(
            &self,
            params: &PluginsSetEnabledParams,
        ) -> anyhow::Result<PluginsSetEnabledResponse> {
            if let Some(msg) = self.next_err.lock().unwrap().take() {
                anyhow::bail!(msg);
            }
            Ok(PluginsSetEnabledResponse {
                plugin_id: params.plugin_id.clone(),
                enabled: params.enabled,
                config_changed: true,
                spawned: if params.enabled {
                    vec![params.plugin_id.clone()]
                } else {
                    vec![]
                },
                removed: !params.enabled,
                warnings: vec![],
            })
        }
    }

    #[tokio::test]
    async fn scan_invokes_adapter() {
        let stub = StubInstaller::default();
        let res = scan_plugins(&stub, serde_json::json!({})).await;
        let payload = res.result.expect("ok");
        assert_eq!(payload["spawned"][0], "telegram");
        assert_eq!(*stub.scans.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn install_rejects_bad_crate_name() {
        let stub = StubInstaller::default();
        let res = install_plugin(
            &stub,
            serde_json::json!({"crate_name": "Bad Name With Spaces"}),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("outside [a-z0-9_-]"));
        assert!(stub.last_install.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn install_rejects_bad_version() {
        let stub = StubInstaller::default();
        let res = install_plugin(
            &stub,
            serde_json::json!({"crate_name": "nexo-plugin-telegram", "version": "1.0 ; rm -rf /"}),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("outside"));
    }

    #[tokio::test]
    async fn install_rejects_bad_repo_slug() {
        let stub = StubInstaller::default();
        let res = install_plugin(
            &stub,
            serde_json::json!({"crate_name": "nexo-plugin-telegram", "repo": "evil; rm -rf"}),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
    }

    #[tokio::test]
    async fn install_accepts_valid_params_with_release_source() {
        let stub = StubInstaller::default();
        let res = install_plugin(
            &stub,
            serde_json::json!({
                "crate_name": "nexo-plugin-telegram",
                "version": "0.3.0",
                "repo": "lordmacu/nexo-rs-plugin-telegram",
            }),
        )
        .await;
        let payload = res.result.expect("ok");
        assert_eq!(payload["installed_version"], "0.3.0");
        let last = stub.last_install.lock().unwrap().clone().unwrap();
        assert_eq!(last.source, InstallSource::Release);
    }

    #[tokio::test]
    async fn uninstall_rejects_empty_plugin_id() {
        let stub = StubInstaller::default();
        let res = uninstall_plugin(&stub, serde_json::json!({"plugin_id": ""})).await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
    }

    #[tokio::test]
    async fn uninstall_validates_crate_name_only_when_cargo_uninstall_requested() {
        let stub = StubInstaller::default();
        // cargo_uninstall = false → crate_name char class not checked
        let res = uninstall_plugin(
            &stub,
            serde_json::json!({"plugin_id": "telegram", "crate_name": "Bad Name"}),
        )
        .await;
        assert!(res.error.is_none(), "should bypass validation");
        // cargo_uninstall = true → crate_name char class enforced
        let res = uninstall_plugin(
            &stub,
            serde_json::json!({"plugin_id": "telegram", "crate_name": "Bad Name", "cargo_uninstall": true}),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
    }

    #[tokio::test]
    async fn install_already_installed_maps_to_invalid_params() {
        let stub = StubInstaller::default();
        *stub.next_err.lock().unwrap() = Some("nexo-plugin-x already installed at v0.1.0".into());
        let res = install_plugin(&stub, serde_json::json!({"crate_name": "nexo-plugin-x"})).await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
    }

    /// Phase 98.4 — admin handler must reject contradictory trust
    /// flags at the protocol boundary, mirroring the CLI's
    /// `--require-signature` / `--skip-signature-verify` mutually
    /// exclusive check in `install_plugin_silent`.
    #[tokio::test]
    async fn install_rejects_mutually_exclusive_trust_flags() {
        let stub = StubInstaller::default();
        let res = install_plugin(
            &stub,
            serde_json::json!({
                "crate_name": "nexo-plugin-telegram",
                "require_signature": true,
                "skip_signature_verify": true,
            }),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(
            err.to_string().contains("mutually exclusive"),
            "expected mutually-exclusive error, got: {}",
            err
        );
        // Adapter must NOT have been called — rejection happens at
        // the boundary, never reaches `PluginInstaller::install`.
        assert!(stub.last_install.lock().unwrap().is_none());
    }

    /// Phase 98.4 — trust flags propagate through the handler to the
    /// adapter unchanged. Sanity check that the new wire fields aren't
    /// silently dropped by deserialization.
    #[tokio::test]
    async fn install_forwards_trust_flags_to_adapter() {
        let stub = StubInstaller::default();
        let res = install_plugin(
            &stub,
            serde_json::json!({
                "crate_name": "nexo-plugin-telegram",
                "require_signature": true,
            }),
        )
        .await;
        assert!(
            res.error.is_none(),
            "handler must accept require_signature alone"
        );
        let last = stub.last_install.lock().unwrap().clone().unwrap();
        assert!(last.require_signature, "require_signature lost in transit");
        assert!(
            !last.skip_signature_verify,
            "skip_signature_verify must default to false when absent from JSON"
        );
    }
}
