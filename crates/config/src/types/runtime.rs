//! Runtime-level knobs for the agent process — currently just
//! hot-reload settings. Loaded from `config/runtime.yaml` when present;
//! an absent file yields [`RuntimeReloadConfig::default`] with reload
//! enabled and a 500 ms debounce window.

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub reload: RuntimeReloadConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeReloadConfig {
    /// Enable the file watcher + broker `control.reload` listener.
    /// `false` turns off automatic reload; operators can still force a
    /// reload via the `agent reload` CLI (which publishes on the
    /// broker topic).
    #[serde(default = "default_reload_enabled")]
    pub enabled: bool,
    /// Debounce window fed to `notify-debouncer-full`. Atomic-save
    /// editors (vim, VSCode) generate several filesystem events per
    /// logical write; the debouncer coalesces them so we only fire
    /// one reload.
    #[serde(default = "default_reload_debounce_ms")]
    pub debounce_ms: u64,
    /// Extra paths (relative to the config directory) to watch in
    /// addition to the built-in set (`agents.yaml`, `agents.d/`,
    /// `llm.yaml`). Empty = defaults only.
    #[serde(default)]
    pub extra_watch_paths: Vec<String>,
}

impl Default for RuntimeReloadConfig {
    fn default() -> Self {
        Self {
            enabled: default_reload_enabled(),
            debounce_ms: default_reload_debounce_ms(),
            extra_watch_paths: Vec::new(),
        }
    }
}

fn default_reload_enabled() -> bool {
    true
}

fn default_reload_debounce_ms() -> u64 {
    500
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> RuntimeConfig {
        serde_yaml::from_str(yaml).expect("valid runtime.yaml")
    }

    #[test]
    fn absent_file_uses_defaults() {
        let cfg = RuntimeConfig::default();
        assert!(cfg.reload.enabled);
        assert_eq!(cfg.reload.debounce_ms, 500);
        assert!(cfg.reload.extra_watch_paths.is_empty());
    }

    #[test]
    fn empty_body_uses_defaults() {
        let cfg = parse("reload: {}\n");
        assert!(cfg.reload.enabled);
        assert_eq!(cfg.reload.debounce_ms, 500);
    }

    #[test]
    fn custom_values_round_trip() {
        let cfg = parse(
            r#"
reload:
  enabled: false
  debounce_ms: 1000
  extra_watch_paths:
    - custom.yaml
"#,
        );
        assert!(!cfg.reload.enabled);
        assert_eq!(cfg.reload.debounce_ms, 1000);
        assert_eq!(
            cfg.reload.extra_watch_paths,
            vec!["custom.yaml".to_string()]
        );
    }

    #[test]
    fn unknown_field_rejected() {
        let err = serde_yaml::from_str::<RuntimeConfig>("reload:\n  bogus: 1\n")
            .expect_err("deny_unknown_fields rejects typos");
        assert!(err.to_string().contains("bogus"));
    }
}
