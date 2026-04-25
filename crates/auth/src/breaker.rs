//! Per-(channel, instance) circuit breaker registry.
//!
//! Each labelled WhatsApp / Telegram / Google account gets its own
//! [`CircuitBreaker`] so a 429 from one number does not lock out the
//! others. The registry is built lazily from the `CredentialsBundle`
//! and shared with plugin tools through `Arc<BreakerRegistry>`.

use std::sync::Arc;

use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig};
use dashmap::DashMap;

use crate::handle::{Channel, CredentialHandle};

#[derive(Default)]
pub struct BreakerRegistry {
    /// Key: `format!("{channel}:{instance}")`. The dashmap is built
    /// lazily — first call to `breaker_for` allocates.
    breakers: DashMap<String, Arc<CircuitBreaker>>,
    config: CircuitBreakerConfig,
}

impl std::fmt::Debug for BreakerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BreakerRegistry")
            .field("breakers", &self.breakers.len())
            .finish()
    }
}

impl BreakerRegistry {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            breakers: DashMap::new(),
            config,
        }
    }

    fn key(channel: Channel, instance: &str) -> String {
        format!("{channel}:{instance}")
    }

    pub fn breaker_for(&self, channel: Channel, instance: &str) -> Arc<CircuitBreaker> {
        let key = Self::key(channel, instance);
        // `entry().or_insert_with` keeps a single allocation across
        // contended first-touches.
        self.breakers
            .entry(key.clone())
            .or_insert_with(|| Arc::new(CircuitBreaker::new(key, self.config.clone())))
            .value()
            .clone()
    }

    pub fn for_handle(&self, handle: &CredentialHandle) -> Arc<CircuitBreaker> {
        self.breaker_for(handle.channel(), handle.account_id_raw())
    }

    /// Snapshot of every breaker's state for telemetry rendering.
    pub fn iter_states(&self) -> Vec<(Channel, String, BreakerState)> {
        self.breakers
            .iter()
            .filter_map(|entry| {
                let key = entry.key().clone();
                let (channel_str, instance) = key.split_once(':')?;
                let channel: Channel = match channel_str {
                    "whatsapp" => crate::handle::WHATSAPP,
                    "telegram" => crate::handle::TELEGRAM,
                    "google" => crate::handle::GOOGLE,
                    _ => return None,
                };
                let state = if entry.value().is_open() {
                    BreakerState::Open
                } else {
                    BreakerState::Closed
                };
                Some((channel, instance.to_string(), state))
            })
            .collect()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    HalfOpen,
    Open,
}

impl From<BreakerState> for crate::telemetry::BreakerState {
    fn from(s: BreakerState) -> Self {
        match s {
            BreakerState::Closed => crate::telemetry::BreakerState::Closed,
            BreakerState::HalfOpen => crate::telemetry::BreakerState::HalfOpen,
            BreakerState::Open => crate::telemetry::BreakerState::Open,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::{TELEGRAM, WHATSAPP};

    #[test]
    fn registry_returns_same_arc_for_same_key() {
        let reg = BreakerRegistry::default();
        let a = reg.breaker_for(WHATSAPP, "personal");
        let b = reg.breaker_for(WHATSAPP, "personal");
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn distinct_keys_yield_distinct_arcs() {
        let reg = BreakerRegistry::default();
        let a = reg.breaker_for(WHATSAPP, "personal");
        let b = reg.breaker_for(WHATSAPP, "work");
        let c = reg.breaker_for(TELEGRAM, "personal");
        assert!(!Arc::ptr_eq(&a, &b));
        assert!(!Arc::ptr_eq(&a, &c));
    }

    #[test]
    fn opening_one_breaker_does_not_affect_others() {
        let reg = BreakerRegistry::default();
        let a = reg.breaker_for(WHATSAPP, "personal");
        let b = reg.breaker_for(WHATSAPP, "work");
        a.trip();
        assert!(a.is_open());
        assert!(!b.is_open(), "sibling breaker must stay closed");
    }

    #[test]
    fn for_handle_dispatches_by_channel_and_account() {
        let reg = BreakerRegistry::default();
        let h_wa = CredentialHandle::new(WHATSAPP, "personal", "ana");
        let h_tg = CredentialHandle::new(TELEGRAM, "personal", "ana");
        let bw = reg.for_handle(&h_wa);
        let bt = reg.for_handle(&h_tg);
        assert!(!Arc::ptr_eq(&bw, &bt));
    }

    #[test]
    fn iter_states_after_trip() {
        let reg = BreakerRegistry::default();
        let a = reg.breaker_for(WHATSAPP, "x");
        a.trip();
        let states = reg.iter_states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].2, BreakerState::Open);
    }
}
