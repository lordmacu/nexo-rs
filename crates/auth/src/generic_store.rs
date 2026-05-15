//! Phase 93.6 — dyn-safe credential store keyed by plugin id.
//!
//! Inverts the typed [`crate::store::CredentialStore`]'s
//! `Account` associated type into an opaque `Vec<u8>` boundary so
//! the daemon can hold `Arc<dyn GenericCredentialStore>` without
//! compile-time knowledge of which plugin's bytes it's handling.
//! Plugins own their typed `from_bytes` interpretation; the daemon
//! never deserialises the payload.
//!
//! Backward compat (Phase 93.6 → 93.9): wrap existing typed stores
//! in [`TypedStoreAdapter`] so they conform without source edits.
//! Phase 93.7 introduces the
//! `DashMap<String, Arc<dyn GenericCredentialStore>>` parallel
//! field on `CredentialsBundle`; Phase 93.8 lets each plugin return
//! its own `Arc<dyn GenericCredentialStore>` from the new
//! `NexoPlugin::credential_store()` trait method; Phase 93.9 drops
//! the typed `CredentialStores` struct + the per-handle
//! `nexo_auth::handle::*` const strings.
//!
//! Nothing in nexo-rs consumes the trait yet — 93.6 only ships the
//! contract + adapter + tests so 93.7 has a tested API surface.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::CredentialError;
use crate::handle::CredentialHandle;
use crate::store::{CredentialStore, ValidationReport};

/// Phase 93.6 — dyn-safe credential store keyed by plugin id.
///
/// See module docs for the staging plan (93.6 → 93.9) and the
/// backward-compat strategy via [`TypedStoreAdapter`].
#[async_trait]
pub trait GenericCredentialStore: Send + Sync + 'static {
    /// Stable plugin id (matches `manifest.plugin.id`). Used as the
    /// `DashMap` key in Phase 93.7.
    fn plugin_id(&self) -> &str;

    /// Enumerate every account id known to this store. Async to
    /// permit future remote-backed stores (KMS, vault) without
    /// blocking the caller; in-memory implementations resolve in
    /// O(1).
    async fn list(&self) -> Vec<String>;

    /// Issue an opaque handle after checking `agent_id` against the
    /// account's allowlist. Returns
    /// [`CredentialError::NotPermitted`] when denied,
    /// [`CredentialError::NotFound`] when `account_id` is unknown.
    async fn issue(
        &self,
        account_id: &str,
        agent_id: &str,
    ) -> Result<CredentialHandle, CredentialError>;

    /// Resolve the raw credential bytes for `handle`. Plugin owns
    /// interpretation — daemon never deserialises this payload. The
    /// plugin's runtime code calls `serde_json::from_slice` (or
    /// equivalent) on the returned `Vec<u8>`.
    async fn resolve_bytes(
        &self,
        handle: &CredentialHandle,
    ) -> Result<Vec<u8>, CredentialError>;

    /// Hot-reload — re-read from disk / env / external KMS. Default
    /// no-op for stores that don't support live updates. Returning
    /// `Err(_)` does NOT abort the daemon; the reload coordinator
    /// logs and continues with the previous in-memory state.
    async fn reload(&self) -> Result<(), CredentialError> {
        Ok(())
    }

    /// Boot-time invariant check. Errors collected across stores
    /// fail boot once aggregated (Phase 93.7 ships the aggregator).
    /// Default report has zero warnings + zero errors.
    fn validate(&self) -> ValidationReport {
        ValidationReport::default()
    }
}

/// Phase 93.6 — wraps any existing typed [`CredentialStore`] so it
/// conforms to [`GenericCredentialStore`].
///
/// `resolve_bytes` serialises the typed `Account` via
/// `serde_json::to_vec`; plugins consuming through the generic path
/// call `serde_json::from_slice::<MyAccount>` on the returned
/// bytes.
///
/// The `S::Account: serde::Serialize` bound is checked at adapter
/// construction. Typed stores whose accounts aren't serializable
/// can't be wrapped automatically and need plugin-side migration
/// (Phase 93.8).
///
/// `reload()` falls through to the trait default (`Ok(())`) — the
/// existing typed [`CredentialStore`] trait has no reload hook, so
/// the adapter can't forward anywhere useful. Plugin-side migration
/// in Phase 93.8 introduces real reload support.
pub struct TypedStoreAdapter<S: CredentialStore + 'static>
where
    S::Account: serde::Serialize,
{
    plugin_id: String,
    inner: Arc<S>,
}

impl<S: CredentialStore + 'static> TypedStoreAdapter<S>
where
    S::Account: serde::Serialize,
{
    /// Wrap `inner` under `plugin_id`. The id is the stable lookup
    /// key Phase 93.7's `DashMap<String, Arc<dyn GenericCredentialStore>>`
    /// will use; it must match `manifest.plugin.id` for the plugin
    /// whose credentials this store holds.
    pub fn new(plugin_id: impl Into<String>, inner: Arc<S>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            inner,
        }
    }
}

#[async_trait]
impl<S: CredentialStore + 'static> GenericCredentialStore for TypedStoreAdapter<S>
where
    S::Account: serde::Serialize,
{
    fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    async fn list(&self) -> Vec<String> {
        self.inner.list()
    }

    async fn issue(
        &self,
        account_id: &str,
        agent_id: &str,
    ) -> Result<CredentialHandle, CredentialError> {
        self.inner.issue(account_id, agent_id)
    }

    async fn resolve_bytes(
        &self,
        handle: &CredentialHandle,
    ) -> Result<Vec<u8>, CredentialError> {
        let account = self.inner.get(handle)?;
        serde_json::to_vec(&account).map_err(|e| CredentialError::InvalidSecret {
            path: PathBuf::from(format!("<typed-store-adapter:{}>", self.plugin_id)),
            message: format!("typed-store serialise failed: {e}"),
        })
    }

    fn validate(&self) -> ValidationReport {
        self.inner.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::{Channel, TELEGRAM};
    use serde::Serialize;

    /// Minimal `GenericCredentialStore` impl for trait-shape tests.
    struct MockGenericStore {
        id: String,
        accounts: Vec<String>,
        resolved: Vec<u8>,
    }

    #[async_trait]
    impl GenericCredentialStore for MockGenericStore {
        fn plugin_id(&self) -> &str {
            &self.id
        }
        async fn list(&self) -> Vec<String> {
            self.accounts.clone()
        }
        async fn issue(
            &self,
            account_id: &str,
            agent_id: &str,
        ) -> Result<CredentialHandle, CredentialError> {
            Ok(CredentialHandle::new(TELEGRAM, account_id, agent_id))
        }
        async fn resolve_bytes(
            &self,
            _handle: &CredentialHandle,
        ) -> Result<Vec<u8>, CredentialError> {
            Ok(self.resolved.clone())
        }
        // `reload` + `validate` use trait defaults.
    }

    #[tokio::test]
    async fn mock_generic_store_lists_accounts() {
        let store = MockGenericStore {
            id: "mock".into(),
            accounts: vec!["alpha".into(), "beta".into()],
            resolved: vec![],
        };
        assert_eq!(
            store.list().await,
            vec!["alpha".to_string(), "beta".to_string()],
        );
    }

    #[tokio::test]
    async fn mock_generic_store_resolves_bytes() {
        let store = MockGenericStore {
            id: "mock".into(),
            accounts: vec![],
            resolved: b"resolved".to_vec(),
        };
        let handle = CredentialHandle::new(TELEGRAM, "acc", "agent");
        assert_eq!(
            store.resolve_bytes(&handle).await.unwrap(),
            b"resolved".to_vec(),
        );
    }

    /// Fake typed store + serializable account, used to exercise
    /// `TypedStoreAdapter`'s serialise path.
    #[derive(Clone, Serialize)]
    struct FakeAcc {
        id: String,
    }

    struct FakeTypedStore {
        acc: FakeAcc,
    }

    impl CredentialStore for FakeTypedStore {
        type Account = FakeAcc;

        fn channel(&self) -> Channel {
            TELEGRAM
        }

        fn get(&self, _handle: &CredentialHandle) -> Result<Self::Account, CredentialError> {
            Ok(self.acc.clone())
        }

        fn issue(
            &self,
            account_id: &str,
            agent_id: &str,
        ) -> Result<CredentialHandle, CredentialError> {
            Ok(CredentialHandle::new(TELEGRAM, account_id, agent_id))
        }

        fn list(&self) -> Vec<String> {
            vec![self.acc.id.clone()]
        }

        fn allow_agents(&self, _account_id: &str) -> Vec<String> {
            Vec::new()
        }

        fn validate(&self) -> ValidationReport {
            ValidationReport::default()
        }
    }

    #[tokio::test]
    async fn typed_adapter_serialises_account_via_serde_json() {
        let typed = Arc::new(FakeTypedStore {
            acc: FakeAcc {
                id: "acc1".into(),
            },
        });
        let adapter = TypedStoreAdapter::new("fake", typed);
        let handle = CredentialHandle::new(TELEGRAM, "acc1", "agent_x");
        let bytes = adapter.resolve_bytes(&handle).await.unwrap();
        let expected = serde_json::to_vec(&FakeAcc {
            id: "acc1".into(),
        })
        .unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(adapter.plugin_id(), "fake");
    }

    #[tokio::test]
    async fn default_reload_is_ok() {
        let store = MockGenericStore {
            id: "mock".into(),
            accounts: vec![],
            resolved: vec![],
        };
        // No override in `MockGenericStore`; falls through to the
        // trait default.
        assert!(store.reload().await.is_ok());
    }
}
