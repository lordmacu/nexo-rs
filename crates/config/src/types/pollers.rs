//! YAML shape for `config/pollers.yaml`. The `Schedule` enum lives in
//! `agent-poller` to keep that crate self-contained, but here we
//! mirror its struct shape via `serde_yaml::Value` so `agent-config`
//! does not depend on `agent-poller` (avoids a cycle: config ← poller ← config).

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct PollersConfigFile {
    pub pollers: PollersConfig,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct PollersConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// SQLite file holding `poll_state` + `poll_lease`. Default
    /// `./data/poller.db`. Created if missing.
    #[serde(default = "default_state_db")]
    pub state_db: String,
    /// Global jitter applied when a job's schedule does not declare
    /// its own. Defaults to 5000 ms.
    #[serde(default = "default_jitter")]
    pub default_jitter_ms: u64,
    /// Lease TTL multiplier — runner sets `running_until = now + ttl_factor * interval`.
    /// Capped to a minimum of 30s in code.
    #[serde(default = "default_ttl_factor")]
    pub lease_ttl_factor: f32,
    /// Failure-alert cooldown per job (seconds). Cooldown anchor is
    /// persisted in `poll_state.last_failure_alert_at`.
    #[serde(default = "default_failure_cooldown")]
    pub failure_alert_cooldown_secs: u64,
    /// Threshold of consecutive errors before the per-job circuit
    /// breaker trips.
    #[serde(default = "default_breaker_threshold")]
    pub breaker_threshold: u32,
    #[serde(default)]
    pub jobs: Vec<PollerJob>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct PollerJob {
    /// Unique identifier — used for state, metrics, admin endpoints.
    pub id: String,
    /// Discriminator matching `Poller::kind()`.
    pub kind: String,
    /// Agent whose Phase 17 credentials this job uses.
    pub agent: String,
    /// `Every`, `Cron`, or `At` — parsed by `agent-poller`.
    pub schedule: serde_yaml::Value,
    /// Module-specific options. Validated by the module's `validate`.
    #[serde(default)]
    pub config: serde_yaml::Value,
    /// Where to dispatch failure alerts after the breaker trips.
    /// Empty = log only, no alert.
    #[serde(default)]
    pub failure_to: Option<DeliveryTarget>,
    /// Stop the job from auto-running on boot. Useful to deploy a job
    /// in paused state and turn it on later via `agent pollers resume`.
    #[serde(default)]
    pub paused_on_boot: bool,
    /// Map for any extras the module reads but the runner does not.
    /// Allows feature flags without bumping the schema. Use sparingly.
    #[serde(default)]
    pub extra: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DeliveryTarget {
    /// `whatsapp` | `telegram` | `google` — must match a registered Channel.
    pub channel: String,
    pub to: String,
}

fn default_enabled() -> bool { true }
fn default_state_db() -> String { "./data/poller.db".to_string() }
fn default_jitter() -> u64 { 5_000 }
fn default_ttl_factor() -> f32 { 2.0 }
fn default_failure_cooldown() -> u64 { 3_600 }
fn default_breaker_threshold() -> u32 { 5 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_yaml() {
        let yaml = r#"
            pollers:
              jobs:
                - id: ana_leads
                  kind: gmail
                  agent: ana
                  schedule:
                    every_secs: 60
                  config:
                    query: "is:unread"
        "#;
        let f: PollersConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.pollers.enabled);
        assert_eq!(f.pollers.jobs.len(), 1);
        let j = &f.pollers.jobs[0];
        assert_eq!(j.id, "ana_leads");
        assert_eq!(j.kind, "gmail");
        assert_eq!(j.agent, "ana");
        assert!(j.failure_to.is_none());
    }

    #[test]
    fn parses_failure_to() {
        let yaml = r#"
            pollers:
              jobs:
                - id: x
                  kind: rss
                  agent: ana
                  schedule: { every_secs: 300 }
                  failure_to:
                    channel: telegram
                    to: "1194292426"
        "#;
        let f: PollersConfigFile = serde_yaml::from_str(yaml).unwrap();
        let ft = f.pollers.jobs[0].failure_to.as_ref().unwrap();
        assert_eq!(ft.channel, "telegram");
        assert_eq!(ft.to, "1194292426");
    }

    #[test]
    fn defaults_are_sensible() {
        let yaml = "pollers: {}";
        let f: PollersConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.pollers.enabled);
        assert_eq!(f.pollers.state_db, "./data/poller.db");
        assert_eq!(f.pollers.default_jitter_ms, 5000);
        assert_eq!(f.pollers.lease_ttl_factor, 2.0);
        assert_eq!(f.pollers.failure_alert_cooldown_secs, 3600);
        assert_eq!(f.pollers.breaker_threshold, 5);
        assert!(f.pollers.jobs.is_empty());
    }

    #[test]
    fn parses_paused_on_boot() {
        let yaml = r#"
            pollers:
              jobs:
                - id: x
                  kind: gmail
                  agent: ana
                  schedule: { every_secs: 60 }
                  paused_on_boot: true
        "#;
        let f: PollersConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.pollers.jobs[0].paused_on_boot);
    }
}
