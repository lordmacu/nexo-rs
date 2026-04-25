//! Driver runtime configuration. The full YAML at
//! `config/driver/claude.yaml` deserialises into [`DriverConfig`].
//!
//! Env-var substitution (`${VAR}` and `${VAR:-default}`) happens
//! *before* yaml parsing — same convention as `crates/config`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::error::DriverError;

#[derive(Clone, Debug, Deserialize)]
pub struct DriverConfig {
    /// Top-level Claude CLI config (binary, default args, timeouts).
    /// Flattened from the YAML root for ergonomic access.
    #[serde(flatten)]
    pub claude: nexo_driver_claude::ClaudeConfig,
    #[serde(with = "humantime_serde", default = "default_setup_timeout")]
    pub setup_timeout: Duration,
    pub binding_store: BindingStoreConfig,
    pub permission: PermissionConfig,
    pub workspace: WorkspaceConfig,
    pub driver: DriverBinConfig,
    #[serde(default)]
    pub acceptance: AcceptanceConfig,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BindingStoreConfig {
    pub kind: BindingStoreKind,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default, with = "humantime_serde::option")]
    pub idle_ttl: Option<Duration>,
    #[serde(default, with = "humantime_serde::option")]
    pub max_age: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BindingStoreKind {
    Sqlite,
    Memory,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PermissionConfig {
    pub socket: PathBuf,
    #[serde(with = "humantime_serde", default = "default_decision_timeout")]
    pub decision_timeout: Duration,
    #[serde(default = "default_session_cache_max")]
    pub session_cache_max: usize,
    pub decider: DeciderConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeciderConfig {
    Llm {
        provider: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default = "default_decider_max_tokens")]
        max_tokens: u32,
        #[serde(default)]
        system_prompt_path: Option<PathBuf>,
    },
    AllowAll,
    DenyAll {
        reason: String,
    },
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkspaceConfig {
    pub root: PathBuf,
    #[serde(default)]
    pub cleanup_on_done: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DriverBinConfig {
    pub bin_path: PathBuf,
    #[serde(default = "default_emit_nats_events")]
    pub emit_nats_events: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct AcceptanceConfig {
    /// Phase 67.5 will populate this. Empty in 67.4.
    #[serde(default, with = "humantime_serde::option")]
    pub default_shell_timeout: Option<Duration>,
}

fn default_setup_timeout() -> Duration {
    Duration::from_secs(30)
}
fn default_decision_timeout() -> Duration {
    Duration::from_secs(30)
}
fn default_session_cache_max() -> usize {
    1024
}
fn default_decider_max_tokens() -> u32 {
    256
}
fn default_emit_nats_events() -> bool {
    true
}

impl DriverConfig {
    pub fn from_yaml_str(yaml: &str) -> Result<Self, DriverError> {
        let substituted = substitute_env_vars(yaml);
        serde_yaml::from_str(&substituted).map_err(|e| DriverError::Yaml(e.to_string()))
    }

    pub fn from_yaml_file(path: &Path) -> Result<Self, DriverError> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_yaml_str(&raw)
    }
}

/// Substitute `${VAR}` and `${VAR:-default}` against process env.
/// Patterns we don't recognise are left intact.
fn substitute_env_vars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(end) = find_close_brace(bytes, i + 2) {
                let inner = &input[i + 2..end];
                let (name, fallback) = match inner.find(":-") {
                    Some(pos) => (&inner[..pos], Some(&inner[pos + 2..])),
                    None => (inner, None),
                };
                if is_var_name(name) {
                    let value = std::env::var(name).ok();
                    let resolved = value.as_deref().or(fallback).unwrap_or("");
                    out.push_str(resolved);
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(input.as_bytes()[i] as char);
        i += 1;
    }
    out
}

fn find_close_brace(bytes: &[u8], from: usize) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|&b| b == b'}')
        .map(|p| from + p)
}

fn is_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_YAML: &str = r#"
binary: claude
binding_store:
  kind: memory
permission:
  socket: /tmp/driver.sock
  decider:
    kind: allow_all
workspace:
  root: /tmp/claude-runs
driver:
  bin_path: /usr/local/bin/nexo-driver-permission-mcp
"#;

    #[test]
    fn parses_minimum_yaml_with_defaults() {
        let cfg = DriverConfig::from_yaml_str(MIN_YAML).unwrap();
        assert_eq!(cfg.binding_store.kind, BindingStoreKind::Memory);
        assert!(matches!(cfg.permission.decider, DeciderConfig::AllowAll));
        assert_eq!(cfg.setup_timeout, Duration::from_secs(30));
        assert_eq!(cfg.permission.decision_timeout, Duration::from_secs(30));
        assert!(cfg.driver.emit_nats_events);
        assert!(!cfg.workspace.cleanup_on_done);
    }

    #[test]
    fn env_substitution_basic() {
        std::env::set_var("NEXO_DRIVER_TEST_PATH", "/run/x.sock");
        let yaml = r#"
binary: claude
binding_store:
  kind: memory
permission:
  socket: ${NEXO_DRIVER_TEST_PATH}
  decider: { kind: allow_all }
workspace:
  root: /tmp/claude-runs
driver:
  bin_path: /usr/local/bin/nexo-driver-permission-mcp
"#;
        let cfg = DriverConfig::from_yaml_str(yaml).unwrap();
        assert_eq!(cfg.permission.socket, PathBuf::from("/run/x.sock"));
        std::env::remove_var("NEXO_DRIVER_TEST_PATH");
    }

    #[test]
    fn env_substitution_with_default_fallback() {
        std::env::remove_var("NEXO_DRIVER_TEST_UNSET");
        let yaml = r#"
binary: claude
binding_store:
  kind: memory
permission:
  socket: ${NEXO_DRIVER_TEST_UNSET:-/fallback.sock}
  decider: { kind: allow_all }
workspace:
  root: /tmp/claude-runs
driver:
  bin_path: /usr/local/bin/nexo-driver-permission-mcp
"#;
        let cfg = DriverConfig::from_yaml_str(yaml).unwrap();
        assert_eq!(cfg.permission.socket, PathBuf::from("/fallback.sock"));
    }

    #[test]
    fn unknown_var_pattern_left_intact() {
        // `$NOT_BRACED` is not our pattern — we only handle `${...}`.
        let yaml = "$NOT_BRACED stays\n";
        let out = substitute_env_vars(yaml);
        assert_eq!(out, "$NOT_BRACED stays\n");
    }
}
