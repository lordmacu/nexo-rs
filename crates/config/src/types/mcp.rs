//! Phase 12.4 — `mcp.yaml` schema.
//!
//! Loads a declarative list of MCP servers plus session/reap timing knobs.
//! `agent-mcp` converts this into `McpRuntimeConfig` at startup; subsequent
//! `update_config` calls are possible without re-parsing YAML.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// YAML wrapper: `mcp:` as the top-level key in `config/mcp.yaml`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfigFile {
    pub mcp: McpConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_session_ttl", with = "humantime_serde")]
    pub session_ttl: Duration,
    #[serde(default = "default_idle_reap_interval", with = "humantime_serde")]
    pub idle_reap_interval: Duration,
    #[serde(default = "default_connect_timeout", with = "humantime_serde")]
    pub connect_timeout: Duration,
    #[serde(default = "default_initialize_timeout", with = "humantime_serde")]
    pub initialize_timeout: Duration,
    #[serde(default = "default_call_timeout", with = "humantime_serde")]
    pub call_timeout: Duration,
    #[serde(default = "default_shutdown_grace", with = "humantime_serde")]
    pub shutdown_grace: Duration,
    /// Ordered by key for stable runtime fingerprints.
    ///
    /// Naming note: keys containing `.` are reserved for explicit
    /// shadowing of extension-declared servers (`{ext_id}.{name}`) in
    /// `from_yaml_with_extensions`. Prefer plain names (without dots)
    /// for regular YAML-native servers.
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerYaml>,
    /// Phase 12.8 — opt-in file watcher for `mcp.yaml`. Re-parses the
    /// file on disk change and calls `McpRuntimeManager::update_config`.
    #[serde(default)]
    pub watch: McpWatchConfig,
    /// Phase 11.5 follow-up symmetric with extensions — opt-in
    /// propagation of `{ agent_id, session_id }` into `tools/call`
    /// requests via `params._meta`.
    #[serde(default)]
    pub context: McpContextConfig,
    /// Phase 12.8 follow-up — when `true`, removing `log_level` from a
    /// server on hot-reload fires `set_log_level` (with
    /// `default_reset_level`) instead of leaving the previous level in
    /// place. Default false for zero regression vs pre-flag behavior.
    #[serde(default)]
    pub reset_level_on_unset: bool,
    /// Phase 12.8 follow-up — level emitted when `reset_level_on_unset`
    /// fires. MCP spec does not define a "default", so operator picks.
    /// Default "info".
    #[serde(default = "default_reset_level")]
    pub default_reset_level: String,
    /// Phase 12.5 follow-up — opt-in LRU+TTL cache for `resources/read`
    /// results. Off by default for zero regression; keyed by
    /// `(server_name, uri)`, invalidated on `notifications/resources/list_changed`.
    #[serde(default)]
    pub resource_cache: McpResourceCacheConfig,
    /// Phase 12.5 follow-up — if non-empty, the `read_resource` tool
    /// logs a warning (and increments a Prometheus counter) when the
    /// requested URI uses a scheme outside this allowlist. Defense in
    /// depth against LLM-crafted URIs that bypass the server's own
    /// validation. Empty list = permissive (default).
    #[serde(default)]
    pub resource_uri_allowlist: Vec<String>,
    /// Phase 12.7 follow-up — when true, an extension manifest that
    /// declares an MCP server whose `command` / `args` / `cwd` escape
    /// the extension's own root dir is rejected at load time instead of
    /// just emitting a warning. Default `false` for back-compat.
    #[serde(default)]
    pub strict_root_paths: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpResourceCacheConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_resource_cache_ttl", with = "humantime_serde")]
    pub ttl: Duration,
    #[serde(default = "default_resource_cache_max_entries")]
    pub max_entries: usize,
}

impl Default for McpResourceCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl: default_resource_cache_ttl(),
            max_entries: default_resource_cache_max_entries(),
        }
    }
}

fn default_resource_cache_ttl() -> Duration {
    Duration::from_secs(30)
}

fn default_resource_cache_max_entries() -> usize {
    256
}

fn default_reset_level() -> String {
    "info".to_string()
}

/// Phase 12.8 — MCP-wide context propagation flag. Off by default.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpContextConfig {
    #[serde(default)]
    pub passthrough: bool,
}

/// Phase 12.8 — watcher knobs. Off by default.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpWatchConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_watch_debounce_ms")]
    pub debounce_ms: u64,
}

impl Default for McpWatchConfig {
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

impl McpConfig {
    /// Phase 12.2 follow-up — verify every HTTP header value is a legal
    /// `field-value` per RFC 7230 (ASCII visible 0x20–0x7E plus HTAB).
    /// `reqwest::HeaderValue::from_str` would otherwise silently drop
    /// offending entries at runtime; parsing is the right place to fail.
    pub fn validate(&self) -> Result<(), String> {
        for (server_name, server) in &self.servers {
            let headers: &BTreeMap<String, String> = match server {
                McpServerYaml::StreamableHttp { headers, .. }
                | McpServerYaml::Sse { headers, .. }
                | McpServerYaml::Auto { headers, .. } => headers,
                McpServerYaml::Stdio { .. } => continue,
            };
            for (key, value) in headers {
                if let Some(bad) = value
                    .chars()
                    .find(|c| !(*c == '\t' || (*c >= '\x20' && *c <= '\x7e')))
                {
                    return Err(format!(
                        "mcp.servers.{server_name}: header `{key}` carries non-ASCII-visible \
                         character `{bad:?}` (byte 0x{:02x}). Only RFC 7230 field-value chars \
                         (HTAB, 0x20–0x7E) are accepted.",
                        bad as u32
                    ));
                }
            }
        }
        Ok(())
    }
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            session_ttl: default_session_ttl(),
            idle_reap_interval: default_idle_reap_interval(),
            connect_timeout: default_connect_timeout(),
            initialize_timeout: default_initialize_timeout(),
            call_timeout: default_call_timeout(),
            shutdown_grace: default_shutdown_grace(),
            servers: BTreeMap::new(),
            watch: McpWatchConfig::default(),
            context: McpContextConfig::default(),
            reset_level_on_unset: false,
            default_reset_level: default_reset_level(),
            resource_cache: McpResourceCacheConfig::default(),
            resource_uri_allowlist: Vec::new(),
            strict_root_paths: false,
        }
    }
}

/// Transport-tagged server declaration. `http` will land in 12.2.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpServerYaml {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        cwd: Option<String>,
        /// Phase 12.8 — if set and server advertises `logging`
        /// capability, the client sends `logging/setLevel` post-init.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_level: Option<String>,
        /// Phase 12.8 — per-server override for `mcp.context.passthrough`.
        /// `None` falls back to the global flag; `Some(bool)` forces.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_passthrough: Option<bool>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_level: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_passthrough: Option<bool>,
    },
    Sse {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_level: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_passthrough: Option<bool>,
    },
    /// Phase 12.2 follow-up — try `streamable_http` first; on 404/405/415
    /// during the initialize POST, retry once over `sse`. Saves operators
    /// from knowing which variant a given server actually speaks.
    Auto {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_level: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_passthrough: Option<bool>,
    },
}

fn default_enabled() -> bool {
    true
}

fn default_session_ttl() -> Duration {
    Duration::from_secs(30 * 60)
}

fn default_idle_reap_interval() -> Duration {
    Duration::from_secs(60)
}

fn default_connect_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_initialize_timeout() -> Duration {
    Duration::from_secs(10)
}

fn default_call_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_shutdown_grace() -> Duration {
    Duration::from_secs(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let yaml = r#"
mcp:
  enabled: true
  servers:
    filesystem:
      transport: stdio
      command: "/bin/echo"
"#;
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(f.mcp.servers.len(), 1);
        assert_eq!(f.mcp.session_ttl, Duration::from_secs(30 * 60));
        match f.mcp.servers.get("filesystem").unwrap() {
            McpServerYaml::Stdio { command, .. } => assert_eq!(command, "/bin/echo"),
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn humantime_durations_parse() {
        let yaml = r#"
mcp:
  session_ttl: "5m"
  idle_reap_interval: "2s"
  call_timeout: "100ms"
  servers: {}
"#;
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(f.mcp.session_ttl, Duration::from_secs(300));
        assert_eq!(f.mcp.idle_reap_interval, Duration::from_secs(2));
        assert_eq!(f.mcp.call_timeout, Duration::from_millis(100));
    }

    #[test]
    fn unknown_transport_rejected() {
        let yaml = r#"
mcp:
  servers:
    x:
      transport: "unknown"
      command: "true"
"#;
        let result: Result<McpConfigFile, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn parses_streamable_http_variant() {
        let yaml = r#"
mcp:
  servers:
    brave:
      transport: streamable_http
      url: "https://api.example.com/mcp"
      headers:
        Authorization: "Bearer x"
"#;
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        match f.mcp.servers.get("brave").unwrap() {
            McpServerYaml::StreamableHttp { url, headers, .. } => {
                assert_eq!(url, "https://api.example.com/mcp");
                assert_eq!(headers.get("Authorization").unwrap(), "Bearer x");
            }
            _ => panic!("expected StreamableHttp"),
        }
    }

    #[test]
    fn parses_sse_variant() {
        let yaml = r#"
mcp:
  servers:
    legacy:
      transport: sse
      url: "https://old.example.com/sse"
"#;
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        match f.mcp.servers.get("legacy").unwrap() {
            McpServerYaml::Sse { url, .. } => {
                assert_eq!(url, "https://old.example.com/sse");
            }
            _ => panic!("expected Sse"),
        }
    }

    #[test]
    fn default_populates_when_section_minimal() {
        let yaml = "mcp:\n  enabled: true\n";
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.mcp.servers.is_empty());
        assert_eq!(f.mcp.connect_timeout, Duration::from_secs(30));
    }

    #[test]
    fn watch_defaults_to_disabled() {
        let yaml = "mcp:\n  enabled: true\n";
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(!f.mcp.watch.enabled);
        assert_eq!(f.mcp.watch.debounce_ms, 500);
    }

    #[test]
    fn mcp_context_defaults_to_false() {
        let yaml = "mcp:\n  enabled: true\n";
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(!f.mcp.context.passthrough);
    }

    #[test]
    fn reset_level_on_unset_defaults_false() {
        let yaml = "mcp:\n  enabled: true\n";
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(!f.mcp.reset_level_on_unset);
    }

    #[test]
    fn default_reset_level_defaults_to_info() {
        let yaml = "mcp:\n  enabled: true\n";
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(f.mcp.default_reset_level, "info");
    }

    #[test]
    fn default_reset_level_parses_custom() {
        let yaml = "mcp:\n  enabled: true\n  default_reset_level: \"warning\"\n";
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(f.mcp.default_reset_level, "warning");
    }

    #[test]
    fn reset_level_on_unset_parses_true() {
        let yaml = "mcp:\n  enabled: true\n  reset_level_on_unset: true\n";
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.mcp.reset_level_on_unset);
    }

    #[test]
    fn per_server_context_passthrough_parses() {
        let yaml = r#"
mcp:
  enabled: true
  servers:
    s:
      transport: stdio
      command: "/bin/true"
      context_passthrough: false
"#;
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        match f.mcp.servers.get("s").unwrap() {
            McpServerYaml::Stdio {
                context_passthrough,
                ..
            } => {
                assert_eq!(*context_passthrough, Some(false));
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn per_server_context_passthrough_defaults_none() {
        let yaml = r#"
mcp:
  enabled: true
  servers:
    s:
      transport: stdio
      command: "/bin/true"
"#;
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        match f.mcp.servers.get("s").unwrap() {
            McpServerYaml::Stdio {
                context_passthrough,
                ..
            } => {
                assert!(context_passthrough.is_none());
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn mcp_context_passthrough_parses_true() {
        let yaml = r#"
mcp:
  enabled: true
  context:
    passthrough: true
"#;
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.mcp.context.passthrough);
    }

    #[test]
    fn validate_rejects_non_ascii_header_value() {
        let yaml = r#"
mcp:
  servers:
    broken:
      transport: streamable_http
      url: "https://x.example/"
      headers:
        Authorization: "Bearer tôken"
"#;
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        let err = f.mcp.validate().expect_err("non-ascii should reject");
        assert!(err.contains("Authorization"));
        assert!(err.contains("mcp.servers.broken"));
    }

    #[test]
    fn validate_rejects_control_char_header_value() {
        let yaml = "mcp:\n  servers:\n    x:\n      transport: sse\n      url: \"https://x.example/\"\n      headers:\n        X-Bad: \"line\\nbreak\"\n";
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        let err = f.mcp.validate().expect_err("control chars rejected");
        assert!(err.contains("X-Bad"));
    }

    #[test]
    fn validate_accepts_legal_header_values() {
        let yaml = r#"
mcp:
  servers:
    ok:
      transport: streamable_http
      url: "https://x.example/"
      headers:
        Authorization: "Bearer abc123=="
        X-Trace-Id: "deadbeef"
"#;
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        f.mcp.validate().expect("legal headers must pass");
    }

    #[test]
    fn parses_auto_variant() {
        let yaml = r#"
mcp:
  servers:
    flex:
      transport: auto
      url: "https://api.example.com/mcp"
      headers:
        Authorization: "Bearer x"
"#;
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        match f.mcp.servers.get("flex").unwrap() {
            McpServerYaml::Auto { url, headers, .. } => {
                assert_eq!(url, "https://api.example.com/mcp");
                assert_eq!(headers.get("Authorization").unwrap(), "Bearer x");
            }
            _ => panic!("expected Auto"),
        }
    }

    #[test]
    fn watch_section_parses() {
        let yaml = r#"
mcp:
  enabled: true
  watch:
    enabled: true
    debounce_ms: 250
"#;
        let f: McpConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.mcp.watch.enabled);
        assert_eq!(f.mcp.watch.debounce_ms, 250);
    }
}
