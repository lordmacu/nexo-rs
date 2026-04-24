use std::time::Duration;

use serde::{Deserialize, Serialize};

/// YAML wrapper: `extensions:` as the top-level key in
/// `config/extensions.yaml` — mirrors how other optional config files wrap
/// their inner struct (see `plugins/*.yaml`).
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionsConfigFile {
    pub extensions: ExtensionsConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionsConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_search_paths")]
    pub search_paths: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default = "default_ignore_dirs")]
    pub ignore_dirs: Vec<String>,
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    #[serde(default)]
    pub transport_defaults: TransportDefaults,
    /// Phase 11.2 follow-up — opt-in watcher for `plugin.toml` files
    /// across discovered extension directories. Changes log at `warn`;
    /// no auto-respawn (restart required to apply).
    #[serde(default)]
    pub watch: ExtensionsWatchConfig,
    /// Timeouts + concurrency for `agent ext doctor --runtime`.
    #[serde(default)]
    pub doctor: ExtensionsDoctorConfig,
    /// When true, discovery follows filesystem symlinks inside
    /// `search_paths`. Useful for monorepos that symlink shared plugins
    /// in. Off by default to avoid loops + surprise path escapes.
    #[serde(default)]
    pub follow_links: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionsDoctorConfig {
    #[serde(default = "default_doctor_stdio_ms")]
    pub stdio_timeout_ms: u64,
    #[serde(default = "default_doctor_nats_ms")]
    pub nats_timeout_ms: u64,
    #[serde(default = "default_doctor_http_ms")]
    pub http_timeout_ms: u64,
    #[serde(default = "default_doctor_concurrency")]
    pub concurrency: u32,
}

impl Default for ExtensionsDoctorConfig {
    fn default() -> Self {
        Self {
            stdio_timeout_ms: default_doctor_stdio_ms(),
            nats_timeout_ms: default_doctor_nats_ms(),
            http_timeout_ms: default_doctor_http_ms(),
            concurrency: default_doctor_concurrency(),
        }
    }
}

fn default_doctor_stdio_ms() -> u64 {
    5000
}
fn default_doctor_nats_ms() -> u64 {
    5000
}
fn default_doctor_http_ms() -> u64 {
    3000
}
fn default_doctor_concurrency() -> u32 {
    4
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionsWatchConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_watch_debounce_ms")]
    pub debounce_ms: u64,
}

impl Default for ExtensionsWatchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            debounce_ms: default_watch_debounce_ms(),
        }
    }
}

fn default_watch_debounce_ms() -> u64 {
    500
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransportDefaults {
    #[serde(default)]
    pub nats: NatsTransportDefaults,
}

/// Defaults applied to every NATS-backed extension runtime (11.4).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NatsTransportDefaults {
    #[serde(default = "default_subject_prefix")]
    pub subject_prefix: String,
    #[serde(default = "default_call_timeout", with = "humantime_serde")]
    pub call_timeout: Duration,
    #[serde(default = "default_handshake_timeout", with = "humantime_serde")]
    pub handshake_timeout: Duration,
    #[serde(default = "default_heartbeat_interval", with = "humantime_serde")]
    pub heartbeat_interval: Duration,
    #[serde(default = "default_heartbeat_grace_factor")]
    pub heartbeat_grace_factor: u32,
    #[serde(default = "default_shutdown_grace", with = "humantime_serde")]
    pub shutdown_grace: Duration,
}

impl Default for NatsTransportDefaults {
    fn default() -> Self {
        Self {
            subject_prefix: default_subject_prefix(),
            call_timeout: default_call_timeout(),
            handshake_timeout: default_handshake_timeout(),
            heartbeat_interval: default_heartbeat_interval(),
            heartbeat_grace_factor: default_heartbeat_grace_factor(),
            shutdown_grace: default_shutdown_grace(),
        }
    }
}

impl Default for ExtensionsConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            search_paths: default_search_paths(),
            disabled: Vec::new(),
            allowlist: Vec::new(),
            ignore_dirs: default_ignore_dirs(),
            max_depth: default_max_depth(),
            transport_defaults: TransportDefaults::default(),
            watch: ExtensionsWatchConfig::default(),
            doctor: ExtensionsDoctorConfig::default(),
            follow_links: false,
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_search_paths() -> Vec<String> {
    vec!["./extensions".to_string()]
}

fn default_ignore_dirs() -> Vec<String> {
    vec![
        "target".to_string(),
        ".git".to_string(),
        "node_modules".to_string(),
        "dist".to_string(),
        ".build".to_string(),
    ]
}

fn default_max_depth() -> usize {
    3
}

fn default_subject_prefix() -> String {
    "ext".to_string()
}

fn default_call_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_handshake_timeout() -> Duration {
    Duration::from_secs(10)
}

fn default_heartbeat_interval() -> Duration {
    Duration::from_secs(15)
}

fn default_heartbeat_grace_factor() -> u32 {
    3
}

fn default_shutdown_grace() -> Duration {
    Duration::from_secs(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_populate_when_section_missing() {
        let yaml = "extensions:\n  enabled: true\n";
        let f: ExtensionsConfigFile = serde_yaml::from_str(yaml).unwrap();
        let nats = &f.extensions.transport_defaults.nats;
        assert_eq!(nats.subject_prefix, "ext");
        assert_eq!(nats.call_timeout, Duration::from_secs(30));
        assert_eq!(nats.heartbeat_grace_factor, 3);
    }

    #[test]
    fn watch_defaults_to_disabled() {
        let yaml = "extensions:\n  enabled: true\n";
        let f: ExtensionsConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(!f.extensions.watch.enabled);
        assert_eq!(f.extensions.watch.debounce_ms, 500);
    }

    #[test]
    fn watch_section_parses() {
        let yaml = r#"
extensions:
  enabled: true
  watch:
    enabled: true
    debounce_ms: 250
"#;
        let f: ExtensionsConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.extensions.watch.enabled);
        assert_eq!(f.extensions.watch.debounce_ms, 250);
    }

    #[test]
    fn humantime_durations_parse() {
        let yaml = r#"
extensions:
  enabled: true
  transport_defaults:
    nats:
      subject_prefix: "prod-ext"
      call_timeout: "5s"
      handshake_timeout: "500ms"
      heartbeat_interval: "2s"
      heartbeat_grace_factor: 5
      shutdown_grace: "1s"
"#;
        let f: ExtensionsConfigFile = serde_yaml::from_str(yaml).unwrap();
        let n = &f.extensions.transport_defaults.nats;
        assert_eq!(n.subject_prefix, "prod-ext");
        assert_eq!(n.call_timeout, Duration::from_secs(5));
        assert_eq!(n.handshake_timeout, Duration::from_millis(500));
        assert_eq!(n.heartbeat_grace_factor, 5);
    }
}
