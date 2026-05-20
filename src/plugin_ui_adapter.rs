//! Phase 99.5 — concrete adapters wiring the `plugin_ui` admin RPC
//! domain into the daemon boot.
//!
//! The domain
//! ([`nexo_core::agent::admin_rpc::domains::plugin_ui::PluginUiDomain`])
//! depends on the live plugin registry, which `wire_plugin_registry`
//! creates AFTER the admin dispatcher is built + served. Since the
//! dispatcher is immutable once built, the registry-backed adapters
//! share a [`PluginUiRegistryCell`] (`OnceLock`) with the boot site:
//! built empty, filled post-wire. Until filled, `list` returns an
//! empty set and trust resolves to `Unverified` — a clean boot
//! window (mirrors the `plugin_installer` late-bind pattern).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;

use nexo_broker::AnyBroker;
use nexo_core::agent::admin_rpc::domains::plugin_ui::{
    PluginConfigStore, PluginRpcForwarder, PluginTrustResolver, PluginUiManifestView,
    PluginUiRegistry,
};
use nexo_core::agent::nexo_plugin_registry::NexoPluginRegistry;
use nexo_plugin_manifest::admin::PluginAdminSection;
use nexo_plugin_manifest::PluginManifest;
use nexo_tool_meta::admin::plugin_discovery::TrustTier;

/// Shared late-bind handle for the plugin registry. Filled by the
/// boot site after `wire_plugin_registry` returns.
pub type PluginUiRegistryCell = Arc<OnceLock<Arc<NexoPluginRegistry>>>;

/// `true` for a filesystem-safe plugin id (matches the manifest id
/// regex subset; blocks path traversal).
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Build a [`PluginUiManifestView`] from a manifest, or `None` when
/// the plugin declares no `[plugin.admin_ui]`.
fn view_of(m: &PluginManifest) -> Option<PluginUiManifestView> {
    let admin_ui = m.plugin.admin_ui.clone()?;
    Some(PluginUiManifestView {
        id: m.plugin.id.clone(),
        name: m.plugin.name.clone(),
        admin_ui,
        admin: m.plugin.admin.clone(),
        config_schema: m
            .plugin
            .config_schema
            .as_ref()
            .and_then(|cs| serde_json::from_str::<Value>(&cs.schema).ok()),
    })
}

// ── registry adapter ─────────────────────────────────────────────

/// [`PluginUiRegistry`] backed by the live [`NexoPluginRegistry`]
/// via a late-bind cell.
pub struct LivePluginUiRegistry {
    cell: PluginUiRegistryCell,
}

impl LivePluginUiRegistry {
    pub fn new(cell: PluginUiRegistryCell) -> Self {
        Self { cell }
    }
}

impl PluginUiRegistry for LivePluginUiRegistry {
    fn admin_ui_views(&self) -> Vec<PluginUiManifestView> {
        let Some(reg) = self.cell.get() else {
            return Vec::new();
        };
        reg.snapshot()
            .plugins
            .iter()
            .filter_map(|p| view_of(&p.manifest))
            .collect()
    }

    fn admin_ui_view(&self, plugin_id: &str) -> Option<PluginUiManifestView> {
        let reg = self.cell.get()?;
        reg.snapshot()
            .plugins
            .iter()
            .find(|p| p.manifest.plugin.id == plugin_id)
            .and_then(|p| view_of(&p.manifest))
    }
}

// ── config store adapter ─────────────────────────────────────────

/// [`PluginConfigStore`] over `<config_dir>/plugins/<id>.yaml`.
///
/// Writes the WRAPPED shape (`{id: <value>}`) so the loader's
/// single-key-strip rule (`nexo_config::load_plugin_entries`)
/// round-trips deterministically regardless of the config body.
pub struct FsPluginConfigStore {
    plugins_dir: PathBuf,
}

impl FsPluginConfigStore {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            plugins_dir: config_dir.join("plugins"),
        }
    }

    fn path_for(&self, id: &str) -> anyhow::Result<PathBuf> {
        if !safe_id(id) {
            anyhow::bail!("unsafe plugin id `{id}`");
        }
        Ok(self.plugins_dir.join(format!("{id}.yaml")))
    }
}

/// Strip a single outer `{stem: value}` wrapper, mirroring the
/// loader (`crates/config/src/lib.rs` load_plugin_entries).
fn strip_wrapper(value: serde_yaml::Value, stem: &str) -> serde_yaml::Value {
    if let serde_yaml::Value::Mapping(map) = &value {
        if map.len() == 1 {
            if let Some(inner) = map.get(serde_yaml::Value::String(stem.to_string())) {
                return inner.clone();
            }
        }
    }
    value
}

impl PluginConfigStore for FsPluginConfigStore {
    fn read(&self, plugin_id: &str) -> anyhow::Result<Option<Value>> {
        let path = self.path_for(plugin_id)?;
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        let yaml: serde_yaml::Value = serde_yaml::from_str(&text)?;
        let stripped = strip_wrapper(yaml, plugin_id);
        let json: Value = serde_json::to_value(stripped)?;
        Ok(Some(json))
    }

    fn write(&self, plugin_id: &str, config: &Value) -> anyhow::Result<()> {
        let path = self.path_for(plugin_id)?;
        std::fs::create_dir_all(&self.plugins_dir)?;
        let inner: serde_yaml::Value = serde_yaml::to_value(config)?;
        let mut map = serde_yaml::Mapping::new();
        map.insert(serde_yaml::Value::String(plugin_id.to_string()), inner);
        let text = serde_yaml::to_string(&serde_yaml::Value::Mapping(map))?;
        // Atomic write: tmp + rename within the same dir.
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

// ── broker forwarder adapter ─────────────────────────────────────

/// [`PluginRpcForwarder`] over the broker, reusing
/// [`nexo_pairing::plugin_admin::forward_request`].
pub struct BrokerPluginRpcForwarder {
    broker: AnyBroker,
}

impl BrokerPluginRpcForwarder {
    pub fn new(broker: AnyBroker) -> Self {
        Self { broker }
    }
}

#[async_trait::async_trait]
impl PluginRpcForwarder for BrokerPluginRpcForwarder {
    async fn forward(
        &self,
        admin: &PluginAdminSection,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let info = nexo_pairing::plugin_admin::MatchInfo {
            plugin_id: String::new(),
            broker_topic_prefix: admin.broker_topic_prefix.clone(),
            method_prefix: admin.method_prefix.clone(),
            timeout: Duration::from_secs(admin.timeout_seconds.unwrap_or(30)),
        };
        let resp = nexo_pairing::plugin_admin::forward_request(&self.broker, info, method, params)
            .await
            .map_err(|e| e.to_string())?;
        if resp.ok {
            Ok(resp.result)
        } else {
            Err(resp.error)
        }
    }
}

// ── trust resolver adapter ───────────────────────────────────────

/// [`PluginTrustResolver`] keyed on the plugin's `[plugin.meta]`
/// author vs the operator's `TrustedKeysConfig.authors` allowlist.
/// `Official` when the author is trusted, else `Unverified`.
/// `CommunityIndexed` (curated-index membership) is deferred — see
/// FOLLOWUPS Phase 99 trust tiers.
pub struct TrustedKeysPluginTrustResolver {
    owners: BTreeSet<String>,
    cell: PluginUiRegistryCell,
}

impl TrustedKeysPluginTrustResolver {
    /// Load the trusted-author allowlist from `config_dir`
    /// (best-effort — empty on missing/invalid file).
    pub fn from_config_dir(config_dir: &Path, cell: PluginUiRegistryCell) -> Self {
        let owners = nexo_ext_installer::TrustedKeysConfig::load(config_dir)
            .map(|t| t.authors.into_iter().map(|a| a.owner).collect())
            .unwrap_or_default();
        Self { owners, cell }
    }
}

impl PluginTrustResolver for TrustedKeysPluginTrustResolver {
    fn trust_of(&self, plugin_id: &str) -> TrustTier {
        let Some(reg) = self.cell.get() else {
            return TrustTier::Unverified;
        };
        let author = reg
            .snapshot()
            .plugins
            .iter()
            .find(|p| p.manifest.plugin.id == plugin_id)
            .and_then(|p| p.manifest.plugin.meta.author.clone());
        match author {
            Some(a) if self.owners.contains(&a) => TrustTier::Official,
            _ => TrustTier::Unverified,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn safe_id_accepts_plugin_ids_rejects_traversal() {
        assert!(safe_id("google"));
        assert!(safe_id("web_search"));
        assert!(!safe_id(""));
        assert!(!safe_id("../etc/passwd"));
        assert!(!safe_id("a/b"));
        assert!(!safe_id("UPPER"));
    }

    #[test]
    fn config_store_write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsPluginConfigStore::new(dir.path());
        let cfg = json!({"host": "smtp.gmail.com", "port": 587, "use_tls": true});
        store.write("google", &cfg).unwrap();
        let back = store.read("google").unwrap().unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn config_store_writes_wrapped_shape() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsPluginConfigStore::new(dir.path());
        store.write("google", &json!({"x": 1})).unwrap();
        let text = std::fs::read_to_string(dir.path().join("plugins/google.yaml")).unwrap();
        // Outer wrapper key == plugin id.
        assert!(
            text.starts_with("google:"),
            "expected wrapped shape, got:\n{text}"
        );
    }

    #[test]
    fn config_store_read_strips_wrapper_only_for_matching_stem() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
        // Single-key map whose key == stem → stripped.
        std::fs::write(
            dir.path().join("plugins/google.yaml"),
            "google:\n  host: x\n",
        )
        .unwrap();
        let store = FsPluginConfigStore::new(dir.path());
        let v = store.read("google").unwrap().unwrap();
        assert_eq!(v, json!({"host": "x"}));

        // Single-key map whose key != stem → kept verbatim.
        std::fs::write(dir.path().join("plugins/other.yaml"), "host: y\n").unwrap();
        let v2 = store.read("other").unwrap().unwrap();
        assert_eq!(v2, json!({"host": "y"}));
    }

    #[test]
    fn config_store_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsPluginConfigStore::new(dir.path());
        assert!(store.read("ghost").unwrap().is_none());
    }

    #[test]
    fn config_store_rejects_unsafe_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsPluginConfigStore::new(dir.path());
        assert!(store.write("../evil", &json!({})).is_err());
        assert!(store.read("a/b").is_err());
    }

    #[test]
    fn trust_resolver_unverified_when_cell_empty() {
        let cell: PluginUiRegistryCell = Arc::new(OnceLock::new());
        let r = TrustedKeysPluginTrustResolver {
            owners: BTreeSet::new(),
            cell,
        };
        assert_eq!(r.trust_of("google"), TrustTier::Unverified);
    }
}
