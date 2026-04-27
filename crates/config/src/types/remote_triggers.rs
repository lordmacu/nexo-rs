//! Phase 79.8 — per-agent allowlist of remote-trigger destinations.
//!
//! The `RemoteTrigger` tool refuses to publish to anything not
//! listed here. Operators YAML-name each destination; the model
//! refers to it by name. URLs and NATS subjects never travel
//! through the model.
//!
//! Reference (PRIMARY): the leak's `RemoteTriggerTool` is a
//! claude.ai-CCR-API client (a CRUD wrapper for Anthropic's hosted
//! scheduled-agent service). Different concept entirely. Nexo-rs
//! adopts the *name* but ships a generic outbound webhook + NATS
//! publisher per our PHASES.md spec, with allowlist + HMAC the leak
//! has no analog for.
//!
//! Reference (secondary): OpenClaw `research/` — no equivalent
//! generic webhook publisher. Single-process TS reference uses the
//! plugin outbound paths directly.

use serde::Deserialize;

/// Per-trigger entry. The runtime resolves a `RemoteTrigger { name }`
/// call to one of these.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum RemoteTriggerEntry {
    /// Outbound HTTP POST. The runtime signs the body with
    /// HMAC-SHA256 using the secret resolved from `secret_env`.
    Webhook {
        name: String,
        url: String,
        /// Env var holding the HMAC shared secret. The runtime
        /// looks the value up at call time. `None` skips signing
        /// (compatibility with services that don't verify).
        #[serde(default)]
        secret_env: Option<String>,
        /// Per-call timeout in milliseconds. Default 5000.
        #[serde(default = "default_webhook_timeout_ms")]
        timeout_ms: u64,
        /// Per-trigger token bucket — calls per minute. `0` =
        /// unlimited.
        #[serde(default = "default_rate_limit_per_minute")]
        rate_limit_per_minute: u32,
    },
    /// NATS publish via the runtime's broker.
    Nats {
        name: String,
        subject: String,
        #[serde(default = "default_rate_limit_per_minute")]
        rate_limit_per_minute: u32,
    },
}

fn default_webhook_timeout_ms() -> u64 {
    5000
}

fn default_rate_limit_per_minute() -> u32 {
    10
}

impl RemoteTriggerEntry {
    pub fn name(&self) -> &str {
        match self {
            RemoteTriggerEntry::Webhook { name, .. } => name,
            RemoteTriggerEntry::Nats { name, .. } => name,
        }
    }

    pub fn rate_limit_per_minute(&self) -> u32 {
        match self {
            RemoteTriggerEntry::Webhook {
                rate_limit_per_minute,
                ..
            } => *rate_limit_per_minute,
            RemoteTriggerEntry::Nats {
                rate_limit_per_minute,
                ..
            } => *rate_limit_per_minute,
        }
    }
}

/// Hard cap on the JSON payload bytes. Prevents a runaway model
/// from saturating an outbound endpoint via the tool.
pub const REMOTE_TRIGGER_MAX_BODY_BYTES: usize = 256 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_yaml_deserialises() {
        let yaml = r#"
            kind: webhook
            name: ops-pager
            url: https://hooks.example.com/abc
            secret_env: OPS_PAGER_SECRET
            timeout_ms: 3000
            rate_limit_per_minute: 5
        "#;
        let e: RemoteTriggerEntry = serde_yaml::from_str(yaml).unwrap();
        match e {
            RemoteTriggerEntry::Webhook {
                name,
                url,
                secret_env,
                timeout_ms,
                rate_limit_per_minute,
            } => {
                assert_eq!(name, "ops-pager");
                assert_eq!(url, "https://hooks.example.com/abc");
                assert_eq!(secret_env.as_deref(), Some("OPS_PAGER_SECRET"));
                assert_eq!(timeout_ms, 3000);
                assert_eq!(rate_limit_per_minute, 5);
            }
            _ => panic!("expected Webhook"),
        }
    }

    #[test]
    fn nats_yaml_deserialises() {
        let yaml = r#"
            kind: nats
            name: internal-ops
            subject: agent.outbound.ops
        "#;
        let e: RemoteTriggerEntry = serde_yaml::from_str(yaml).unwrap();
        match e {
            RemoteTriggerEntry::Nats {
                name,
                subject,
                rate_limit_per_minute,
            } => {
                assert_eq!(name, "internal-ops");
                assert_eq!(subject, "agent.outbound.ops");
                assert_eq!(rate_limit_per_minute, 10); // default
            }
            _ => panic!("expected Nats"),
        }
    }

    #[test]
    fn webhook_defaults_apply() {
        let yaml = r#"
            kind: webhook
            name: x
            url: https://example.com
        "#;
        let e: RemoteTriggerEntry = serde_yaml::from_str(yaml).unwrap();
        match e {
            RemoteTriggerEntry::Webhook {
                timeout_ms,
                secret_env,
                rate_limit_per_minute,
                ..
            } => {
                assert_eq!(timeout_ms, 5000);
                assert!(secret_env.is_none());
                assert_eq!(rate_limit_per_minute, 10);
            }
            _ => panic!("expected Webhook"),
        }
    }

    #[test]
    fn unknown_field_rejected() {
        let yaml = r#"
            kind: webhook
            name: x
            url: https://example.com
            mystery: 1
        "#;
        let e = serde_yaml::from_str::<RemoteTriggerEntry>(yaml).unwrap_err();
        assert!(e.to_string().contains("mystery"), "got: {e}");
    }

    #[test]
    fn name_helper_works() {
        let e = RemoteTriggerEntry::Nats {
            name: "abc".into(),
            subject: "x".into(),
            rate_limit_per_minute: 0,
        };
        assert_eq!(e.name(), "abc");
        assert_eq!(e.rate_limit_per_minute(), 0);
    }
}
