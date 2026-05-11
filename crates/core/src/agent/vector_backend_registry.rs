//! `VectorBackendRegistry` per-name registry of
//! `VectorBackend` implementations.
//!
//! Subprocess plugins declaring
//! `[plugin.extends].memory_backends = [...]` register
//! `RemoteVectorBackend` instances here at boot. v1 ships the
//! registry + wire surface only — operator-side consumer wiring
//! (`LongTermMemory.recall_vector` reading from this registry)
//! lands in 81.26.b.
//!
//! Conflict semantic: **first-registers-wins-rest-rejected**
//! (mirrors `ChannelAdapterRegistry` from 81.8). Two plugins
//! claiming the same backend name would cause routing ambiguity,
//! so the second registration returns
//! [`VectorBackendRegistrationError::NameAlreadyRegistered`].
//! `unregister(name, plugin_id)` is ownership-checked: only the
//! plugin that registered a backend can remove it.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use thiserror::Error;

use nexo_memory::VectorBackend;

#[derive(Default)]
pub struct VectorBackendRegistry {
    inner: RwLock<BTreeMap<String, BackendEntry>>,
}

impl std::fmt::Debug for VectorBackendRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        f.debug_struct("VectorBackendRegistry")
            .field("names", &guard.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Clone)]
struct BackendEntry {
    backend: Arc<dyn VectorBackend>,
    registered_by: String,
}

impl VectorBackendRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &self,
        backend: Arc<dyn VectorBackend>,
        registered_by: impl Into<String>,
    ) -> Result<(), VectorBackendRegistrationError> {
        let name = backend.name().to_string();
        let registered_by = registered_by.into();
        let mut guard = self.inner.write().unwrap_or_else(|p| p.into_inner());
        match guard.get(&name) {
            Some(prior) => Err(VectorBackendRegistrationError::NameAlreadyRegistered {
                name,
                prior_registered_by: prior.registered_by.clone(),
                attempted_by: registered_by,
            }),
            None => {
                guard.insert(
                    name,
                    BackendEntry {
                        backend,
                        registered_by,
                    },
                );
                Ok(())
            }
        }
    }

    /// Remove a backend only when owned by `plugin_id`.
    /// Defends against one plugin unregistering another's
    /// backend. Returns `true` when removed.
    pub fn unregister(&self, name: &str, plugin_id: &str) -> bool {
        let mut guard = self.inner.write().unwrap_or_else(|p| p.into_inner());
        match guard.get(name) {
            Some(entry) if entry.registered_by == plugin_id => {
                guard.remove(name);
                true
            }
            _ => false,
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn VectorBackend>> {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        guard.get(name).map(|e| e.backend.clone())
    }

    pub fn names(&self) -> Vec<String> {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        guard.keys().cloned().collect()
    }

    pub fn has_any(&self) -> bool {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        !guard.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum VectorBackendRegistrationError {
    #[error(
        "vector backend `{name}` already registered by plugin `{prior_registered_by}` (attempted by `{attempted_by}`)"
    )]
    NameAlreadyRegistered {
        name: String,
        prior_registered_by: String,
        attempted_by: String,
    },
    #[error("subprocess plugin inner not initialized — call register_remote_vector_backends AFTER init()")]
    InnerUnavailable,
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use nexo_memory::{DeleteAck, UpsertAck, VectorMatch, VectorQuery, VectorRecord};

    struct DummyBackend {
        name: &'static str,
    }

    impl DummyBackend {
        fn new(name: &'static str) -> Arc<Self> {
            Arc::new(Self { name })
        }
    }

    #[async_trait]
    impl VectorBackend for DummyBackend {
        fn name(&self) -> &str {
            self.name
        }
        async fn upsert(
            &self,
            _collection: &str,
            _records: Vec<VectorRecord>,
        ) -> anyhow::Result<UpsertAck> {
            Ok(UpsertAck::default())
        }
        async fn search(
            &self,
            _collection: &str,
            _query: VectorQuery,
        ) -> anyhow::Result<Vec<VectorMatch>> {
            Ok(Vec::new())
        }
        async fn delete(&self, _collection: &str, _ids: Vec<String>) -> anyhow::Result<DeleteAck> {
            Ok(DeleteAck::default())
        }
    }

    #[test]
    fn register_first_succeeds() {
        let reg = VectorBackendRegistry::new();
        assert!(!reg.has_any());
        let r = reg.register(DummyBackend::new("pinecone"), "plugin_a");
        assert!(r.is_ok());
        assert!(reg.has_any());
        assert_eq!(reg.names(), vec!["pinecone".to_string()]);
        assert!(reg.get("pinecone").is_some());
    }

    #[test]
    fn register_duplicate_name_rejected() {
        let reg = VectorBackendRegistry::new();
        reg.register(DummyBackend::new("pinecone"), "plugin_a")
            .unwrap();
        let err = reg
            .register(DummyBackend::new("pinecone"), "plugin_b")
            .expect_err("duplicate must fail");
        match err {
            VectorBackendRegistrationError::NameAlreadyRegistered {
                name,
                prior_registered_by,
                attempted_by,
            } => {
                assert_eq!(name, "pinecone");
                assert_eq!(prior_registered_by, "plugin_a");
                assert_eq!(attempted_by, "plugin_b");
            }
            other => panic!("expected NameAlreadyRegistered, got {other:?}"),
        }
        // Original registration intact.
        assert_eq!(reg.names(), vec!["pinecone".to_string()]);
    }

    #[test]
    fn unregister_only_removes_when_owner_matches() {
        let reg = VectorBackendRegistry::new();
        reg.register(DummyBackend::new("pinecone"), "plugin_a")
            .unwrap();
        // Mismatched plugin_id leaves the entry intact.
        assert!(!reg.unregister("pinecone", "plugin_evil"));
        assert!(reg.get("pinecone").is_some());
        // Matched plugin_id removes it.
        assert!(reg.unregister("pinecone", "plugin_a"));
        assert!(reg.get("pinecone").is_none());
        // Unregister of absent name returns false (idempotent).
        assert!(!reg.unregister("pinecone", "plugin_a"));
    }

    #[test]
    fn names_returns_sorted_list() {
        let reg = VectorBackendRegistry::new();
        reg.register(DummyBackend::new("zeta"), "p1").unwrap();
        reg.register(DummyBackend::new("alpha"), "p2").unwrap();
        reg.register(DummyBackend::new("mike"), "p3").unwrap();
        assert_eq!(
            reg.names(),
            vec!["alpha".to_string(), "mike".to_string(), "zeta".to_string()]
        );
    }
}
