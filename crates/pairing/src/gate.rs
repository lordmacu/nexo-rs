//! Hot-path inbound gate.
//!
//! Plugins call [`PairingGate::should_admit`] before publishing an
//! inbound event to the broker. Three outcomes:
//!
//! - [`Decision::Admit`] — sender is in `allow_from`; publish.
//! - [`Decision::Challenge`] — first time we see this sender and the
//!   binding has `auto_challenge: true`; the plugin replies with the
//!   code and drops the message.
//! - [`Decision::Drop`] — `auto_challenge: false` and sender unknown,
//!   or max-pending exhausted; silent drop.
//!
//! A short-TTL cache (30 s) keeps the SQLite hit out of the hot path
//! for already-allowlisted senders. Operators that need an immediate
//! revoke can call [`PairingGate::flush_cache`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::store::PairingStore;
use crate::types::{Decision, PairingError, PairingPolicy};

const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct CacheEntry {
    decision: Decision,
    expires_at: Instant,
}

pub struct PairingGate {
    store: Arc<PairingStore>,
    cache: DashMap<String, CacheEntry>,
    cache_ttl: Duration,
}

impl PairingGate {
    pub fn new(store: Arc<PairingStore>) -> Self {
        Self {
            store,
            cache: DashMap::new(),
            cache_ttl: DEFAULT_CACHE_TTL,
        }
    }

    pub fn with_cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Drop every cached decision. The next call for any sender will
    /// re-query the store. Cheap (single dashmap clear).
    pub fn flush_cache(&self) {
        self.cache.clear();
    }

    /// Hot path. Returns the decision the plugin should act on. Errors
    /// only on storage failures — those get logged and are treated as
    /// `Drop` by the caller (fail-closed).
    pub async fn should_admit(
        &self,
        channel: &str,
        account_id: &str,
        sender_id: &str,
        policy: &PairingPolicy,
    ) -> Result<Decision, PairingError> {
        // Gate disabled → always admit. Skip the store entirely so a
        // setup with `auto_challenge: false` pays zero overhead.
        if !policy.auto_challenge {
            return Ok(Decision::Admit);
        }

        let key = cache_key(channel, account_id, sender_id);
        if let Some(entry) = self.cache.get(&key) {
            if entry.expires_at > Instant::now() {
                return Ok(entry.decision.clone());
            }
        }

        let decision = if self.store.is_allowed(channel, account_id, sender_id).await? {
            Decision::Admit
        } else {
            match self
                .store
                .upsert_pending(channel, account_id, sender_id, serde_json::Value::Null)
                .await
            {
                Ok(out) => Decision::Challenge { code: out.code },
                Err(PairingError::MaxPending { .. }) => Decision::Drop,
                Err(e) => return Err(e),
            }
        };

        self.cache.insert(
            key,
            CacheEntry {
                decision: decision.clone(),
                expires_at: Instant::now() + self.cache_ttl,
            },
        );
        Ok(decision)
    }
}

fn cache_key(channel: &str, account_id: &str, sender_id: &str) -> String {
    format!("{channel}|{account_id}|{sender_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow() -> PairingPolicy {
        PairingPolicy { auto_challenge: true }
    }
    fn off() -> PairingPolicy {
        PairingPolicy { auto_challenge: false }
    }

    #[tokio::test]
    async fn gate_admits_when_policy_off() {
        let store = Arc::new(PairingStore::open_memory().await.unwrap());
        let gate = PairingGate::new(store);
        let d = gate.should_admit("wa", "p", "+57", &off()).await.unwrap();
        assert!(matches!(d, Decision::Admit));
    }

    #[tokio::test]
    async fn first_unknown_sender_gets_challenge_with_code() {
        let store = Arc::new(PairingStore::open_memory().await.unwrap());
        let gate = PairingGate::new(store);
        let d = gate.should_admit("wa", "p", "+57", &allow()).await.unwrap();
        match d {
            Decision::Challenge { code } => assert_eq!(code.len(), crate::code::LENGTH),
            other => panic!("expected challenge, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approved_sender_admits_after_cache_flush() {
        let store = Arc::new(PairingStore::open_memory().await.unwrap());
        let gate = PairingGate::new(Arc::clone(&store));
        let d1 = gate.should_admit("wa", "p", "+57", &allow()).await.unwrap();
        let code = match d1 {
            Decision::Challenge { code } => code,
            other => panic!("{other:?}"),
        };
        store.approve(&code).await.unwrap();
        gate.flush_cache();
        let d2 = gate.should_admit("wa", "p", "+57", &allow()).await.unwrap();
        assert_eq!(d2, Decision::Admit);
    }

    #[tokio::test]
    async fn cache_returns_same_decision_within_ttl() {
        let store = Arc::new(PairingStore::open_memory().await.unwrap());
        let gate = PairingGate::new(store);
        let d1 = gate.should_admit("wa", "p", "+57", &allow()).await.unwrap();
        let d2 = gate.should_admit("wa", "p", "+57", &allow()).await.unwrap();
        assert_eq!(d1, d2);
    }

    #[tokio::test]
    async fn fourth_unknown_sender_drops_due_to_max_pending() {
        let store = Arc::new(PairingStore::open_memory().await.unwrap());
        let gate = PairingGate::new(store);
        for i in 1..=3 {
            let s = format!("+5710000000{i}");
            let d = gate.should_admit("wa", "p", &s, &allow()).await.unwrap();
            assert!(matches!(d, Decision::Challenge { .. }));
        }
        let d4 = gate.should_admit("wa", "p", "+571000000099", &allow()).await.unwrap();
        assert_eq!(d4, Decision::Drop);
    }
}
