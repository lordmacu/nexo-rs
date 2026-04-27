//! Per-account worker health snapshot (Phase 48.3).
//!
//! `AccountWorker` updates its `Arc<RwLock<AccountHealth>>` whenever the
//! state machine advances or a network op completes. The plugin exposes
//! `EmailPlugin::health()` later (Phase 48.10) by snapshotting every
//! account.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WorkerState {
    /// Initial / reconnecting after a failure. CB may be open.
    #[default]
    Connecting,
    /// IDLE command active, awaiting EXISTS push.
    Idle,
    /// Server lacks IDLE or CB pushed us into 60s polling.
    Polling,
    /// CB open beyond reconnect window — operator-visible "channel
    /// unavailable" state.
    Down,
}

impl WorkerState {
    /// Numeric encoding for the Prometheus gauge `email_imap_state`.
    /// Stable across releases — operators key dashboards on these.
    pub const fn as_metric_value(&self) -> i64 {
        match self {
            WorkerState::Connecting => 0,
            WorkerState::Idle => 1,
            WorkerState::Polling => 2,
            WorkerState::Down => 3,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct AccountHealth {
    pub state: WorkerState,
    /// Unix seconds at which the most recent IDLE wait was confirmed
    /// alive (server pushed something or reissue cycle landed).
    pub last_idle_alive_ts: i64,
    /// Unix seconds at which the most recent UID SEARCH poll completed.
    pub last_poll_ts: i64,
    /// Unix seconds at which the most recent connect+auth succeeded.
    pub last_connect_ok_ts: i64,
    pub consecutive_failures: u32,
    pub messages_seen_total: u64,
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_value_is_stable() {
        assert_eq!(WorkerState::Connecting.as_metric_value(), 0);
        assert_eq!(WorkerState::Idle.as_metric_value(), 1);
        assert_eq!(WorkerState::Polling.as_metric_value(), 2);
        assert_eq!(WorkerState::Down.as_metric_value(), 3);
    }

    #[test]
    fn default_state_is_connecting() {
        let h = AccountHealth::default();
        assert_eq!(h.state, WorkerState::Connecting);
        assert_eq!(h.consecutive_failures, 0);
        assert_eq!(h.messages_seen_total, 0);
        assert!(h.last_error.is_none());
    }
}
