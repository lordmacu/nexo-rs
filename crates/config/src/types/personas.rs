//! Phase F5 of `cody-cli-install` — operator-configured
//! persona discovery walk knobs. Loaded from
//! `<config_dir>/personas/discovery.yaml` (optional —
//! missing file means defaults: empty `search_paths` so
//! nothing is scanned and only personas the operator
//! manually wired into `agents.d/` survive).
//!
//! Mirrors the `plugins.discovery` schema (Phase 81.5) so
//! operators familiar with one know the other; the only
//! shape difference is the missing `follow_symlinks`
//! toggle (persona installs via `nexo persona install` are
//! fully managed by the daemon — no symlink scenarios in
//! the supported flow).

use std::path::PathBuf;

use serde::Deserialize;

/// Top-level personas config. Owned by `Config::personas`;
/// future fields land here (e.g. signing-policy overrides
/// per `nexo-ext-installer::TrustedKeysConfig`).
#[derive(Debug, Default, Clone)]
pub struct PersonasConfig {
    /// Boot-time discovery walk knobs.
    pub discovery: PersonaDiscoveryConfig,
}

/// File-on-disk shape for `personas/discovery.yaml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaDiscoveryConfigFile {
    /// Wraps the inner config so the YAML file's top-level
    /// key matches the field name (`discovery:`), mirroring
    /// `plugins.discovery`'s convention.
    pub discovery: PersonaDiscoveryConfig,
}

/// Persona discovery walk knobs.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaDiscoveryConfig {
    /// Directories scanned at boot for `<id>-<version>/persona.toml`.
    /// Each entry's immediate children are tested as persona
    /// dirs. Supports `$NEXO_HOME` and `$HOME` env-var
    /// expansion (resolved by the loader, not this struct).
    /// Default: empty (no scan).
    #[serde(default)]
    pub search_paths: Vec<PathBuf>,
    /// Persona ids to skip even when a valid manifest is
    /// found. Mirrors `plugins.discovery.disabled`.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Empty = accept every valid persona. Non-empty =
    /// whitelist; only ids in this list are loaded. Mirrors
    /// `plugins.discovery.allowlist`.
    #[serde(default)]
    pub allowlist: Vec<String>,
}

impl PersonaDiscoveryConfig {
    /// Apply the disabled/allowlist filters to a discovered
    /// id. Returns `true` when the persona should be loaded.
    /// Pure function — easy to unit-test the policy in
    /// isolation from disk I/O.
    pub fn id_passes_filters(&self, id: &str) -> bool {
        if self.disabled.iter().any(|d| d == id) {
            return false;
        }
        if !self.allowlist.is_empty() && !self.allowlist.iter().any(|a| a == id) {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_accepts_any_id() {
        let cfg = PersonaDiscoveryConfig::default();
        assert!(cfg.id_passes_filters("cody"));
        assert!(cfg.id_passes_filters("muse"));
    }

    #[test]
    fn disabled_blocks_match() {
        let cfg = PersonaDiscoveryConfig {
            disabled: vec!["cody".into()],
            ..Default::default()
        };
        assert!(!cfg.id_passes_filters("cody"));
        assert!(cfg.id_passes_filters("muse"));
    }

    #[test]
    fn allowlist_restricts_to_listed_only() {
        let cfg = PersonaDiscoveryConfig {
            allowlist: vec!["cody".into()],
            ..Default::default()
        };
        assert!(cfg.id_passes_filters("cody"));
        assert!(!cfg.id_passes_filters("muse"));
    }

    #[test]
    fn disabled_overrides_allowlist() {
        let cfg = PersonaDiscoveryConfig {
            disabled: vec!["cody".into()],
            allowlist: vec!["cody".into()],
            ..Default::default()
        };
        assert!(
            !cfg.id_passes_filters("cody"),
            "disabled must win over allowlist"
        );
    }

    #[test]
    fn parses_yaml_file_shape() {
        let yaml = r#"discovery:
  search_paths:
    - /var/lib/nexo/personas
    - /opt/nexo/personas
  disabled: [legacy]
  allowlist: []
"#;
        let parsed: PersonaDiscoveryConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.discovery.search_paths.len(), 2);
        assert_eq!(parsed.discovery.disabled, vec!["legacy"]);
        assert!(parsed.discovery.allowlist.is_empty());
    }
}
