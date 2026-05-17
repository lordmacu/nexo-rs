//! Phase 81.33.b.real Stage 2 — `[plugin.http]` manifest section.
//!
//! Plugins that need to expose HTTP routes on the daemon's
//! HTTP server (e.g. pairing pages, OAuth callback endpoints,
//! webhook receivers) declare a mount prefix here. The daemon
//! matches incoming requests under the prefix and forwards them
//! to the plugin's subprocess via broker JSON-RPC. The plugin
//! handles internal routing under the prefix on its own.
//!
//! Distinct from [`crate::manifest::PluginSection::http_server`],
//! which documents a plugin-bound port (the plugin listens
//! directly; daemon does not proxy).
//!
//! ## Wire format
//!
//! Daemon → plugin via `plugin.<id>.http.request`:
//! ```json
//! {
//!   "method": "GET",
//!   "path": "/whatsapp/pair",
//!   "query": "instance=default",
//!   "headers": [["Host", "127.0.0.1:8080"], ...],
//!   "body_base64": ""
//! }
//! ```
//!
//! Plugin → daemon reply:
//! ```json
//! {
//!   "status": 200,
//!   "headers": [["Content-Type", "text/html"]],
//!   "body_base64": "<base64-encoded body bytes>"
//! }
//! ```
//!
//! ## Limitations
//!
//! - Streaming responses (SSE, chunked transfer) NOT supported in
//!   Stage 2. Plugin must buffer the full response and reply once.
//! - WebSocket upgrades NOT supported. Plugin must bind its own
//!   port (`[plugin.http_server]`) for WS endpoints today.
//! - Request body is base64-encoded JSON; OK for HTML pages
//!   (≤100KB typical) but wasteful for large file uploads. Plugins
//!   needing large bodies should expose `[plugin.http_server]`
//!   directly.

use serde::{Deserialize, Serialize};

/// Daemon-side HTTP mount declaration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginHttpSection {
    /// Path prefix the daemon mounts on its HTTP server
    /// (`:8080`). Must start with `/`. Daemon matches incoming
    /// `request.path.starts_with(mount_prefix)` and forwards.
    ///
    /// Plugins owning multiple prefixes ship one
    /// `[[plugin.http.route]]` entry per prefix — for Stage 2
    /// only the single-prefix form is supported (most pairing
    /// flows + webhooks fit one prefix).
    pub mount_prefix: String,

    /// Per-request broker RPC timeout. `None` = daemon default
    /// (30 seconds). Plugins serving slow flows (image
    /// generation, OAuth dances) can extend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

impl PluginHttpSection {
    /// Validation: `mount_prefix` must start with `/` and contain
    /// no `?`, `#`, or query/fragment separators. Empty prefix
    /// rejected (would match every path).
    pub fn validate(&self) -> Result<(), String> {
        let p = &self.mount_prefix;
        if p.is_empty() {
            return Err("mount_prefix cannot be empty".into());
        }
        if !p.starts_with('/') {
            return Err(format!("mount_prefix must start with `/`; got `{p}`"));
        }
        if p.contains('?') || p.contains('#') {
            return Err(format!("mount_prefix cannot contain `?` or `#`; got `{p}`"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_simple_prefix() {
        let s = PluginHttpSection {
            mount_prefix: "/whatsapp".into(),
            timeout_seconds: None,
        };
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty() {
        let s = PluginHttpSection::default();
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_unanchored() {
        let s = PluginHttpSection {
            mount_prefix: "whatsapp".into(),
            timeout_seconds: None,
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_query_separator() {
        let s = PluginHttpSection {
            mount_prefix: "/whatsapp?instance=default".into(),
            timeout_seconds: None,
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn deserializes_minimal_toml() {
        let toml = r#"
            mount_prefix = "/whatsapp"
        "#;
        let s: PluginHttpSection = toml::from_str(toml).expect("parse");
        assert_eq!(s.mount_prefix, "/whatsapp");
        assert!(s.timeout_seconds.is_none());
    }

    #[test]
    fn deserializes_with_timeout() {
        let toml = r#"
            mount_prefix = "/oauth"
            timeout_seconds = 60
        "#;
        let s: PluginHttpSection = toml::from_str(toml).expect("parse");
        assert_eq!(s.timeout_seconds, Some(60));
    }
}
