//! Production [`ChannelCredentialPersister`]
//! for the whatsapp channel.
//!
//! WhatsApp credentials follow a different lifecycle than the
//! other channels: pairing happens out-of-band via the
//! `nexo/admin/pairing/*` admin RPC family + QR scan. The
//! pairing flow already writes its own session state under
//! `<state_root>/pairing/<instance>/`. So this persister is
//! intentionally a NO-OP: it gives the dispatcher a registered
//! handler for `channel = "whatsapp"` (so the back-compat
//! opaque-only fallback doesn't kick in) but does not duplicate
//! the pairing flow's writes.
//!
//! Probe returns `not_probed` because session health is the
//! pairing subsystem's concern (it surfaces via
//! `nexo/notify/pairing_status_changed` already).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use nexo_core::agent::admin_rpc::dispatcher::AdminRpcError;
use nexo_core::agent::admin_rpc::domains::credentials::ChannelCredentialPersister;
use nexo_tool_meta::admin::credentials::{reason_code, CredentialValidationOutcome};

/// Persister for the WhatsApp channel. Owns the
/// `<config_dir>/plugins/whatsapp.yaml` lifecycle: when the
/// admin UI completes a QR pair the dispatcher's
/// `pairing/start` fires [`Self::on_pair_success`] with the
/// operator-chosen instance label; the persister upserts the
/// matching entry so the WhatsApp plugin subprocess publishes
/// inbound on `plugin.inbound.whatsapp.<instance>` — aligned
/// with the agent binding the wizard writes in the same flow.
pub struct WhatsappPersister {
    /// Daemon's config dir (`<state_root>/config/`); the
    /// persister derives `plugins/whatsapp.yaml` against it.
    config_dir: PathBuf,
    /// Daemon's data dir (`<state_root>/data/`); per-instance
    /// `session_dir` / `media_dir` defaults land under
    /// `<data_dir>/whatsapp/<instance>/`. Each WA account
    /// owns its own Signal session.
    data_dir: PathBuf,
    /// Phase 97 — live plugin handles cell. After writing the
    /// new yaml entry, the persister re-reads the file and
    /// calls `configure()` on the WhatsApp plugin handle so the
    /// subprocess's `configured_state` carries the new instance
    /// BEFORE the pairing pump fires (the pump resolves
    /// `session_dir` via that state). Without this push the
    /// plugin's `pairing_admin` returns `instance not configured`.
    plugin_handles: crate::admin_adapters::SharedPluginHandles,
    /// Phase 97.2 — supervisor restart hook. When a pair lands
    /// (`on_pair_success`) the persister fires `restart("whatsapp")`
    /// AFTER the `plugin.configure` push so the subprocess tears
    /// down its stale Signal session and respawns reading the
    /// freshly-written yaml. Without this the subprocess keeps the
    /// old session looping with `reason=401` even after the operator
    /// paired a new device. `None` only in tests / minimal embeddings
    /// without admin RPC.
    restarter:
        Option<Arc<dyn nexo_core::agent::admin_rpc::domains::plugin_restart::PluginRestarter>>,
}

impl WhatsappPersister {
    /// Build the persister with the daemon's directory layout
    /// snapshot. `config_dir` MUST point at the directory whose
    /// `plugins/whatsapp.yaml` the plugin runtime reads.
    pub fn new(
        config_dir: PathBuf,
        data_dir: PathBuf,
        plugin_handles: crate::admin_adapters::SharedPluginHandles,
    ) -> Arc<Self> {
        Arc::new(Self {
            config_dir,
            data_dir,
            plugin_handles,
            restarter: None,
        })
    }

    /// Phase 97.2 — wire the supervisor restart hook. Constructor
    /// returns `Arc<Self>` so we can't `&mut` after the fact; this
    /// helper rebuilds the Arc with the restarter installed. Cheap
    /// enough since the persister is only constructed once at boot.
    pub fn with_restarter(
        self: Arc<Self>,
        restarter: Arc<dyn nexo_core::agent::admin_rpc::domains::plugin_restart::PluginRestarter>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config_dir: self.config_dir.clone(),
            data_dir: self.data_dir.clone(),
            plugin_handles: self.plugin_handles.clone(),
            restarter: Some(restarter),
        })
    }

    fn yaml_path(&self) -> PathBuf {
        self.config_dir.join("plugins").join("whatsapp.yaml")
    }

    fn session_dir_for(&self, instance: &str) -> String {
        self.data_dir
            .join("whatsapp")
            .join(instance)
            .join("session")
            .to_string_lossy()
            .into_owned()
    }

    fn media_dir_for(&self, instance: &str) -> String {
        self.data_dir
            .join("whatsapp")
            .join(instance)
            .join("media")
            .to_string_lossy()
            .into_owned()
    }

    /// Re-read the yaml + send `plugin.configure(value)` to the
    /// WhatsApp plugin handle if it's already booted. No-op when
    /// the handle cell is empty (daemon still booting).
    async fn push_configure_to_live_plugin(
        &self,
        yaml_path: &std::path::Path,
    ) -> anyhow::Result<()> {
        // Map yaml `whatsapp: [...]` → bare sequence so the
        // plugin's `on_configure` deserialises into
        // `Vec<WhatsappPluginConfig>`.
        let raw = std::fs::read_to_string(yaml_path)?;
        let resolved =
            nexo_config::env::resolve_placeholders(&raw, &yaml_path.display().to_string())?;
        let parsed: serde_yaml::Value = serde_yaml::from_str(&resolved)?;
        let inner = parsed
            .get("whatsapp")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("whatsapp.yaml missing top-level `whatsapp` key"))?;

        let guard = self.plugin_handles.read().await;
        let Some(handles_arc) = guard.as_ref() else {
            anyhow::bail!("plugin_handles cell not yet populated (daemon still booting)")
        };
        for (id, handle) in handles_arc.iter() {
            if id == "whatsapp" || id.starts_with("whatsapp.") {
                if let Err(e) = handle.configure(&inner).await {
                    tracing::warn!(
                        target: "persister.whatsapp",
                        plugin_id = %id,
                        error = %e,
                        "handle.configure rejected new value"
                    );
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ChannelCredentialPersister for WhatsappPersister {
    fn channel(&self) -> &str {
        "whatsapp"
    }

    fn validate_shape(
        &self,
        _payload: &Value,
        _metadata: &HashMap<String, Value>,
    ) -> Result<(), AdminRpcError> {
        // Accept any shape — pairing flow does the real
        // validation when the operator scans the QR.
        Ok(())
    }

    async fn persist(
        &self,
        _instance: Option<&str>,
        _payload: &Value,
        _metadata: &HashMap<String, Value>,
    ) -> Result<(), AdminRpcError> {
        // Intentional no-op: pairing flow writes its own state.
        Ok(())
    }

    async fn revoke(&self, _instance: Option<&str>) -> Result<bool, AdminRpcError> {
        // No-op: pairing flow has its own revoke surface
        // (`nexo/admin/pairing/cancel`). Returning false signals
        // "nothing changed at the persister layer" so the
        // dispatcher uses the opaque-store delete result alone
        // to decide whether to fire the reload.
        Ok(false)
    }

    async fn probe(
        &self,
        _instance: Option<&str>,
        _payload: &Value,
        _metadata: &HashMap<String, Value>,
    ) -> CredentialValidationOutcome {
        CredentialValidationOutcome {
            probed: false,
            healthy: false,
            detail: Some(
                "whatsapp session health is reported via nexo/notify/pairing_status_changed".into(),
            ),
            reason_code: Some(reason_code::NOT_PROBED.into()),
        }
    }

    /// Stamp the operator-chosen `instance` label into
    /// `plugins/whatsapp.yaml`. Refuses an absent or empty
    /// label — multi-account WhatsApp REQUIRES a discriminator
    /// so the plugin can publish on a per-instance topic and
    /// the agent's `inbound_bindings[].instance` lines up.
    async fn on_pair_success(&self, instance: Option<&str>) -> Result<(), AdminRpcError> {
        let inst = instance.map(str::trim).unwrap_or("");
        if inst.is_empty() {
            return Err(AdminRpcError::InvalidParams(
                "whatsapp pair requires a non-empty `instance` label so the plugin's \
                 `plugin.inbound.whatsapp.<instance>` topic matches the agent binding"
                    .into(),
            ));
        }
        let yaml_path = self.yaml_path();
        let session_dir = self.session_dir_for(inst);
        let media_dir = self.media_dir_for(inst);
        crate::yaml_patch::whatsapp_upsert_instance(
            &yaml_path,
            inst,
            &session_dir,
            &media_dir,
            &[],
        )
        .map_err(|e| {
            AdminRpcError::Internal(format!(
                "whatsapp_upsert_instance({}): {e}",
                yaml_path.display()
            ))
        })?;
        tracing::info!(
            target: "persister.whatsapp",
            instance = %inst,
            yaml = %yaml_path.display(),
            "wrote whatsapp.yaml entry post-pair"
        );

        // Phase 97 — push the new yaml content to the live
        // plugin subprocess so its `configured_state` includes
        // the just-written instance BEFORE the pairing pump
        // calls back into `pairing_admin::pairing_start` (which
        // resolves `session_dir` from that state). Without this
        // the pump returns `instance not configured` for the
        // operator-chosen label.
        if let Err(e) = self.push_configure_to_live_plugin(&yaml_path).await {
            tracing::warn!(
                target: "persister.whatsapp",
                instance = %inst,
                error = %e,
                "plugin.configure push failed; pairing pump may not see new instance"
            );
        }

        // Phase 97.2 — supervisor-driven re-launch. The subprocess
        // plugin's `start()` runs once at boot, so a configure push
        // alone leaves the Signal session loop pinned to whatever
        // device was active when the subprocess spawned. Restart so
        // the child re-reads `whatsapp.yaml` and brings the new
        // instance online without a daemon restart. Idempotent —
        // missing restarter (tests / minimal mode) skips silently.
        // Restart failure logs as warn so the operator sees why a
        // freshly-paired device might still be 401-looping but does
        // NOT fail the `credentials/register` RPC (yaml write
        // already succeeded; manual `pkill nexo` is a fallback).
        if let Some(restarter) = self.restarter.as_ref() {
            match restarter.restart("whatsapp").await {
                Ok(resp) => tracing::info!(
                    target: "persister.whatsapp",
                    instance = %inst,
                    previous_uptime_ms = resp.previous_uptime_ms,
                    "supervisor restart fired post-pair"
                ),
                Err(e) => tracing::warn!(
                    target: "persister.whatsapp",
                    instance = %inst,
                    error = %e,
                    "supervisor restart post-pair failed; new device may not connect until manual restart"
                ),
            }
        } else {
            tracing::debug!(
                target: "persister.whatsapp",
                instance = %inst,
                "no PluginRestarter wired; skipping post-pair restart"
            );
        }
        Ok(())
    }

    /// Undo a previous `on_pair_success` write when the pairing
    /// pump refused to start. Reads `whatsapp.yaml`, filters out
    /// the entry whose `instance` matches, persists. Returns
    /// `Ok(false)` when no matching entry exists (idempotent).
    async fn rollback_pair(&self, instance: Option<&str>) -> Result<bool, AdminRpcError> {
        let Some(inst) = instance.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(false);
        };
        let yaml_path = self.yaml_path();
        if !yaml_path.exists() {
            return Ok(false);
        }
        let text = std::fs::read_to_string(&yaml_path)
            .map_err(|e| AdminRpcError::Internal(format!("read {}: {e}", yaml_path.display())))?;
        let mut root: serde_yaml::Value = serde_yaml::from_str(&text)
            .map_err(|e| AdminRpcError::Internal(format!("parse {}: {e}", yaml_path.display())))?;
        let map = match root.as_mapping_mut() {
            Some(m) => m,
            None => return Ok(false),
        };
        let key = serde_yaml::Value::String("whatsapp".into());
        let removed = match map.get_mut(&key) {
            Some(serde_yaml::Value::Sequence(seq)) => {
                let before = seq.len();
                seq.retain(|v| v.get("instance").and_then(|i| i.as_str()) != Some(inst));
                before != seq.len()
            }
            _ => false,
        };
        if !removed {
            return Ok(false);
        }
        let serialized = serde_yaml::to_string(&root).map_err(|e| {
            AdminRpcError::Internal(format!("serialize {}: {e}", yaml_path.display()))
        })?;
        std::fs::write(&yaml_path, serialized)
            .map_err(|e| AdminRpcError::Internal(format!("write {}: {e}", yaml_path.display())))?;
        tracing::info!(
            target: "persister.whatsapp",
            instance = %inst,
            yaml = %yaml_path.display(),
            "rolled back whatsapp.yaml entry after failed pair"
        );
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn persister() -> Arc<WhatsappPersister> {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let data = dir.path().join("data");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        // Leak the tempdir: tests only need the paths to exist
        // for the duration of the call; persister doesn't
        // outlive the tempdir guard inside the test.
        let p = WhatsappPersister::new(
            config,
            data,
            crate::admin_adapters::shared_plugin_handles_cell(),
        );
        // dir dropped at end of fn — but the persister's
        // PathBufs stay valid (they don't reach the fs unless
        // on_pair_success runs, which the relevant tests
        // exercise explicitly).
        std::mem::forget(dir);
        p
    }

    #[test]
    fn channel_id_is_whatsapp() {
        assert_eq!(persister().channel(), "whatsapp");
    }

    #[test]
    fn validate_shape_accepts_anything() {
        let p = persister();
        p.validate_shape(&json!(null), &HashMap::new()).unwrap();
        p.validate_shape(&json!({ "anything": "goes" }), &HashMap::new())
            .unwrap();
    }

    #[tokio::test]
    async fn persist_is_noop() {
        let p = persister();
        p.persist(Some("personal"), &json!({}), &HashMap::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn revoke_returns_false() {
        let p = persister();
        let removed = p.revoke(Some("personal")).await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn probe_returns_not_probed_with_explanation() {
        let p = persister();
        let outcome = p.probe(None, &json!({}), &HashMap::new()).await;
        assert!(!outcome.probed);
        assert!(!outcome.healthy);
        assert_eq!(
            outcome.reason_code.as_deref(),
            Some(reason_code::NOT_PROBED)
        );
        assert!(outcome.detail.is_some());
    }
}
