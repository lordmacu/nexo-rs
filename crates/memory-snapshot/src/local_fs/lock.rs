//! Per-agent snapshot exclusion.
//!
//! Snapshot and restore must not race against each other for the same
//! agent: a half-applied restore behind a fresh snapshot would leave
//! the bundle integrity-checked but semantically wrong. We use a
//! `tokio::sync::Mutex` keyed in a `DashMap` so the lock state per
//! agent is bounded and lazy.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::error::SnapshotError;
use crate::id::AgentId;

#[derive(Default)]
pub struct AgentLockMap {
    locks: DashMap<AgentId, Arc<Mutex<()>>>,
}

impl AgentLockMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire the per-agent lock or fail with
    /// [`SnapshotError::Concurrent`] after `timeout`. The returned
    /// guard releases on drop.
    pub async fn acquire(
        &self,
        agent_id: &AgentId,
        timeout: Duration,
    ) -> Result<AgentLockGuard, SnapshotError> {
        let mtx = self
            .locks
            .entry(agent_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();

        match tokio::time::timeout(timeout, mtx.lock_owned()).await {
            Ok(guard) => Ok(AgentLockGuard {
                _guard: guard,
                _agent_id: agent_id.clone(),
            }),
            Err(_elapsed) => Err(SnapshotError::Concurrent(agent_id.clone())),
        }
    }
}

/// RAII handle. Holding it proves no other snapshot/restore is running
/// for the named agent.
pub struct AgentLockGuard {
    _guard: OwnedMutexGuard<()>,
    _agent_id: AgentId,
}

impl std::fmt::Debug for AgentLockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLockGuard")
            .field("agent_id", &self._agent_id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn second_acquire_times_out_with_concurrent_error() {
        let map = Arc::new(AgentLockMap::new());
        let id = "ana".to_string();
        let _g1 = map.acquire(&id, Duration::from_millis(50)).await.unwrap();

        let map2 = map.clone();
        let id2 = id.clone();
        let err = map2
            .acquire(&id2, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(err, SnapshotError::Concurrent(ref a) if a == &id));
    }

    #[tokio::test]
    async fn drop_releases_lock_for_subsequent_acquire() {
        let map = AgentLockMap::new();
        let id = "ana".to_string();
        {
            let _g = map.acquire(&id, Duration::from_millis(50)).await.unwrap();
        }
        // Lock dropped — fresh acquire should succeed within timeout.
        let _g2 = map.acquire(&id, Duration::from_millis(50)).await.unwrap();
    }

    #[tokio::test]
    async fn distinct_agents_do_not_block_each_other() {
        let map = Arc::new(AgentLockMap::new());
        let _g1 = map
            .acquire(&"ana".into(), Duration::from_millis(50))
            .await
            .unwrap();
        let _g2 = map
            .acquire(&"otro".into(), Duration::from_millis(50))
            .await
            .unwrap();
    }
}
