//! TaskFlow runtime knobs. Loaded from `config/taskflow.yaml` when present;
//! an absent file yields [`TaskflowConfig::default`].

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskflowConfig {
    /// How often `WaitEngine::tick()` runs. Parsed by `humantime`.
    #[serde(default = "default_tick_interval")]
    pub tick_interval: String,
    /// Maximum future deadline allowed for `WaitCondition::Timer`. Parsed by `humantime`.
    #[serde(default = "default_timer_max_horizon")]
    pub timer_max_horizon: String,
    /// SQLite path. Falls back to `TASKFLOW_DB_PATH` env then `./data/taskflow.db`.
    #[serde(default)]
    pub db_path: Option<String>,
}

impl Default for TaskflowConfig {
    fn default() -> Self {
        Self {
            tick_interval: default_tick_interval(),
            timer_max_horizon: default_timer_max_horizon(),
            db_path: None,
        }
    }
}

fn default_tick_interval() -> String {
    "5s".into()
}

fn default_timer_max_horizon() -> String {
    "30d".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = TaskflowConfig::default();
        assert_eq!(c.tick_interval, "5s");
        assert_eq!(c.timer_max_horizon, "30d");
        assert!(c.db_path.is_none());
    }

    #[test]
    fn parses_yaml() {
        let c: TaskflowConfig = serde_yaml::from_str(
            "tick_interval: 10s\ntimer_max_horizon: 7d\ndb_path: /var/lib/taskflow.db\n",
        )
        .unwrap();
        assert_eq!(c.tick_interval, "10s");
        assert_eq!(c.timer_max_horizon, "7d");
        assert_eq!(c.db_path.as_deref(), Some("/var/lib/taskflow.db"));
    }

    #[test]
    fn unknown_field_rejected() {
        let err = serde_yaml::from_str::<TaskflowConfig>("bogus: 1\n").expect_err("deny_unknown");
        assert!(err.to_string().contains("bogus"));
    }
}
