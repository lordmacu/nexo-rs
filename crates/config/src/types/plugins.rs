use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use serde_yaml::Value;

#[derive(Debug, Default, Clone)]
pub struct PluginsConfig {
    /// Phase 93.3 — opaque per-plugin config slice keyed by
    /// plugin id (matches `manifest.plugin.id`). Populated by
    /// `load_plugins` from `<config_dir>/plugins/<id>.yaml` (flat
    /// file layout). Empty map = no plugin configs supplied.
    ///
    /// Each entry is the operator's YAML AST with the outer
    /// wrapper key stripped when it matches the filename stem.
    /// Plugins consume via `NexoPlugin::configure(value)`
    /// (Phase 93.2); typed fields below stay populated through
    /// the Phase 93.5 deprecation window.
    pub entries: BTreeMap<String, Value>,

    // Wave 7 — typed whatsapp field dropped. Cfg lives in
    // `entries["whatsapp"]` opaque map; consumers deserialize the
    // slice locally (nexo-auth wire.rs + main.rs pairing init).
    // Wave 6 — typed telegram field dropped. Cfg lives in
    // `entries["telegram"]` opaque map; consumers deserialize the
    // slice locally (nexo-auth wire.rs, factory wiring in main.rs).
    // Wave 5 — typed email field dropped. Email cfg lives in the
    // opaque `entries["email"]` map (Phase 93.3). Consumers
    // (nexo-auth, autonomous worker) deserialize the slice locally.
    // The plugin owns the canonical typed shape.
    pub browser: Option<BrowserConfig>,
    /// Operator-configured plugin discovery walk knobs.
    /// Loaded from `<config_dir>/plugins/discovery.yaml` (optional —
    /// missing file means defaults: empty search_paths so nothing is
    /// scanned; legacy plugins continue to register through the
    /// hardcoded boot loop).
    pub discovery: PluginDiscoveryConfig,
}

impl PluginsConfig {
    /// Phase 93.5.a — opaque accessor returning the raw YAML
    /// slice for `plugin_id`, or `None` when the operator did
    /// not declare that plugin. Daemon-native channels (whatsapp,
    /// telegram, email, browser) still expose typed fields above
    /// for legacy consumers; new plugins (slack, discord, sms)
    /// land only here.
    pub fn entries_for(&self, plugin_id: &str) -> Option<&Value> {
        self.entries.get(plugin_id)
    }

    /// Phase 93.5.a — every declared plugin id, sorted (BTreeMap).
    pub fn plugin_ids(&self) -> impl Iterator<Item = &String> + '_ {
        self.entries.keys()
    }

    /// Phase 93.5.e — extract `instance` field values from the
    /// plugin's array-shaped YAML. Returns an empty vec when the
    /// plugin is absent, single-instance (Mapping), or has no
    /// `instance` fields. Used by the daemon's per-plugin binding
    /// validator so a new array-shape plugin (slack/discord/sms)
    /// participates without daemon-side typed access.
    ///
    /// Convention: array-shape plugin entries identify themselves
    /// via an `instance: <name>` field (WhatsApp + Telegram +
    /// Email accounts follow this pattern). Plugins that don't
    /// declare a manifest `[plugin.config_schema] shape = "array"`
    /// produce no instances here.
    pub fn instances_for(&self, plugin_id: &str) -> Vec<String> {
        let Some(value) = self.entries.get(plugin_id) else {
            return Vec::new();
        };
        let Value::Sequence(seq) = value else {
            return Vec::new();
        };
        seq.iter()
            .filter_map(|entry| {
                entry
                    .as_mapping()?
                    .get(Value::String("instance".to_string()))?
                    .as_str()
                    .map(|s| s.to_string())
            })
            .collect()
    }

    /// Phase 93.5.a — uniform presence check. Returns `true` when
    /// the operator declared this plugin AND the declaration is
    /// non-empty (single-instance object OR non-empty array).
    /// Replaces the per-channel `cfg.plugins.X.is_empty()` /
    /// `cfg.plugins.X.is_some()` idioms so a new plugin (slack/
    /// discord/sms) participates without daemon-side typed access.
    pub fn is_active(&self, plugin_id: &str) -> bool {
        let Some(value) = self.entries.get(plugin_id) else {
            return false;
        };
        match value {
            // Single-instance object (email, browser, discovery shape).
            Value::Mapping(m) => !m.is_empty(),
            // Multi-instance array (whatsapp, telegram shape).
            Value::Sequence(s) => !s.is_empty(),
            // Null / explicit empty → inactive.
            Value::Null => false,
            // Other primitives (bool, number, string) — declared,
            // counted as active so the caller can decide how to
            // interpret. Plugin manifests own validation.
            _ => true,
        }
    }
}

#[cfg(test)]
mod plugins_config_helpers_tests {
    use super::*;

    fn entries_from(yaml: &str) -> std::collections::BTreeMap<String, Value> {
        let v: Value = serde_yaml::from_str(yaml).unwrap();
        v.as_mapping()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str().unwrap().to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn is_active_returns_true_for_non_empty_array() {
        let cfg = PluginsConfig {
            entries: entries_from("whatsapp:\n  - instance: main\n"),
            ..PluginsConfig::default()
        };
        assert!(cfg.is_active("whatsapp"));
    }

    #[test]
    fn is_active_returns_false_for_empty_array() {
        let cfg = PluginsConfig {
            entries: entries_from("telegram: []\n"),
            ..PluginsConfig::default()
        };
        assert!(!cfg.is_active("telegram"));
    }

    #[test]
    fn is_active_returns_false_for_unknown_plugin() {
        let cfg = PluginsConfig::default();
        assert!(!cfg.is_active("slack"));
        assert_eq!(cfg.entries_for("slack"), None);
    }

    #[test]
    fn is_active_returns_true_for_non_empty_object() {
        let cfg = PluginsConfig {
            entries: entries_from("email:\n  enabled: true\n"),
            ..PluginsConfig::default()
        };
        assert!(cfg.is_active("email"));
    }

    #[test]
    fn instances_for_extracts_named_entries_from_array() {
        let cfg = PluginsConfig {
            entries: entries_from(
                "telegram:\n  - instance: main\n    token: t1\n  - instance: backup\n    token: t2\n",
            ),
            ..PluginsConfig::default()
        };
        assert_eq!(
            cfg.instances_for("telegram"),
            vec!["main".to_string(), "backup".to_string()]
        );
    }

    #[test]
    fn instances_for_returns_empty_when_no_instance_field() {
        let cfg = PluginsConfig {
            entries: entries_from("telegram:\n  - token: only_token_no_instance\n"),
            ..PluginsConfig::default()
        };
        assert_eq!(cfg.instances_for("telegram"), Vec::<String>::new());
    }

    #[test]
    fn instances_for_returns_empty_for_single_instance_object() {
        let cfg = PluginsConfig {
            entries: entries_from("email:\n  enabled: true\n"),
            ..PluginsConfig::default()
        };
        assert_eq!(cfg.instances_for("email"), Vec::<String>::new());
    }

    #[test]
    fn instances_for_returns_empty_for_unknown_plugin() {
        let cfg = PluginsConfig::default();
        assert_eq!(cfg.instances_for("slack"), Vec::<String>::new());
    }

    #[test]
    fn plugin_ids_lists_declared_plugins_sorted() {
        let cfg = PluginsConfig {
            entries: entries_from("whatsapp: []\nslack:\n  workspace: T1\ntelegram: []\n"),
            ..PluginsConfig::default()
        };
        let ids: Vec<&String> = cfg.plugin_ids().collect();
        assert_eq!(ids, vec!["slack", "telegram", "whatsapp"]);
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDiscoveryConfigFile {
    pub discovery: PluginDiscoveryConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDiscoveryConfig {
    /// Directories scanned at boot for `nexo-plugin.toml` manifests
    /// AND for `nexo-plugin-*` executables when `auto_detect_binaries`
    /// is enabled. Each entry's immediate children are tested as
    /// plugin dirs. Supports `$NEXO_HOME`, `$HOME`, and `$CARGO_HOME`
    /// env-var expansion.
    ///
    /// Defaults include `$CARGO_HOME/bin` (a.k.a. `~/.cargo/bin`),
    /// `~/.local/share/nexo/plugins`, and
    /// `/usr/local/libexec/nexo/plugins` so `cargo install nexo-plugin-X`
    /// is auto-discovered without operator action. Missing dirs are
    /// tolerated (warn diagnostic, walker continues).
    #[serde(default = "default_search_paths")]
    pub search_paths: Vec<PathBuf>,
    /// Plugin ids to skip even when a valid manifest is found.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Empty = accept all valid plugins. Non-empty = whitelist; only
    /// plugins whose id is in this list are loaded.
    #[serde(default)]
    pub allowlist: Vec<String>,
    /// When false (default), the walker refuses to follow symlinks
    /// that escape the search-path canonical root. Set true only in
    /// trusted dev environments.
    #[serde(default)]
    pub follow_symlinks: bool,
    /// When true (default), the walker treats `nexo-plugin-*`
    /// executables found directly under each `search_paths` entry as
    /// plugin candidates: spawn `<bin> --print-manifest` (2s timeout)
    /// and use the dumped TOML as the manifest. Set false to restrict
    /// discovery to filesystem-resident `nexo-plugin.toml` files.
    #[serde(default = "default_auto_detect_binaries")]
    pub auto_detect_binaries: bool,
}

impl Default for PluginDiscoveryConfig {
    fn default() -> Self {
        Self {
            search_paths: default_search_paths(),
            disabled: Vec::new(),
            allowlist: Vec::new(),
            follow_symlinks: false,
            auto_detect_binaries: default_auto_detect_binaries(),
        }
    }
}

fn default_search_paths() -> Vec<PathBuf> {
    vec![
        // `cargo install nexo-plugin-X` lands here by default.
        PathBuf::from("$HOME/.cargo/bin"),
        // XDG-like per-user plugin tree.
        PathBuf::from("$HOME/.local/share/nexo/plugins"),
        // System-wide install target (Debian/Fedora-style libexec).
        PathBuf::from("/usr/local/libexec/nexo/plugins"),
    ]
}

fn default_auto_detect_binaries() -> bool {
    true
}

// ── Browser plugin config ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserConfigFile {
    pub browser: BrowserConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserConfig {
    #[serde(default)]
    pub headless: bool,
    #[serde(default)]
    pub executable: String,
    /// Empty = launch new Chrome. Set to e.g. "http://127.0.0.1:9222" to attach.
    #[serde(default)]
    pub cdp_url: String,
    #[serde(default = "default_user_data_dir")]
    pub user_data_dir: String,
    #[serde(default = "default_window_width")]
    pub window_width: u32,
    #[serde(default = "default_window_height")]
    pub window_height: u32,
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_command_timeout_ms")]
    pub command_timeout_ms: u64,
    /// Extra CLI flags forwarded verbatim to the spawned Chrome/Chromium
    /// process. Empty by default — nothing changes for Linux/macOS
    /// deployments. Intended use is restricted environments that need
    /// e.g. `--no-sandbox --disable-dev-shm-usage` (Termux, certain
    /// hardened containers). Ignored when `cdp_url` is set, since
    /// attaching to an existing Chrome means the operator already
    /// launched it with their own flags.
    #[serde(default)]
    pub args: Vec<String>,
}

fn default_user_data_dir() -> String {
    "./data/browser/profile".to_string()
}
fn default_window_width() -> u32 {
    1280
}
fn default_window_height() -> u32 {
    800
}
fn default_connect_timeout_ms() -> u64 {
    10_000
}
fn default_command_timeout_ms() -> u64 {
    15_000
}
