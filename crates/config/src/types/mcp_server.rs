//! Phase 12.6 — `mcp_server.yaml` schema.
//!
//! Opt-in feature: expose this agent as an MCP server so Claude Desktop /
//! Cursor / Zed can invoke its tools over stdio.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfigFile {
    pub mcp_server: McpServerConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Advertised as `serverInfo.name` during MCP `initialize`. Defaults to
    /// `"agent"` when absent.
    #[serde(default)]
    pub name: Option<String>,
    /// Explicit tool allowlist. Empty = expose all non-proxy tools; proxy
    /// tools (`ext_*`, `mcp_*`) still require `expose_proxies: true`.
    #[serde(default)]
    pub allowlist: Vec<String>,
    /// When false (default), tool proxies generated from other runtimes
    /// (`ext_*`, `mcp_*`) are hidden unless explicitly allowlisted.
    #[serde(default)]
    pub expose_proxies: bool,
    /// Optional env var name containing the expected initialize token.
    /// When set, clients must include that token in initialize params
    /// (`auth_token` or `_meta.auth_token`).
    #[serde(default)]
    pub auth_token_env: Option<String>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            name: None,
            allowlist: Vec::new(),
            expose_proxies: false,
            auth_token_env: None,
        }
    }
}

fn default_enabled() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal() {
        let yaml = "mcp_server:\n  enabled: true\n";
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.mcp_server.enabled);
        assert!(f.mcp_server.allowlist.is_empty());
        assert!(!f.mcp_server.expose_proxies);
        assert!(f.mcp_server.auth_token_env.is_none());
    }

    #[test]
    fn parses_allowlist() {
        let yaml = r#"
mcp_server:
  enabled: true
  name: "kate"
  allowlist:
    - who_am_i
    - memory_recall
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(f.mcp_server.name.as_deref(), Some("kate"));
        assert_eq!(f.mcp_server.allowlist.len(), 2);
        assert!(!f.mcp_server.expose_proxies);
        assert!(f.mcp_server.auth_token_env.is_none());
    }

    #[test]
    fn parses_expose_proxies_flag() {
        let yaml = r#"
mcp_server:
  enabled: true
  expose_proxies: true
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.mcp_server.expose_proxies);
    }

    #[test]
    fn parses_auth_token_env() {
        let yaml = r#"
mcp_server:
  enabled: true
  auth_token_env: "MCP_SERVER_TOKEN"
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            f.mcp_server.auth_token_env.as_deref(),
            Some("MCP_SERVER_TOKEN")
        );
    }
}
