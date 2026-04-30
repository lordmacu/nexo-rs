//! Phase 80.2-80.6 — cron jitter + killswitch config.
//!
//! Six-knob configuration replacing the legacy single-`pct` jitter.
//! Hot-reloadable via Phase 18 ArcSwap path. Per-binding override
//! is deferred to 80.2.b — slim MVP is global config.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct CronJitterConfig {
    /// Master killswitch. `false` makes the runner skip every tick
    /// (entries stay in storage; operator flips back to `true` to
    /// resume). Hot-reloaded — observed on the next tick.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Fraction of the next-fire interval used as the jitter window
    /// for **recurring** entries. `0.0` disables jitter for
    /// recurring. Range `[0.0, 1.0]`; values > 1.0 are clamped at
    /// validate time.
    ///
    /// Example: `recurring_frac = 0.1` on a 60-minute interval →
    /// jitter window is up to 6 minutes (capped further by
    /// `recurring_cap_ms`).
    #[serde(default = "default_recurring_frac")]
    pub recurring_frac: f64,

    /// Absolute cap on the recurring jitter window in milliseconds.
    /// `0` disables jitter for recurring (regardless of
    /// `recurring_frac`). Default 5 minutes — keeps long intervals
    /// from spreading too widely.
    #[serde(default = "default_recurring_cap_ms")]
    pub recurring_cap_ms: i64,

    /// Maximum backward lead applied to **one-shot** entries in
    /// milliseconds. The one-shot fires `lead` ms BEFORE
    /// `next_fire_at`, never after. `0` disables one-shot lead.
    #[serde(default = "default_one_shot_max_ms")]
    pub one_shot_max_ms: i64,

    /// Minimum backward lead in milliseconds. When the deterministic
    /// jitter would produce less than `floor`, the helper returns
    /// `floor`. `0` disables the floor.
    #[serde(default)]
    pub one_shot_floor_ms: i64,

    /// Only apply one-shot lead when the `next_fire_at`'s minute
    /// falls on a multiple of this value. `1` = every minute (always
    /// jitter); `5` = on minutes 0/5/10/15/...; `0` = NEVER jitter
    /// one-shots (the helper treats `mod 0` as never-fire to avoid
    /// a divide-by-zero panic).
    #[serde(default = "default_one_shot_minute_mod")]
    pub one_shot_minute_mod: u32,

    /// Auto-expire **recurring** entries older than this many
    /// milliseconds. `0` disables auto-expiry (default).
    /// `permanent: true` entries are exempt regardless of this
    /// value.
    #[serde(default)]
    pub recurring_max_age_ms: i64,
}

fn default_enabled() -> bool {
    true
}
fn default_recurring_frac() -> f64 {
    0.10
}
fn default_recurring_cap_ms() -> i64 {
    5 * 60_000 // 5 minutes
}
fn default_one_shot_max_ms() -> i64 {
    30_000 // 30 seconds
}
fn default_one_shot_minute_mod() -> u32 {
    1 // jitter every minute
}

impl Default for CronJitterConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            recurring_frac: default_recurring_frac(),
            recurring_cap_ms: default_recurring_cap_ms(),
            one_shot_max_ms: default_one_shot_max_ms(),
            one_shot_floor_ms: 0,
            one_shot_minute_mod: default_one_shot_minute_mod(),
            recurring_max_age_ms: 0,
        }
    }
}

impl CronJitterConfig {
    /// Validate at boot. Operator typos that produce silent
    /// no-jitter behaviour become errors here instead of mysterious
    /// runtime nothing-happens.
    pub fn validate(&self) -> Result<(), String> {
        if self.recurring_frac < 0.0 || self.recurring_frac > 1.0 {
            return Err(format!(
                "cron_jitter.recurring_frac {} is out of range [0.0, 1.0]",
                self.recurring_frac
            ));
        }
        if !self.recurring_frac.is_finite() {
            return Err("cron_jitter.recurring_frac must be finite".into());
        }
        if self.recurring_cap_ms < 0 {
            return Err("cron_jitter.recurring_cap_ms must be >= 0".into());
        }
        if self.one_shot_max_ms < 0 {
            return Err("cron_jitter.one_shot_max_ms must be >= 0".into());
        }
        if self.one_shot_floor_ms < 0 {
            return Err("cron_jitter.one_shot_floor_ms must be >= 0".into());
        }
        if self.one_shot_floor_ms > self.one_shot_max_ms && self.one_shot_max_ms > 0 {
            return Err(format!(
                "cron_jitter.one_shot_floor_ms ({}) exceeds one_shot_max_ms ({})",
                self.one_shot_floor_ms, self.one_shot_max_ms
            ));
        }
        if self.recurring_max_age_ms < 0 {
            return Err("cron_jitter.recurring_max_age_ms must be >= 0".into());
        }
        // `one_shot_minute_mod = 0` is the documented "never jitter
        // one-shots" sentinel; not an error.
        Ok(())
    }

    /// Backward-compat constructor — `pct` (0..=100) maps to
    /// `recurring_frac = pct/100.0`. Used by
    /// `CronRunner::with_jitter_pct` to keep legacy callers
    /// working without YAML changes.
    pub fn from_legacy_pct(pct: u32) -> Self {
        let pct = pct.min(100);
        Self {
            recurring_frac: pct as f64 / 100.0,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_enabled_with_sensible_values() {
        let c = CronJitterConfig::default();
        assert!(c.enabled);
        assert_eq!(c.recurring_frac, 0.10);
        assert_eq!(c.recurring_cap_ms, 5 * 60_000);
        assert_eq!(c.one_shot_max_ms, 30_000);
        assert_eq!(c.one_shot_floor_ms, 0);
        assert_eq!(c.one_shot_minute_mod, 1);
        assert_eq!(c.recurring_max_age_ms, 0);
        c.validate().unwrap();
    }

    #[test]
    fn from_legacy_pct_maps_to_recurring_frac() {
        let c = CronJitterConfig::from_legacy_pct(20);
        assert!((c.recurring_frac - 0.20).abs() < 1e-9);
    }

    #[test]
    fn from_legacy_pct_clamps_above_100() {
        let c = CronJitterConfig::from_legacy_pct(250);
        assert_eq!(c.recurring_frac, 1.0);
    }

    #[test]
    fn validate_rejects_recurring_frac_out_of_range() {
        let c = CronJitterConfig {
            recurring_frac: 1.5,
            ..Default::default()
        };
        assert!(c.validate().is_err());
        let c = CronJitterConfig {
            recurring_frac: -0.1,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_floor_above_max() {
        let c = CronJitterConfig {
            one_shot_max_ms: 1000,
            one_shot_floor_ms: 5000,
            ..Default::default()
        };
        let err = c.validate().unwrap_err();
        assert!(err.contains("exceeds"));
    }

    #[test]
    fn validate_accepts_minute_mod_zero_as_never_sentinel() {
        let c = CronJitterConfig {
            one_shot_minute_mod: 0,
            ..Default::default()
        };
        c.validate().unwrap();
    }

    #[test]
    fn yaml_round_trip_full() {
        let yaml = "\
enabled: true
recurring_frac: 0.25
recurring_cap_ms: 600000
one_shot_max_ms: 60000
one_shot_floor_ms: 5000
one_shot_minute_mod: 5
recurring_max_age_ms: 2592000000
";
        let parsed: CronJitterConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.recurring_frac, 0.25);
        assert_eq!(parsed.recurring_cap_ms, 600_000);
        assert_eq!(parsed.recurring_max_age_ms, 2_592_000_000);
        parsed.validate().unwrap();
    }

    #[test]
    fn yaml_partial_uses_defaults() {
        let yaml = "enabled: false\n";
        let parsed: CronJitterConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(!parsed.enabled);
        assert_eq!(parsed.recurring_frac, 0.10);
    }
}
