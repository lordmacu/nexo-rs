//! Phase 80.9 — MCP channel routing config.
//!
//! Channels let an MCP server become an inbound surface — a Slack
//! bot, a Telegram chat, an iMessage relay, etc. The server
//! advertises a capability (`experimental['nexo/channel']`) and
//! emits `notifications/nexo/channel` with `{content, meta}` when a
//! human sends a message on the underlying platform. The runtime
//! wraps the content in `<channel source="..."> ... </channel>`
//! and feeds it into the conversation as a user message.
//!
//! A channel server is a *trusted* source, so registration is
//! gated by a 5-step filter rooted in this config:
//!
//! 1. **Capability** — server actually declared the capability.
//! 2. **Killswitch** — operator may flip `channels.enabled` off
//!    via Phase 18 hot-reload to mute the entire feature.
//! 3. **Per-binding session allowlist** — each binding lists the
//!    server names it will accept channel notifications from.
//! 4. **Plugin source verification** — when the server is loaded
//!    via a plugin, the runtime plugin source must match the
//!    operator's declared `plugin_source` for the entry.
//! 5. **Approved allowlist** — `approved` lists the server names
//!    (and optional plugin sources) the operator vetted. A server
//!    that survives every previous gate but is not on this list
//!    is denied.
//!
//! Out-of-scope for the MVP (deferred): permission relay
//! (`notifications/nexo/channel/permission`) — handled by 80.9.b.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ChannelsConfig {
    /// Master killswitch. `false` (default) makes every channel
    /// server fall through with `Skip { kind: Disabled }`. Hot
    /// reloadable through Phase 18 — flipping mid-process disables
    /// the next gate evaluation; existing registered handlers stay
    /// up until the connection cycles.
    #[serde(default)]
    pub enabled: bool,

    /// Operator-approved server names. A server has to appear here
    /// to pass gate 5, even when it survives the per-binding
    /// allowlist (gate 3). Lets the operator separate "this binding
    /// can route through Slack" (gate 3) from "we've vetted the
    /// Slack server itself" (gate 5).
    #[serde(default)]
    pub approved: Vec<ApprovedChannel>,

    /// Maximum length of the `content` field on a single inbound
    /// notification. Larger payloads are truncated with `…` before
    /// being wrapped in `<channel>`. `0` disables the cap (not
    /// recommended — a single noisy server can flood the
    /// conversation context).
    #[serde(default = "default_max_content_chars")]
    pub max_content_chars: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ApprovedChannel {
    /// Server name as advertised at MCP `tools/list`. The runtime
    /// matches against this exactly. For plugin-served channels
    /// the runtime name is conventionally `plugin:<plugin_name>:<server>`
    /// — set [`plugin_source`] alongside `server` to verify the
    /// installed plugin source matches what the operator approved.
    pub server: String,

    /// Optional declared plugin source. When set, gate 4 verifies
    /// the runtime's plugin source for `server` matches this value
    /// before allowing the registration to proceed. Use the form
    /// `<plugin>@<marketplace>` to be explicit; matching is exact.
    #[serde(default)]
    pub plugin_source: Option<String>,

    /// Phase 80.9 outbound — name of the MCP tool the
    /// `channel_send` LLM wrapper should call when the agent
    /// emits a reply to this server. Defaults to
    /// [`DEFAULT_OUTBOUND_TOOL_NAME`] (`send_message`) which
    /// matches the convention every channel server in our
    /// reference set follows. Operators can override per-server
    /// for stacks with a different naming scheme (e.g.
    /// `slack_send`, `chat.postMessage`).
    #[serde(default = "default_outbound_tool_name_opt")]
    pub outbound_tool_name: Option<String>,
}

/// Default outbound tool name. Hard-coded so the
/// `channel_send` wrapper has a sensible fallback even if the
/// operator omits the field entirely.
pub const DEFAULT_OUTBOUND_TOOL_NAME: &str = "send_message";

fn default_outbound_tool_name_opt() -> Option<String> {
    Some(DEFAULT_OUTBOUND_TOOL_NAME.to_string())
}

impl ApprovedChannel {
    /// Resolve the outbound tool name, honouring the operator
    /// override and falling back to [`DEFAULT_OUTBOUND_TOOL_NAME`].
    pub fn resolved_outbound_tool_name(&self) -> &str {
        self.outbound_tool_name
            .as_deref()
            .unwrap_or(DEFAULT_OUTBOUND_TOOL_NAME)
    }
}

fn default_max_content_chars() -> u32 {
    16_000
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            approved: Vec::new(),
            max_content_chars: default_max_content_chars(),
        }
    }
}

impl ChannelsConfig {
    /// Validate at boot. Mirrors the rest of the Phase 80 cluster —
    /// fail loud at boot rather than mysteriously skip channels at
    /// runtime when the operator typoed a knob.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_content_chars > 0 && self.max_content_chars < 64 {
            return Err(format!(
                "channels.max_content_chars ({}) is below minimum 64",
                self.max_content_chars
            ));
        }
        for entry in &self.approved {
            if entry.server.is_empty() {
                return Err("channels.approved[].server must be non-empty".into());
            }
            if let Some(ps) = entry.plugin_source.as_deref() {
                if ps.is_empty() {
                    return Err(format!(
                        "channels.approved[server={}].plugin_source must be non-empty when set",
                        entry.server
                    ));
                }
            }
            if let Some(ot) = entry.outbound_tool_name.as_deref() {
                if ot.is_empty() {
                    return Err(format!(
                        "channels.approved[server={}].outbound_tool_name must be non-empty when set",
                        entry.server
                    ));
                }
            }
        }
        Ok(())
    }

    /// Convenience used by gate-5: returns the matching approved
    /// entry for `server`, if any.
    pub fn lookup_approved(&self, server: &str) -> Option<&ApprovedChannel> {
        self.approved.iter().find(|e| e.server == server)
    }

    /// Apply `max_content_chars` to the inbound content. `0` is the
    /// no-cap sentinel.
    pub fn truncate_content(&self, content: &str) -> String {
        if self.max_content_chars == 0 {
            return content.to_string();
        }
        let cap = self.max_content_chars as usize;
        if content.chars().count() <= cap {
            return content.to_string();
        }
        let mut out: String = content.chars().take(cap).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_with_sensible_values() {
        let c = ChannelsConfig::default();
        assert!(!c.enabled);
        assert!(c.approved.is_empty());
        assert_eq!(c.max_content_chars, 16_000);
        c.validate().unwrap();
    }

    #[test]
    fn validate_rejects_tiny_max_content_chars() {
        let c = ChannelsConfig {
            max_content_chars: 32,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_accepts_max_content_chars_zero_as_no_cap() {
        let c = ChannelsConfig {
            max_content_chars: 0,
            ..Default::default()
        };
        c.validate().unwrap();
    }

    #[test]
    fn validate_rejects_empty_server_name() {
        let c = ChannelsConfig {
            approved: vec![ApprovedChannel {
                server: String::new(),
                plugin_source: None,
                outbound_tool_name: None,
            }],
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_plugin_source_when_set() {
        let c = ChannelsConfig {
            approved: vec![ApprovedChannel {
                server: "slack".into(),
                plugin_source: Some(String::new()),
                outbound_tool_name: None,
            }],
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_outbound_tool_name_when_set() {
        let c = ChannelsConfig {
            approved: vec![ApprovedChannel {
                server: "slack".into(),
                plugin_source: None,
                outbound_tool_name: Some(String::new()),
            }],
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn resolved_outbound_tool_name_falls_back_to_default() {
        let entry = ApprovedChannel {
            server: "slack".into(),
            plugin_source: None,
            outbound_tool_name: None,
        };
        assert_eq!(
            entry.resolved_outbound_tool_name(),
            DEFAULT_OUTBOUND_TOOL_NAME
        );
    }

    #[test]
    fn resolved_outbound_tool_name_honours_override() {
        let entry = ApprovedChannel {
            server: "slack".into(),
            plugin_source: None,
            outbound_tool_name: Some("chat.postMessage".into()),
        };
        assert_eq!(entry.resolved_outbound_tool_name(), "chat.postMessage");
    }

    #[test]
    fn lookup_approved_finds_entry() {
        let c = ChannelsConfig {
            approved: vec![ApprovedChannel {
                server: "slack".into(),
                plugin_source: Some("slack@anthropic".into()),
                outbound_tool_name: None,            }],
            ..Default::default()
        };
        let got = c.lookup_approved("slack").unwrap();
        assert_eq!(got.plugin_source.as_deref(), Some("slack@anthropic"));
        assert!(c.lookup_approved("telegram").is_none());
    }

    #[test]
    fn truncate_content_respects_cap() {
        let c = ChannelsConfig {
            max_content_chars: 100,
            ..Default::default()
        };
        let short = "hello";
        assert_eq!(c.truncate_content(short), short);
        let long: String = "x".repeat(200);
        let truncated = c.truncate_content(&long);
        assert_eq!(truncated.chars().count(), 101); // 100 + ellipsis
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_content_zero_is_no_cap() {
        let c = ChannelsConfig {
            max_content_chars: 0,
            ..Default::default()
        };
        let long: String = "x".repeat(10_000);
        assert_eq!(c.truncate_content(&long).len(), 10_000);
    }

    #[test]
    fn yaml_round_trip_full() {
        let yaml = "\
enabled: true
max_content_chars: 8000
approved:
  - server: slack
    plugin_source: slack@anthropic
  - server: telegram
";
        let parsed: ChannelsConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.max_content_chars, 8_000);
        assert_eq!(parsed.approved.len(), 2);
        assert_eq!(parsed.approved[0].plugin_source.as_deref(), Some("slack@anthropic"));
        assert!(parsed.approved[1].plugin_source.is_none());
        parsed.validate().unwrap();
    }

    #[test]
    fn yaml_partial_uses_defaults() {
        let yaml = "enabled: true\n";
        let parsed: ChannelsConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(parsed.enabled);
        assert!(parsed.approved.is_empty());
        assert_eq!(parsed.max_content_chars, 16_000);
    }
}
