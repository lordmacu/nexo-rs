//! Phase 80.1 — autoDream consolidation config.
//!
//! Lives here (not in `nexo-dream`) so `nexo-config::AgentConfig` can
//! embed it without a circular dep. `nexo-dream` re-exports for
//! ergonomic operator imports.
//!
//! Defaults mirror `claude-code-leak/src/services/autoDream/autoDream.ts:63-66`
//! (`minHours=24, minSessions=5`) and `consolidationLock.ts:19`
//! (`HOLDER_STALE_MS = 60*60*1000`).

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AutoDreamConfig {
    /// Master toggle. Defaults `false` (opt-in matches OpenClaw stance).
    #[serde(default)]
    pub enabled: bool,

    /// Minimum elapsed since last consolidation. Default 24h
    /// (leak `autoDream.ts:64`).
    #[serde(default = "defaults::min_hours", with = "humantime_serde")]
    pub min_hours: Duration,

    /// Minimum transcripts touched since last run. Default 5
    /// (leak `autoDream.ts:65`).
    #[serde(default = "defaults::min_sessions")]
    pub min_sessions: u32,

    /// Throttle scan when time-gate passes but session-gate doesn't.
    /// Default 10min (leak `autoDream.ts:56`).
    #[serde(default = "defaults::scan_interval", with = "humantime_serde")]
    pub scan_interval: Duration,

    /// Max wall-clock for one fork run. Nexo addition (leak has no
    /// explicit timeout). Default 5min.
    #[serde(default = "defaults::fork_timeout", with = "humantime_serde")]
    pub fork_timeout: Duration,

    /// Lock reclaim threshold. Default 1h
    /// (leak `consolidationLock.ts:19`).
    #[serde(default = "defaults::holder_stale", with = "humantime_serde")]
    pub holder_stale: Duration,

    /// Memory directory override. Caller provides; no autodetect in
    /// nexo (leak has `getAutoMemPath()` global).
    #[serde(default)]
    pub memory_dir: Option<PathBuf>,
}

impl Default for AutoDreamConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_hours: defaults::min_hours(),
            min_sessions: defaults::min_sessions(),
            scan_interval: defaults::scan_interval(),
            fork_timeout: defaults::fork_timeout(),
            holder_stale: defaults::holder_stale(),
            memory_dir: None,
        }
    }
}

mod defaults {
    use std::time::Duration;
    pub fn min_hours() -> Duration {
        Duration::from_secs(24 * 60 * 60)
    }
    pub fn min_sessions() -> u32 {
        5
    }
    pub fn scan_interval() -> Duration {
        Duration::from_secs(10 * 60)
    }
    pub fn fork_timeout() -> Duration {
        Duration::from_secs(5 * 60)
    }
    pub fn holder_stale() -> Duration {
        Duration::from_secs(60 * 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_leak() {
        let c = AutoDreamConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.min_hours, Duration::from_secs(24 * 3600));
        assert_eq!(c.min_sessions, 5);
        assert_eq!(c.scan_interval, Duration::from_secs(10 * 60));
        assert_eq!(c.fork_timeout, Duration::from_secs(5 * 60));
        assert_eq!(c.holder_stale, Duration::from_secs(60 * 60));
    }

    #[test]
    fn yaml_round_trip() {
        let yaml = "enabled: true\nmin_hours: 25h\nmin_sessions: 7\n";
        let cfg: AutoDreamConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.min_hours, Duration::from_secs(25 * 3600));
        assert_eq!(cfg.min_sessions, 7);
        // Other fields fall back to defaults.
        assert_eq!(cfg.fork_timeout, defaults::fork_timeout());
    }
}
