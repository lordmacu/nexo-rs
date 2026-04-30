//! Phase 80.14 — AWAY_SUMMARY re-connection digest config.
//!
//! When enabled, the runtime composes a short markdown digest the
//! first time a user sends a message after being silent for
//! `threshold_hours`. The digest summarises goals + aborts +
//! failures recorded in the Phase 72 turn-log during the silence
//! window. Default disabled; per-binding opt-in.

use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AwaySummaryConfig {
    /// Master toggle. `false` (default) keeps the runtime quiet.
    #[serde(default)]
    pub enabled: bool,

    /// Hours of silence before the next inbound triggers a digest.
    /// Default 4h. `0` would fire on every inbound — operators that
    /// want that should pair it with their own outer rate limiting.
    #[serde(default = "default_threshold_hours")]
    pub threshold_hours: u64,

    /// Max events to include in the digest. Larger windows still
    /// fire but truncate with a `(+N more)` suffix. Default 50.
    #[serde(default = "default_max_events")]
    pub max_events: usize,
}

impl Default for AwaySummaryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_hours: default_threshold_hours(),
            max_events: default_max_events(),
        }
    }
}

fn default_threshold_hours() -> u64 {
    4
}
fn default_max_events() -> usize {
    50
}

impl AwaySummaryConfig {
    /// Validate at boot. `threshold_hours > 30 days` is rejected as
    /// implausible (likely operator confusion); `max_events == 0`
    /// would render an empty digest, also rejected.
    pub fn validate(&self) -> Result<(), String> {
        if self.threshold_hours > 24 * 30 {
            return Err(format!(
                "away_summary.threshold_hours={} is > 30 days; \
                 reject as implausible",
                self.threshold_hours
            ));
        }
        if self.max_events == 0 {
            return Err("away_summary.max_events must be > 0".into());
        }
        Ok(())
    }

    pub fn threshold(&self) -> Duration {
        Duration::from_secs(self.threshold_hours * 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_with_4h_threshold() {
        let cfg = AwaySummaryConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.threshold_hours, 4);
        assert_eq!(cfg.max_events, 50);
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_rejects_giant_threshold() {
        let cfg = AwaySummaryConfig {
            enabled: true,
            threshold_hours: 24 * 31,
            max_events: 50,
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("30 days"));
    }

    #[test]
    fn validate_rejects_zero_max_events() {
        let cfg = AwaySummaryConfig {
            enabled: true,
            threshold_hours: 4,
            max_events: 0,
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("max_events"));
    }

    #[test]
    fn yaml_round_trip_full() {
        let yaml = "\
enabled: true
threshold_hours: 6
max_events: 100
";
        let parsed: AwaySummaryConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.threshold_hours, 6);
        assert_eq!(parsed.max_events, 100);
        parsed.validate().unwrap();
    }

    #[test]
    fn yaml_round_trip_partial_uses_defaults() {
        // Only `enabled: true` set — others fall to defaults.
        let yaml = "enabled: true\n";
        let parsed: AwaySummaryConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.threshold_hours, 4);
        assert_eq!(parsed.max_events, 50);
    }

    #[test]
    fn threshold_returns_duration() {
        let cfg = AwaySummaryConfig {
            enabled: true,
            threshold_hours: 6,
            max_events: 50,
        };
        assert_eq!(cfg.threshold(), Duration::from_secs(6 * 3600));
    }
}
