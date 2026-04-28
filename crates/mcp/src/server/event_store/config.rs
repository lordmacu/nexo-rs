//! Phase 76.8 — runtime config for the session event store.
//! YAML mirror lives in `crates/config/src/types/mcp_server.rs`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SessionEventStoreConfig {
    /// Default `true` so an operator who reaches Phase 76.8 gets
    /// resumability by default. Opt out with `enabled: false`.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// SQLite file path. Resolved relative to process CWD when
    /// relative; recommend an absolute path in production.
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    /// Per-session ring cap. Append paths trigger
    /// `purge_oldest_for_session(keep=max)` when a session exceeds.
    #[serde(default = "default_max_per_session")]
    pub max_events_per_session: u64,
    /// Hard cap on rows replayed per reconnect. Guards a malicious
    /// client from forcing the daemon to stream a multi-GB tail
    /// through one SSE connection.
    #[serde(default = "default_max_replay_batch")]
    pub max_replay_batch: usize,
    /// Background purge worker interval. Prunes events older than
    /// the session max-lifetime cutoff.
    #[serde(default = "default_purge_interval_secs")]
    pub purge_interval_secs: u64,
}

fn default_enabled() -> bool {
    true
}
fn default_db_path() -> PathBuf {
    PathBuf::from("data/mcp_sessions.db")
}
fn default_max_per_session() -> u64 {
    10_000
}
fn default_max_replay_batch() -> usize {
    1_000
}
fn default_purge_interval_secs() -> u64 {
    60
}

impl Default for SessionEventStoreConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            db_path: default_db_path(),
            max_events_per_session: default_max_per_session(),
            max_replay_batch: default_max_replay_batch(),
            purge_interval_secs: default_purge_interval_secs(),
        }
    }
}

impl SessionEventStoreConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.db_path.as_os_str().is_empty() {
            return Err("session_event_store.db_path must not be empty".into());
        }
        if self.max_events_per_session == 0 {
            return Err("session_event_store.max_events_per_session must be > 0".into());
        }
        if self.max_replay_batch == 0 {
            return Err("session_event_store.max_replay_batch must be > 0".into());
        }
        if self.max_replay_batch > 10_000 {
            return Err(format!(
                "session_event_store.max_replay_batch ({}) exceeds 10_000 ceiling",
                self.max_replay_batch
            ));
        }
        if self.purge_interval_secs == 0 {
            return Err("session_event_store.purge_interval_secs must be > 0".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_validates() {
        SessionEventStoreConfig::default().validate().unwrap();
    }

    #[test]
    fn disabled_skips_validation() {
        let mut c = SessionEventStoreConfig::default();
        c.enabled = false;
        c.db_path = PathBuf::new();
        c.max_replay_batch = 0;
        c.validate().unwrap();
    }

    #[test]
    fn rejects_zero_max_replay_batch_when_enabled() {
        let mut c = SessionEventStoreConfig::default();
        c.max_replay_batch = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_replay_batch_above_ceiling() {
        let mut c = SessionEventStoreConfig::default();
        c.max_replay_batch = 10_001;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_empty_db_path_when_enabled() {
        let mut c = SessionEventStoreConfig::default();
        c.db_path = PathBuf::new();
        assert!(c.validate().is_err());
    }
}
