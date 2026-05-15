//! Phase 81.33.b.real Stage 4 — `[plugin.admin]` manifest section.
//!
//! Plugins that want to expose admin RPC methods (e.g. WhatsApp
//! `nexo/admin/whatsapp/bot/list`, Telegram bot-info commands,
//! email account inspectors) declare a method prefix in their
//! `nexo-plugin.toml`. The daemon's admin RPC dispatcher matches
//! every incoming method against the registered prefixes and
//! forwards matches to the plugin's subprocess via broker
//! JSON-RPC. The plugin handles internal dispatch.
//!
//! Replaces the previous pattern where each plugin needed a
//! hardcoded `.with_<plugin>_handle(Arc<dyn XxxHandle>)`
//! builder method on the admin dispatcher.
//!
//! ## Wire format
//!
//! Daemon → plugin on `<broker_topic_prefix>.<method_suffix>`:
//!
//! ```json
//! { "method": "<full-method-name>", "params": <params-json> }
//! ```
//!
//! Plugin replies:
//!
//! ```json
//! { "ok": true,  "result": <typed-result-json> }
//! { "ok": false, "error": "<message>" }
//! ```
//!
//! ## Method routing
//!
//! Daemon receives `method = "nexo/admin/whatsapp/bot/list"`.
//! With manifest `method_prefix = "nexo/admin/whatsapp/"` and
//! `broker_topic_prefix = "plugin.whatsapp.admin"`:
//!
//! 1. Strip prefix → `bot/list`.
//! 2. Replace `/` with `.` → `bot.list`.
//! 3. Append to broker prefix → `plugin.whatsapp.admin.bot.list`.
//! 4. Forward request + parse reply.

use serde::{Deserialize, Serialize};

/// Daemon-side admin RPC mount declaration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginAdminSection {
    /// Admin RPC method prefix the plugin owns. Must start with
    /// `nexo/admin/` (the canonical admin RPC namespace) and end
    /// with `/` so prefix matching is unambiguous. Example:
    /// `nexo/admin/whatsapp/`.
    pub method_prefix: String,

    /// Broker subject prefix the daemon forwards under. Example:
    /// `plugin.whatsapp.admin`. The daemon appends the
    /// method-suffix (with `/` → `.`) so a plugin handler
    /// subscribes to `<broker_topic_prefix>.<verb>`.
    pub broker_topic_prefix: String,

    /// Optional per-method broker RPC timeout. Default 30s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

impl PluginAdminSection {
    pub fn validate(&self) -> Result<(), String> {
        if !self.method_prefix.starts_with("nexo/admin/") {
            return Err(format!(
                "method_prefix must start with `nexo/admin/`; got `{}`",
                self.method_prefix
            ));
        }
        if !self.method_prefix.ends_with('/') {
            return Err(format!(
                "method_prefix must end with `/`; got `{}`",
                self.method_prefix
            ));
        }
        if self.method_prefix == "nexo/admin/" {
            return Err("method_prefix `nexo/admin/` is the root namespace; declare a sub-prefix".into());
        }
        if self.broker_topic_prefix.is_empty() {
            return Err("broker_topic_prefix cannot be empty".into());
        }
        if self.broker_topic_prefix.contains(' ') {
            return Err("broker_topic_prefix cannot contain spaces".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_typical_prefix() {
        let s = PluginAdminSection {
            method_prefix: "nexo/admin/whatsapp/".into(),
            broker_topic_prefix: "plugin.whatsapp.admin".into(),
            timeout_seconds: None,
        };
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_rejects_missing_admin_anchor() {
        let s = PluginAdminSection {
            method_prefix: "/whatsapp/".into(),
            broker_topic_prefix: "plugin.whatsapp".into(),
            timeout_seconds: None,
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_missing_trailing_slash() {
        let s = PluginAdminSection {
            method_prefix: "nexo/admin/whatsapp".into(),
            broker_topic_prefix: "plugin.whatsapp".into(),
            timeout_seconds: None,
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_root_namespace() {
        let s = PluginAdminSection {
            method_prefix: "nexo/admin/".into(),
            broker_topic_prefix: "plugin.foo".into(),
            timeout_seconds: None,
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_broker_topic() {
        let s = PluginAdminSection {
            method_prefix: "nexo/admin/whatsapp/".into(),
            broker_topic_prefix: String::new(),
            timeout_seconds: None,
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn deserializes_minimal_toml() {
        let toml = r#"
            method_prefix       = "nexo/admin/whatsapp/"
            broker_topic_prefix = "plugin.whatsapp.admin"
        "#;
        let s: PluginAdminSection = toml::from_str(toml).expect("parse");
        assert_eq!(s.method_prefix, "nexo/admin/whatsapp/");
        assert_eq!(s.broker_topic_prefix, "plugin.whatsapp.admin");
    }
}
