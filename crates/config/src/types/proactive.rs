use serde::Deserialize;

fn default_tick_interval() -> u64 {
    600
}

fn default_jitter() -> f32 {
    25.0
}

fn default_max_idle_secs() -> u64 {
    86_400
}

fn default_true() -> bool {
    true
}

fn default_daily_turn_budget() -> u32 {
    200
}

/// Phase 77.20 — per-agent / per-binding proactive tick loop configuration.
///
/// When `enabled: true` the driver-loop keeps the goal alive after each LLM
/// turn. Instead of terminating, it waits for the next `<tick>` injection
/// and feeds it to the agent as a low-priority user message. The agent may
/// call `Sleep { duration_ms, reason }` to control the wait interval.
///
/// YAML example (agent-level default):
/// ```yaml
/// agents:
///   - id: ana
///     proactive:
///       enabled: true
///       tick_interval_secs: 600
///       jitter_pct: 25
///       max_idle_secs: 86400
///       initial_greeting: true
///       cache_aware_schedule: true
/// ```
///
/// Per-binding override (replaces the whole struct when present):
/// ```yaml
/// inbound_bindings:
///   - plugin: whatsapp
///     proactive:
///       enabled: true
///       tick_interval_secs: 120
/// ```
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProactiveConfig {
    /// Enable the proactive tick loop for goals spawned from this binding.
    #[serde(default)]
    pub enabled: bool,

    /// Base interval between ticks when the agent does NOT call Sleep.
    /// Overridden by `Sleep { duration_ms }` on a per-turn basis.
    #[serde(default = "default_tick_interval")]
    pub tick_interval_secs: u64,

    /// ± fraction jitter applied to `tick_interval_secs` to prevent
    /// thundering-herd when many goals share the same interval.
    /// Value is a percent in [0.0, 100.0]. Default 25 = ±25 %.
    #[serde(default = "default_jitter")]
    pub jitter_pct: f32,

    /// Hard cap before forcing a wake-up. Default 24 h.
    #[serde(default = "default_max_idle_secs")]
    pub max_idle_secs: u64,

    /// Mirror Claude Code's proactive startup behavior: start with a short
    /// greeting before settling into the tick loop.
    #[serde(default = "default_true")]
    pub initial_greeting: bool,

    /// Bias Sleep durations away from the prompt-cache dead zone.
    #[serde(default = "default_true")]
    pub cache_aware_schedule: bool,

    /// Allow `tick_interval_secs < 60`. Off by default because each tick is a
    /// billed model turn.
    #[serde(default)]
    pub allow_short_intervals: bool,

    /// Per-binding daily tick budget. 0 disables the guard.
    #[serde(default = "default_daily_turn_budget")]
    pub daily_turn_budget: u32,
}

impl Default for ProactiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tick_interval_secs: default_tick_interval(),
            jitter_pct: default_jitter(),
            max_idle_secs: default_max_idle_secs(),
            initial_greeting: true,
            cache_aware_schedule: true,
            allow_short_intervals: false,
            daily_turn_budget: default_daily_turn_budget(),
        }
    }
}

impl ProactiveConfig {
    pub fn effective_tick_interval_secs(&self) -> u64 {
        if self.allow_short_intervals {
            self.tick_interval_secs.max(1)
        } else {
            self.tick_interval_secs.max(60)
        }
    }

    pub fn normalized_jitter_pct(&self) -> f32 {
        self.jitter_pct.clamp(0.0, 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_phase_77_20() {
        let c = ProactiveConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.tick_interval_secs, 600);
        assert_eq!(c.jitter_pct, 25.0);
        assert_eq!(c.max_idle_secs, 86_400);
        assert!(c.initial_greeting);
        assert!(c.cache_aware_schedule);
        assert!(!c.allow_short_intervals);
        assert_eq!(c.daily_turn_budget, 200);
    }

    #[test]
    fn tick_interval_enforces_cost_floor_unless_opted_in() {
        let mut c = ProactiveConfig {
            tick_interval_secs: 5,
            ..Default::default()
        };
        assert_eq!(c.effective_tick_interval_secs(), 60);
        c.allow_short_intervals = true;
        assert_eq!(c.effective_tick_interval_secs(), 5);
    }
}
