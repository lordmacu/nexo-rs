//! Public-internet tunnel declaration.
//!
//! Plugins that expose an HTTP route the operator might want to
//! reach from outside the LAN (e.g. WhatsApp pairing on a phone
//! while the daemon runs on a desktop) declare this section. When
//! the operator flips the daemon-wide capability env
//! `NEXO_PLUGIN_PUBLIC_TUNNEL_ALLOW=1`, the daemon spawns a
//! Cloudflare quick tunnel pointed at its HTTP port for every
//! plugin with `enabled = true`. The plugin's HTTP routes (served
//! through `[plugin.http]` + `PluginHttpRouter`) become reachable
//! at `https://*.trycloudflare.com/<mount_prefix>/...`.
//!
//! Opt-in by design — the daemon never opens a public tunnel
//! without BOTH the manifest declaration AND the operator's env
//! capability grant. Sandbox-friendly: plugins that don't need
//! external reach omit the section entirely.

use serde::{Deserialize, Serialize};

/// `[plugin.public_tunnel]` section. Absent / unset = plugin
/// declines a public tunnel even if the operator turns on
/// `NEXO_PLUGIN_PUBLIC_TUNNEL_ALLOW`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginPublicTunnelSection {
    /// Plugin-side opt-in. `false` (default) keeps the daemon
    /// from ever spawning a tunnel for this plugin. Independent
    /// of the operator capability env — both must be on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enabled: bool,

    /// Optional broker subject the daemon subscribes to as the
    /// "close tunnel" signal. When the plugin publishes ANY
    /// message on this subject (e.g. a pairing-completed event),
    /// the daemon tears the tunnel down. `None` keeps the tunnel
    /// running for the daemon's lifetime.
    ///
    /// Typical shape: `plugin.lifecycle.<plugin_id>.tunnel_done`.
    /// Wildcards (`*`, `>`) are forbidden — the daemon needs an
    /// exact subscriber subject so a stray plugin event doesn't
    /// race-close a healthy tunnel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_on_event: Option<String>,
}

impl PluginPublicTunnelSection {
    /// `true` when the manifest writer omitted every field.
    /// Equivalent to "no section present"; used by
    /// `skip_serializing_if` on the parent `PluginSection`.
    pub fn is_unset(&self) -> bool {
        !self.enabled && self.close_on_event.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_section_is_unset() {
        let s = PluginPublicTunnelSection::default();
        assert!(s.is_unset());
    }

    #[test]
    fn enabled_alone_round_trips() {
        let toml_src = r#"enabled = true"#;
        let parsed: PluginPublicTunnelSection = toml::from_str(toml_src).unwrap();
        assert!(parsed.enabled);
        assert!(parsed.close_on_event.is_none());
    }

    #[test]
    fn enabled_with_close_event_round_trips() {
        let toml_src = r#"
enabled = true
close_on_event = "plugin.lifecycle.whatsapp.tunnel_done"
"#;
        let parsed: PluginPublicTunnelSection = toml::from_str(toml_src).unwrap();
        assert!(parsed.enabled);
        assert_eq!(
            parsed.close_on_event.as_deref(),
            Some("plugin.lifecycle.whatsapp.tunnel_done"),
        );
    }

    #[test]
    fn deny_unknown_fields_rejects_typo() {
        let toml_src = r#"
enabled = true
clsoe_on_event = "x"
"#;
        let err = toml::from_str::<PluginPublicTunnelSection>(toml_src).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn skip_serializing_if_unset_emits_empty_toml() {
        let s = PluginPublicTunnelSection::default();
        let out = toml::to_string(&s).unwrap();
        assert!(out.trim().is_empty(), "expected empty TOML, got: {out:?}");
    }
}
