//! Phase 82.10.h.3 — production adapters wiring the core admin
//! RPC domain traits to the on-disk yaml + secrets surfaces.
//!
//! Each adapter resolves the cycle that motivated its trait:
//! `nexo-core` declares the trait, `nexo-setup` implements it (and
//! is the only crate that may depend on `crate::yaml_patch`).
//!
//! Wired in main.rs via `AdminRpcDispatcher::with_*_domain`:
//!
//! ```ignore
//! let yaml = AgentsYamlPatcher::new(config.join("agents.yaml"));
//! let llm  = LlmYamlPatcherFs::new(config.join("llm.yaml"));
//! let cred = FilesystemCredentialStore::new(secrets_root);
//! dispatcher
//!     .with_agents_domain(yaml.clone())
//!     .with_credentials_domain(yaml, cred)
//!     .with_llm_providers_domain(llm);
//! ```
//!
//! Pairing challenge store + notifier are deferred to 82.10.h.b
//! because both need new infrastructure (a fresh challenge SQLite
//! schema for the QR state machine + a stdio-bridged notifier
//! that ties into the main JSON-RPC writer in src/main.rs).

use std::path::{Path, PathBuf};

use nexo_core::agent::admin_rpc::domains::agents::YamlPatcher;
use nexo_core::agent::admin_rpc::domains::credentials::CredentialStore;
use nexo_core::agent::admin_rpc::domains::llm_providers::LlmYamlPatcher;

use crate::yaml_patch;

/// `agents.yaml` adapter for the agents + credentials admin
/// domains. Holds the path; every method opens the file fresh so
/// the writer stays consistent with operator-side edits made
/// outside the admin RPC surface (e.g. wizard, hand edits).
#[derive(Debug, Clone)]
pub struct AgentsYamlPatcher {
    path: PathBuf,
}

impl AgentsYamlPatcher {
    /// Path to the operator-managed `agents.yaml`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl YamlPatcher for AgentsYamlPatcher {
    fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        yaml_patch::list_agent_ids(&self.path)
    }

    fn read_agent_field(
        &self,
        agent_id: &str,
        dotted: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        match yaml_patch::read_agent_field(&self.path, agent_id, dotted)? {
            Some(yaml_value) => Ok(Some(yaml_to_json(yaml_value)?)),
            None => Ok(None),
        }
    }

    fn upsert_agent_field(
        &self,
        agent_id: &str,
        dotted: &str,
        value: serde_json::Value,
    ) -> anyhow::Result<()> {
        let yaml_value = json_to_yaml(value)?;
        yaml_patch::upsert_agent_field(&self.path, agent_id, dotted, yaml_value)
    }

    fn remove_agent(&self, agent_id: &str) -> anyhow::Result<()> {
        yaml_patch::remove_agent_block(&self.path, agent_id)?;
        Ok(())
    }
}

/// `llm.yaml` adapter for the llm_providers admin domain. Same
/// fresh-open semantics as the agents adapter.
#[derive(Debug, Clone)]
pub struct LlmYamlPatcherFs {
    path: PathBuf,
}

impl LlmYamlPatcherFs {
    /// Path to the operator-managed `llm.yaml`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl LlmYamlPatcher for LlmYamlPatcherFs {
    fn list_provider_ids(&self) -> anyhow::Result<Vec<String>> {
        yaml_patch::list_llm_provider_ids(&self.path)
    }

    fn read_provider_field(
        &self,
        provider_id: &str,
        dotted: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        match yaml_patch::read_llm_provider_field(&self.path, provider_id, dotted)? {
            Some(yaml_value) => Ok(Some(yaml_to_json(yaml_value)?)),
            None => Ok(None),
        }
    }

    fn upsert_provider_field(
        &self,
        provider_id: &str,
        dotted: &str,
        value: serde_json::Value,
    ) -> anyhow::Result<()> {
        let yaml_value = json_to_yaml(value)?;
        yaml_patch::upsert_llm_provider_field(&self.path, provider_id, dotted, yaml_value)
    }

    fn remove_provider(&self, provider_id: &str) -> anyhow::Result<()> {
        yaml_patch::remove_llm_provider(&self.path, provider_id)?;
        Ok(())
    }
}

/// Filesystem-backed `CredentialStore` rooted at `secrets/`.
/// Layout: `secrets/<channel>/[<instance>/]payload.json`. The
/// `<instance>` segment is omitted for `instance = None`. Atomic
/// writes via `tempfile::NamedTempFile::persist` so a partial
/// crash never leaves a half-written payload on disk.
#[derive(Debug, Clone)]
pub struct FilesystemCredentialStore {
    secrets_root: PathBuf,
}

impl FilesystemCredentialStore {
    /// Build a store rooted at `secrets_root`. Created on first
    /// write — does not fail when the directory is absent.
    pub fn new(secrets_root: impl Into<PathBuf>) -> Self {
        Self {
            secrets_root: secrets_root.into(),
        }
    }

    fn payload_path(&self, channel: &str, instance: Option<&str>) -> PathBuf {
        let mut p = self.secrets_root.join(channel);
        if let Some(i) = instance {
            p.push(i);
        }
        p.push("payload.json");
        p
    }
}

impl CredentialStore for FilesystemCredentialStore {
    fn list_credentials(&self) -> anyhow::Result<Vec<(String, Option<String>)>> {
        if !self.secrets_root.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for channel_entry in std::fs::read_dir(&self.secrets_root)? {
            let channel_entry = channel_entry?;
            if !channel_entry.file_type()?.is_dir() {
                continue;
            }
            let channel = channel_entry.file_name().to_string_lossy().into_owned();
            // Direct payload (no instance).
            if channel_entry.path().join("payload.json").exists() {
                out.push((channel.clone(), None));
            }
            // Per-instance payloads.
            for inst_entry in std::fs::read_dir(channel_entry.path())? {
                let inst_entry = inst_entry?;
                if !inst_entry.file_type()?.is_dir() {
                    continue;
                }
                if inst_entry.path().join("payload.json").exists() {
                    let instance = inst_entry.file_name().to_string_lossy().into_owned();
                    out.push((channel.clone(), Some(instance)));
                }
            }
        }
        Ok(out)
    }

    fn write_credential(
        &self,
        channel: &str,
        instance: Option<&str>,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        if channel.is_empty() {
            anyhow::bail!("channel must not be empty");
        }
        let target = self.payload_path(channel, instance);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec_pretty(payload)?;
        write_atomic_bytes(&target, &body)
    }

    fn delete_credential(
        &self,
        channel: &str,
        instance: Option<&str>,
    ) -> anyhow::Result<bool> {
        let target = self.payload_path(channel, instance);
        if !target.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&target)?;
        // Best-effort dir prune so `list_credentials` doesn't see
        // the empty parent. Ignore errors — non-empty dirs are
        // legitimate.
        if let Some(parent) = target.parent() {
            let _ = std::fs::remove_dir(parent);
            if let Some(channel_dir) = parent.parent() {
                if channel_dir != self.secrets_root {
                    let _ = std::fs::remove_dir(channel_dir);
                }
            }
        }
        Ok(true)
    }
}

// ── Internal helpers ────────────────────────────────────────────

fn yaml_to_json(yaml_value: serde_yaml::Value) -> anyhow::Result<serde_json::Value> {
    serde_json::to_value(yaml_value).map_err(Into::into)
}

fn json_to_yaml(json_value: serde_json::Value) -> anyhow::Result<serde_yaml::Value> {
    serde_yaml::to_value(json_value).map_err(Into::into)
}

fn write_atomic_bytes(path: &Path, body: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        anyhow::anyhow!("credential payload path has no parent: {}", path.display())
    })?;
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        use std::io::Write;
        let mut f = tmp.as_file();
        f.write_all(body)?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("persist credential {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_yaml(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn agents_adapter_lists_ids_from_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "agents.yaml",
            "agents:\n  - id: ana\n    model:\n      provider: minimax\n  - id: bob\n    model:\n      provider: anthropic\n",
        );
        let adapter = AgentsYamlPatcher::new(path);
        let mut ids = adapter.list_agent_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec!["ana".to_string(), "bob".into()]);
    }

    #[test]
    fn agents_adapter_returns_empty_list_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = AgentsYamlPatcher::new(dir.path().join("missing.yaml"));
        assert!(adapter.list_agent_ids().unwrap().is_empty());
    }

    #[test]
    fn agents_adapter_read_and_upsert_field_roundtrip_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "agents.yaml",
            "agents:\n  - id: ana\n    model:\n      provider: minimax\n      model: M\n",
        );
        let adapter = AgentsYamlPatcher::new(path);
        let v = adapter
            .read_agent_field("ana", "model.provider")
            .unwrap()
            .unwrap();
        assert_eq!(v.as_str(), Some("minimax"));

        adapter
            .upsert_agent_field("ana", "language", json!("es"))
            .unwrap();
        let lang = adapter
            .read_agent_field("ana", "language")
            .unwrap()
            .unwrap();
        assert_eq!(lang.as_str(), Some("es"));

        // Round-trip a complex JSON object (inbound_bindings).
        let bindings = json!([{ "plugin": "whatsapp", "instance": "personal" }]);
        adapter
            .upsert_agent_field("ana", "inbound_bindings", bindings.clone())
            .unwrap();
        let read_back = adapter
            .read_agent_field("ana", "inbound_bindings")
            .unwrap()
            .unwrap();
        assert_eq!(read_back, bindings);
    }

    #[test]
    fn agents_adapter_remove_agent_drops_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "agents.yaml",
            "agents:\n  - id: ana\n    model:\n      provider: minimax\n  - id: bob\n    model:\n      provider: anthropic\n",
        );
        let adapter = AgentsYamlPatcher::new(path);
        adapter.remove_agent("ana").unwrap();
        assert_eq!(adapter.list_agent_ids().unwrap(), vec!["bob".to_string()]);
        // Idempotent re-removal.
        adapter.remove_agent("ana").unwrap();
        assert_eq!(adapter.list_agent_ids().unwrap(), vec!["bob".to_string()]);
    }

    #[test]
    fn llm_adapter_lists_provider_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "llm.yaml",
            "providers:\n  minimax:\n    base_url: https://x\n  anthropic:\n    base_url: https://y\n",
        );
        let adapter = LlmYamlPatcherFs::new(path);
        let mut ids = adapter.list_provider_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec!["anthropic".to_string(), "minimax".into()]);
    }

    #[test]
    fn llm_adapter_upsert_creates_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm.yaml");
        let adapter = LlmYamlPatcherFs::new(path.clone());
        adapter
            .upsert_provider_field("newp", "base_url", json!("https://x"))
            .unwrap();
        adapter
            .upsert_provider_field("newp", "api_key_env", json!("MY_KEY"))
            .unwrap();
        let v = adapter
            .read_provider_field("newp", "api_key_env")
            .unwrap()
            .unwrap();
        assert_eq!(v.as_str(), Some("MY_KEY"));
    }

    #[test]
    fn llm_adapter_remove_provider_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "llm.yaml",
            "providers:\n  retired:\n    base_url: https://x\n",
        );
        let adapter = LlmYamlPatcherFs::new(path);
        adapter.remove_provider("retired").unwrap();
        adapter.remove_provider("retired").unwrap();
        assert!(adapter.list_provider_ids().unwrap().is_empty());
    }

    #[test]
    fn credential_store_write_then_read_back_via_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::new(dir.path().join("secrets"));
        store
            .write_credential("whatsapp", Some("personal"), &json!({ "token": "wa.X" }))
            .unwrap();
        store
            .write_credential("whatsapp", Some("business"), &json!({ "token": "wa.Y" }))
            .unwrap();
        let mut creds = store.list_credentials().unwrap();
        creds.sort();
        assert_eq!(
            creds,
            vec![
                ("whatsapp".to_string(), Some("business".to_string())),
                ("whatsapp".to_string(), Some("personal".to_string())),
            ]
        );
    }

    #[test]
    fn credential_store_delete_returns_true_then_false() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::new(dir.path().join("secrets"));
        store
            .write_credential("whatsapp", Some("p"), &json!({}))
            .unwrap();
        assert!(store.delete_credential("whatsapp", Some("p")).unwrap());
        assert!(!store.delete_credential("whatsapp", Some("p")).unwrap());
    }

    #[test]
    fn credential_store_supports_no_instance() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::new(dir.path().join("secrets"));
        store
            .write_credential("email", None, &json!({ "user": "x@y" }))
            .unwrap();
        let creds = store.list_credentials().unwrap();
        assert_eq!(creds, vec![("email".to_string(), None)]);
        assert!(store.delete_credential("email", None).unwrap());
    }

    #[test]
    fn credential_store_write_rejects_empty_channel() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::new(dir.path().join("secrets"));
        let err = store
            .write_credential("", None, &json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("channel must not be empty"));
    }

    #[test]
    fn credential_store_list_on_missing_root_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::new(dir.path().join("never_created"));
        assert!(store.list_credentials().unwrap().is_empty());
    }
}
