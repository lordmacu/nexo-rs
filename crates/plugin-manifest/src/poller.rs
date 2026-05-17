//! Phase 96 — `[plugin.poller]` manifest section.
//!
//! Plugins exposing scheduled work (cron / interval / one-shot
//! pollers) declare the kinds they handle here. The daemon's
//! `nexo-poller` runtime discovers them at boot, dispatches
//! `<broker_topic_prefix>.tick` broker RPC per scheduled fire, and
//! routes the reply (`TickAck` cursor + metrics) back into the
//! runner's persistence + telemetry pipeline.
//!
//! Closes the lineage of Phase 81.33.b.real manifest sections
//! (pairing/http/admin/metrics/dashboard); the 7th and final
//! section so out-of-tree pollers stop using the deprecated
//! `nexo-poller-ext` `StdioRuntime` bridge.

use serde::{Deserialize, Serialize};

/// Per-plugin lifecycle policy for the subprocess that serves the
/// declared kinds.
///
/// - `long_lived` (default) — daemon spawns once at boot, reuses the
///   subprocess across every tick. The subprocess subscribes to its
///   `<broker_topic_prefix>.tick` topic and replies via the message
///   `reply_to` channel. Best for pollers with warm state (OAuth
///   tokens, HTTP connection pools, parsed feeds).
/// - `ephemeral` — daemon spawns a fresh subprocess per scheduled
///   fire, drives the tick over stdio JSON-RPC, kills the child on
///   reply. Best for untrusted code or high-isolation use cases.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PollerLifecycle {
    #[default]
    LongLived,
    Ephemeral,
}

/// `[plugin.poller]` manifest section.
///
/// ```toml
/// [plugin.poller]
/// kinds                 = ["google_calendar"]
/// broker_topic_prefix   = "plugin.poller.google_calendar"
/// lifecycle             = "long_lived"        # default
/// max_concurrent_ticks  = 1                   # default 1, range 1..=64
/// tick_timeout_secs     = 60                  # default 60, range 1..=3600
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginPollerSection {
    /// Kinds this plugin handles. Must be non-empty + unique +
    /// match `^[a-z][a-z0-9_]+$`. Two plugins declaring the same
    /// kind fail the daemon's boot validator.
    pub kinds: Vec<String>,

    /// Broker subject prefix. Daemon publishes
    /// `<prefix>.tick` per scheduled fire; subprocess subscribes to
    /// match. No trailing dot allowed.
    pub broker_topic_prefix: String,

    /// Subprocess lifecycle. See [`PollerLifecycle`].
    #[serde(default)]
    pub lifecycle: PollerLifecycle,

    /// Maximum concurrent ticks per kind. Default 1 — serializes
    /// per kind so OAuth refresh tokens and other shared state can't
    /// race themselves.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_ticks: u32,

    /// Per-tick broker RPC timeout in seconds. Default 60. Range
    /// `[1, 3600]`.
    #[serde(default = "default_tick_timeout")]
    pub tick_timeout_secs: u64,
}

fn default_max_concurrent() -> u32 {
    1
}

fn default_tick_timeout() -> u64 {
    60
}

const KIND_NAME_PATTERN: &str = r"^[a-z][a-z0-9_]+$";

impl PluginPollerSection {
    /// Validate the section against the rules documented above.
    /// Returns a human-readable error string suitable for emission
    /// during daemon boot.
    pub fn validate(&self) -> Result<(), String> {
        if self.kinds.is_empty() {
            return Err("[plugin.poller].kinds must declare at least one kind".into());
        }

        let kind_re = regex::Regex::new(KIND_NAME_PATTERN).unwrap();
        let mut seen = std::collections::HashSet::new();
        for kind in &self.kinds {
            if !kind_re.is_match(kind) {
                return Err(format!(
                    "[plugin.poller].kinds entry '{kind}' must match {KIND_NAME_PATTERN}"
                ));
            }
            if !seen.insert(kind.as_str()) {
                return Err(format!(
                    "[plugin.poller].kinds contains duplicate '{kind}'"
                ));
            }
        }

        if self.broker_topic_prefix.is_empty() {
            return Err("[plugin.poller].broker_topic_prefix cannot be empty".into());
        }
        if self.broker_topic_prefix.ends_with('.') {
            return Err(format!(
                "[plugin.poller].broker_topic_prefix '{}' must not end with '.'",
                self.broker_topic_prefix
            ));
        }
        if self.broker_topic_prefix.contains(' ') {
            return Err(format!(
                "[plugin.poller].broker_topic_prefix '{}' must not contain spaces",
                self.broker_topic_prefix
            ));
        }

        if !(1..=64).contains(&self.max_concurrent_ticks) {
            return Err(format!(
                "[plugin.poller].max_concurrent_ticks {} must be in [1, 64]",
                self.max_concurrent_ticks
            ));
        }

        if !(1..=3600).contains(&self.tick_timeout_secs) {
            return Err(format!(
                "[plugin.poller].tick_timeout_secs {} must be in [1, 3600]",
                self.tick_timeout_secs
            ));
        }

        Ok(())
    }

    /// Topic the daemon publishes a tick request to.
    pub fn tick_topic(&self) -> String {
        format!("{}.tick", self.broker_topic_prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_section() -> PluginPollerSection {
        PluginPollerSection {
            kinds: vec!["google_calendar".into()],
            broker_topic_prefix: "plugin.poller.google_calendar".into(),
            lifecycle: PollerLifecycle::LongLived,
            max_concurrent_ticks: 1,
            tick_timeout_secs: 60,
        }
    }

    #[test]
    fn validate_accepts_minimal() {
        assert!(ok_section().validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_kinds() {
        let s = PluginPollerSection {
            kinds: vec![],
            ..ok_section()
        };
        let err = s.validate().unwrap_err();
        assert!(err.contains("must declare at least one"));
    }

    #[test]
    fn validate_rejects_duplicate_kinds() {
        let s = PluginPollerSection {
            kinds: vec!["rss".into(), "rss".into()],
            ..ok_section()
        };
        let err = s.validate().unwrap_err();
        assert!(err.contains("duplicate"), "got: {err}");
    }

    #[test]
    fn validate_rejects_invalid_kind_format() {
        let cases = vec!["Google", "1leading_digit", "uppercase_X", "hy-phen", ""];
        for kind in cases {
            let s = PluginPollerSection {
                kinds: vec![kind.into()],
                ..ok_section()
            };
            assert!(
                s.validate().is_err(),
                "kind '{kind}' should have been rejected"
            );
        }
    }

    #[test]
    fn validate_accepts_underscore_and_digits_in_kind() {
        let s = PluginPollerSection {
            kinds: vec!["rss_v2".into(), "webhook_poll".into()],
            ..ok_section()
        };
        s.validate().expect("valid kind formats accepted");
    }

    #[test]
    fn validate_rejects_empty_topic_prefix() {
        let s = PluginPollerSection {
            broker_topic_prefix: String::new(),
            ..ok_section()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_trailing_dot_in_prefix() {
        let s = PluginPollerSection {
            broker_topic_prefix: "plugin.poller.x.".into(),
            ..ok_section()
        };
        let err = s.validate().unwrap_err();
        assert!(err.contains("must not end with '.'"));
    }

    #[test]
    fn validate_rejects_space_in_prefix() {
        let s = PluginPollerSection {
            broker_topic_prefix: "plugin poller x".into(),
            ..ok_section()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_out_of_range_max_concurrent() {
        for n in [0u32, 65, 1_000] {
            let s = PluginPollerSection {
                max_concurrent_ticks: n,
                ..ok_section()
            };
            assert!(s.validate().is_err(), "{n} should be out of range");
        }
    }

    #[test]
    fn validate_rejects_out_of_range_timeout() {
        for s_secs in [0u64, 3601, 10_000] {
            let s = PluginPollerSection {
                tick_timeout_secs: s_secs,
                ..ok_section()
            };
            assert!(s.validate().is_err());
        }
    }

    #[test]
    fn tick_topic_appends_dot_tick() {
        assert_eq!(
            ok_section().tick_topic(),
            "plugin.poller.google_calendar.tick"
        );
    }

    #[test]
    fn deserializes_minimal_toml() {
        let toml = r#"
            kinds                 = ["rss"]
            broker_topic_prefix   = "plugin.poller.rss"
        "#;
        let s: PluginPollerSection = toml::from_str(toml).expect("parse");
        assert_eq!(s.kinds, vec!["rss".to_string()]);
        assert_eq!(s.lifecycle, PollerLifecycle::LongLived);
        assert_eq!(s.max_concurrent_ticks, 1);
        assert_eq!(s.tick_timeout_secs, 60);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn deserializes_ephemeral_lifecycle() {
        let toml = r#"
            kinds               = ["sandbox_thing"]
            broker_topic_prefix = "plugin.poller.sandbox"
            lifecycle           = "ephemeral"
        "#;
        let s: PluginPollerSection = toml::from_str(toml).expect("parse");
        assert_eq!(s.lifecycle, PollerLifecycle::Ephemeral);
    }

    #[test]
    fn deserializes_rejects_unknown_field() {
        let toml = r#"
            kinds               = ["rss"]
            broker_topic_prefix = "x.y"
            bogus_key           = 1
        "#;
        let r: Result<PluginPollerSection, _> = toml::from_str(toml);
        assert!(r.is_err());
    }

    #[test]
    fn roundtrips_serde_json() {
        let s = ok_section();
        let json = serde_json::to_string(&s).unwrap();
        let back: PluginPollerSection = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
