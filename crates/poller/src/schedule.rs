//! Schedule shape — `every | cron | at`. Returns the next firing
//! instant for the runner's spawn loop. Cron expressions use the
//! 6-field `cron = "0.12"` syntax (with seconds).

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::error::PollerError;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Schedule {
    Every(EverySchedule),
    Cron(CronSchedule),
    At(AtSchedule),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EverySchedule {
    pub every_secs: u64,
    /// Random offset added to each `next_run_at` to avoid thundering
    /// herd when many jobs share a period. None = use runner default.
    #[serde(default)]
    pub stagger_jitter_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CronSchedule {
    /// 6-field expression: `sec min hour day-of-month month day-of-week`.
    pub cron: String,
    /// IANA tz, e.g. `America/Bogota`. Requires `cron-tz` feature.
    /// Default: UTC.
    #[serde(default)]
    pub tz: Option<String>,
    #[serde(default)]
    pub stagger_jitter_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AtSchedule {
    /// RFC3339 timestamp. One-shot — after firing, the job stays
    /// `paused = 1` so it doesn't re-trigger.
    pub at: String,
}

impl Schedule {
    pub fn nominal_interval(&self) -> Duration {
        match self {
            Schedule::Every(e) => Duration::from_secs(e.every_secs.max(1)),
            Schedule::Cron(_) => Duration::from_secs(60),
            Schedule::At(_) => Duration::from_secs(0),
        }
    }

    /// Minimum jitter the runner will use even if the job's schedule
    /// declares 0. Empty value falls through to the runner-wide
    /// default. Tests pin this to 0 by passing the explicit value.
    pub fn jitter_hint(&self) -> Option<u64> {
        match self {
            Schedule::Every(e) => e.stagger_jitter_ms,
            Schedule::Cron(c) => c.stagger_jitter_ms,
            Schedule::At(_) => Some(0),
        }
    }

    /// Compute the next firing instant strictly after `after`. Returns
    /// `None` for one-shot `At` schedules whose target is in the past.
    pub fn next_run_at(
        &self,
        after: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PollerError> {
        match self {
            Schedule::Every(e) => {
                let secs = e.every_secs.max(1);
                Ok(Some(after + chrono::Duration::seconds(secs as i64)))
            }
            Schedule::Cron(c) => Self::next_cron(c, after),
            Schedule::At(a) => {
                let ts = chrono::DateTime::parse_from_rfc3339(&a.at).map_err(|e| {
                    PollerError::Config {
                        job: "<schedule>".into(),
                        reason: format!("invalid at='{}': {e}", a.at),
                    }
                })?;
                let ts_utc = ts.with_timezone(&Utc);
                if ts_utc > after {
                    Ok(Some(ts_utc))
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn next_cron(
        cfg: &CronSchedule,
        after: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PollerError> {
        let schedule: cron::Schedule = cfg.cron.parse().map_err(|e| PollerError::Config {
            job: "<schedule>".into(),
            reason: format!("invalid cron expression '{}': {e}", cfg.cron),
        })?;
        // V1: tz field accepted in YAML but evaluation is in UTC. With
        // the `cron-tz` feature on, this branch swaps to chrono-tz.
        // Surfaced in FOLLOWUPS until DST-correct evaluation lands.
        if cfg.tz.is_some() {
            tracing::trace!(
                "schedule.cron.tz set but `cron-tz` feature off — evaluating in UTC"
            );
        }
        Ok(schedule.after(&after).next())
    }
}

/// Apply the schedule's jitter (or the runner default) to `base`.
/// Pure function so tests can pin the jitter via the supplied seed.
pub fn apply_jitter(base: DateTime<Utc>, jitter_ms: u64, seed: u64) -> DateTime<Utc> {
    if jitter_ms == 0 {
        return base;
    }
    // Deterministic LCG so tests don't need a real RNG. Real boots
    // pass `rand::random()` as seed.
    let offset_ms = seed.wrapping_mul(2862933555777941757).wrapping_add(3037000493) % jitter_ms;
    base + chrono::Duration::milliseconds(offset_ms as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn every_advances_by_period() {
        let s = Schedule::Every(EverySchedule {
            every_secs: 60,
            stagger_jitter_ms: None,
        });
        let next = s.next_run_at(t(1000)).unwrap().unwrap();
        assert_eq!(next, t(1060));
    }

    #[test]
    fn cron_every_5_minutes() {
        let s = Schedule::Cron(CronSchedule {
            cron: "0 */5 * * * *".into(),
            tz: None,
            stagger_jitter_ms: None,
        });
        // start at 12:01:30; next 5-minute boundary is 12:05:00.
        let after = Utc.with_ymd_and_hms(2026, 4, 25, 12, 1, 30).unwrap();
        let expected = Utc.with_ymd_and_hms(2026, 4, 25, 12, 5, 0).unwrap();
        assert_eq!(s.next_run_at(after).unwrap().unwrap(), expected);
    }

    #[test]
    fn at_returns_some_when_future() {
        let s = Schedule::At(AtSchedule {
            at: "2099-01-01T00:00:00Z".into(),
        });
        let next = s.next_run_at(t(0)).unwrap();
        assert!(next.is_some());
    }

    #[test]
    fn at_returns_none_when_past() {
        let s = Schedule::At(AtSchedule {
            at: "2000-01-01T00:00:00Z".into(),
        });
        let next = s.next_run_at(Utc::now()).unwrap();
        assert!(next.is_none());
    }

    #[test]
    fn at_invalid_rfc3339_errors() {
        let s = Schedule::At(AtSchedule {
            at: "not-a-date".into(),
        });
        let err = s.next_run_at(t(0)).unwrap_err();
        assert!(matches!(err, PollerError::Config { .. }));
    }

    #[test]
    fn cron_invalid_expr_errors() {
        let s = Schedule::Cron(CronSchedule {
            cron: "not a cron".into(),
            tz: None,
            stagger_jitter_ms: None,
        });
        let err = s.next_run_at(t(0)).unwrap_err();
        assert!(matches!(err, PollerError::Config { .. }));
    }

    #[test]
    fn jitter_zero_is_identity() {
        let base = t(1000);
        assert_eq!(apply_jitter(base, 0, 42), base);
    }

    #[test]
    fn jitter_stays_within_window() {
        let base = t(1000);
        for seed in 0u64..100 {
            let j = apply_jitter(base, 5000, seed);
            let delta_ms = (j - base).num_milliseconds();
            assert!(delta_ms >= 0 && delta_ms < 5000, "seed {seed} → {delta_ms}");
        }
    }

    #[test]
    fn deserialize_every_yaml() {
        let raw = "every_secs: 60";
        let s: Schedule = serde_yaml::from_str(raw).unwrap();
        match s {
            Schedule::Every(e) => assert_eq!(e.every_secs, 60),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn deserialize_cron_yaml() {
        let raw = "cron: \"0 0 9 * * 1-5\"\ntz: \"America/Bogota\"";
        let s: Schedule = serde_yaml::from_str(raw).unwrap();
        match s {
            Schedule::Cron(c) => {
                assert_eq!(c.cron, "0 0 9 * * 1-5");
                assert_eq!(c.tz.as_deref(), Some("America/Bogota"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn deserialize_at_yaml() {
        let raw = "at: \"2099-01-01T00:00:00Z\"";
        let s: Schedule = serde_yaml::from_str(raw).unwrap();
        assert!(matches!(s, Schedule::At(_)));
    }
}
