//! Phase 93.8.a-daemon — daemon-side `GenericCredentialStore`
//! implementation that forwards each method to a `plugin.credentials.*`
//! JSON-RPC round-trip with the owning `SubprocessNexoPlugin`.
//!
//! Constructed by `SubprocessNexoPlugin::credential_store()` when
//! the plugin's manifest declares `[plugin.credentials_schema]
//! enabled = true` (Phase 93.8.a-daemon manifest section). Inserted
//! into `bundle.stores_v2[plugin.id]` by the Phase 93.7 init-loop
//! helper `collect_credential_store`.
//!
//! Plugin-side handlers ship in Phase 93.8.a-telegram (the four
//! `PluginAdapter::on_credentials_*` builders introduced in Phase
//! 93.8.a-sdk, commit e59ba0be).

use std::path::PathBuf;
use std::sync::{LazyLock, Weak};
use std::time::Instant;

use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use nexo_auth::generic_store::GenericCredentialStore;
use nexo_auth::store::ValidationReport;
use nexo_auth::{Channel, CredentialError, CredentialHandle};

use crate::agent::nexo_plugin_registry::subprocess::SubprocessNexoPlugin;

/// Wire reply shape for `plugin.credentials.list`. Mirrors
/// `nexo_microapp_sdk::plugin::CredentialsListReply` field-for-field
/// so the daemon decodes the JSON without a cross-crate dep.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CredentialsListReply {
    pub accounts: Vec<String>,
    pub warnings: Vec<String>,
}

/// Cached response from the plugin's `plugin.credentials.list` RPC.
/// 30-second TTL so hot-path `bundle.stores_v2.get("X").list()`
/// reads don't round-trip on every call.
struct CachedList {
    reply: CredentialsListReply,
    fetched_at: Instant,
}

const CACHE_TTL_SECS: u64 = 30;

/// Phase 93.8.a-daemon — process-wide intern table for plugin-id
/// strings. `Channel = &'static str` requires the channel id to
/// outlive any handle issuance; `Box::leak` produces the static
/// reference. Bounded by the operator's manifest size (one entry
/// per distinct plugin id; typically < 100).
static CHANNEL_INTERN: LazyLock<DashMap<String, &'static str>> =
    LazyLock::new(DashMap::new);

fn intern_channel(plugin_id: &str) -> Channel {
    if let Some(s) = CHANNEL_INTERN.get(plugin_id) {
        return *s;
    }
    let leaked: &'static str = Box::leak(plugin_id.to_string().into_boxed_str());
    CHANNEL_INTERN.insert(plugin_id.to_string(), leaked);
    leaked
}

/// Daemon-side `GenericCredentialStore` impl that round-trips each
/// method to the plugin's subprocess via JSON-RPC.
pub struct RemoteCredentialStore {
    plugin_id: String,
    /// Interned `&'static str` for use as `Channel` in
    /// `CredentialHandle::new`. Matches `plugin_id` byte-for-byte.
    channel: Channel,
    /// Weak ref to the owning subprocess; upgrade per call. Failed
    /// upgrades return `CredentialError::InvalidSecret { message:
    /// "subprocess gone" }`.
    weak: Weak<SubprocessNexoPlugin>,
    /// Cached `list()` reply; cleared by `reload()` or on TTL miss.
    cached_list: Mutex<Option<CachedList>>,
}

impl RemoteCredentialStore {
    /// Construct a remote store for `plugin_id` backed by the
    /// subprocess `weak` references.
    pub fn new(plugin_id: String, weak: Weak<SubprocessNexoPlugin>) -> Self {
        let channel = intern_channel(&plugin_id);
        Self {
            plugin_id,
            channel,
            weak,
            cached_list: Mutex::new(None),
        }
    }

    fn synthetic_path(&self) -> PathBuf {
        PathBuf::from(format!("<remote:{}>", self.plugin_id))
    }

    fn map_remote_err(&self, msg: String, agent_id: &str, account_id: &str) -> CredentialError {
        if msg.starts_with("not_permitted") {
            CredentialError::NotPermitted {
                channel: self.channel,
                agent: agent_id.to_string(),
                fp: nexo_auth::Fingerprint::of(account_id),
            }
        } else if msg.starts_with("not_found") {
            CredentialError::NotFound {
                channel: self.channel,
                account: account_id.to_string(),
            }
        } else {
            CredentialError::InvalidSecret {
                path: self.synthetic_path(),
                message: msg,
            }
        }
    }
}

#[async_trait]
impl GenericCredentialStore for RemoteCredentialStore {
    fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    async fn list(&self) -> Vec<String> {
        // Cache hit fast path.
        {
            let guard = self.cached_list.lock().await;
            if let Some(cached) = guard.as_ref() {
                if cached.fetched_at.elapsed().as_secs() < CACHE_TTL_SECS {
                    return cached.reply.accounts.clone();
                }
            }
        }
        let Some(sub) = self.weak.upgrade() else {
            tracing::warn!(
                target: "auth.credentials.list",
                plugin_id = %self.plugin_id,
                "subprocess gone; returning empty account list",
            );
            return Vec::new();
        };
        match sub.send_credentials_list_rpc().await {
            Ok(reply) => {
                for w in &reply.warnings {
                    tracing::warn!(
                        target: "auth.credentials.boot_warning",
                        plugin_id = %self.plugin_id,
                        warning = %w,
                    );
                }
                let accounts = reply.accounts.clone();
                *self.cached_list.lock().await = Some(CachedList {
                    reply,
                    fetched_at: Instant::now(),
                });
                accounts
            }
            Err(e) => {
                tracing::warn!(
                    target: "auth.credentials.list",
                    plugin_id = %self.plugin_id,
                    error = %e,
                    "plugin.credentials.list RPC failed",
                );
                Vec::new()
            }
        }
    }

    async fn issue(
        &self,
        account_id: &str,
        agent_id: &str,
    ) -> Result<CredentialHandle, CredentialError> {
        let Some(sub) = self.weak.upgrade() else {
            return Err(CredentialError::InvalidSecret {
                path: self.synthetic_path(),
                message: "subprocess gone".to_string(),
            });
        };
        match sub.send_credentials_issue_rpc(account_id, agent_id).await {
            Ok(()) => Ok(CredentialHandle::new(self.channel, account_id, agent_id)),
            Err(msg) => Err(self.map_remote_err(msg, agent_id, account_id)),
        }
    }

    async fn resolve_bytes(
        &self,
        handle: &CredentialHandle,
    ) -> Result<Vec<u8>, CredentialError> {
        let Some(sub) = self.weak.upgrade() else {
            return Err(CredentialError::InvalidSecret {
                path: self.synthetic_path(),
                message: "subprocess gone".to_string(),
            });
        };
        let account_id = handle.account_id_raw();
        let agent_id = handle.agent_id();
        let fingerprint = handle.fingerprint().to_string();
        match sub
            .send_credentials_resolve_bytes_rpc(account_id, agent_id, &fingerprint)
            .await
        {
            Ok(b64) => {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|e| CredentialError::InvalidSecret {
                        path: self.synthetic_path(),
                        message: format!("base64 decode: {e}"),
                    })
            }
            Err(msg) => Err(self.map_remote_err(msg, agent_id, account_id)),
        }
    }

    async fn reload(&self) -> Result<(), CredentialError> {
        *self.cached_list.lock().await = None;
        let Some(sub) = self.weak.upgrade() else {
            return Ok(());
        };
        if let Err(e) = sub.send_credentials_reload_rpc().await {
            tracing::warn!(
                target: "auth.credentials.reload",
                plugin_id = %self.plugin_id,
                error = %e,
                "plugin.credentials.reload RPC failed (best-effort)",
            );
        }
        Ok(())
    }

    fn validate(&self) -> ValidationReport {
        // Sync trait method — can't await; surface what's already
        // cached. `try_lock` returns `Err` on contention; degrade
        // to empty report rather than block.
        let Ok(guard) = self.cached_list.try_lock() else {
            return ValidationReport::default();
        };
        let Some(cached) = guard.as_ref() else {
            return ValidationReport::default();
        };
        ValidationReport {
            accounts_ok: cached.reply.accounts.len(),
            warnings: cached.reply.warnings.clone(),
            ..ValidationReport::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_plugin_manifest::PluginManifest;

    fn manifest_with_credentials_schema(enabled: bool) -> PluginManifest {
        let raw = format!(
            "[plugin]\n\
             id = \"test_plugin\"\n\
             version = \"0.1.0\"\n\
             name = \"test\"\n\
             description = \"fixture\"\n\
             min_nexo_version = \">=0.0.1\"\n\
             \n\
             [plugin.credentials_schema]\n\
             enabled = {enabled}\n",
        );
        toml::from_str(&raw).expect("manifest TOML parses")
    }

    fn manifest_without_credentials_schema() -> PluginManifest {
        let raw = "[plugin]\n\
                   id = \"test_plugin\"\n\
                   version = \"0.1.0\"\n\
                   name = \"test\"\n\
                   description = \"fixture\"\n\
                   min_nexo_version = \">=0.0.1\"\n";
        toml::from_str(raw).expect("manifest TOML parses")
    }

    /// Phase 93.8.a-daemon — manifest WITHOUT `[plugin.credentials_schema]`
    /// section ⇒ `credential_store()` returns `None`.
    #[test]
    fn credential_store_returns_none_when_section_absent() {
        use crate::agent::plugin_host::NexoPlugin;
        let plugin = SubprocessNexoPlugin::new(manifest_without_credentials_schema());
        assert!(plugin.credential_store().is_none());
    }

    /// Phase 93.8.a-daemon — manifest declares the section but
    /// `enabled = false` ⇒ `credential_store()` returns `None`.
    #[test]
    fn credential_store_returns_none_when_opted_out() {
        use crate::agent::plugin_host::NexoPlugin;
        let plugin = SubprocessNexoPlugin::new(manifest_with_credentials_schema(false));
        assert!(plugin.credential_store().is_none());
    }

    /// Phase 93.8.a-daemon — manifest declares `enabled = true` but
    /// `weak_self` is unpopulated (plugin built outside the
    /// factory) ⇒ `credential_store()` still returns `None`
    /// (defensive: avoid emitting a `RemoteCredentialStore` that
    /// can't reach the subprocess).
    #[test]
    fn credential_store_returns_none_when_weak_self_unpopulated() {
        use crate::agent::plugin_host::NexoPlugin;
        let plugin = SubprocessNexoPlugin::new(manifest_with_credentials_schema(true));
        // `SubprocessNexoPlugin::new` doesn't populate `weak_self`;
        // only the factory does. So `credential_store()` short-
        // circuits on the `weak_self.get()?` even though the
        // manifest opted in.
        assert!(plugin.credential_store().is_none());
    }

    /// Phase 93.8.a-daemon — `intern_channel` returns the SAME
    /// `&'static str` for repeated calls with the same plugin id
    /// (cached via `DashMap`). Different ids produce different
    /// pointers.
    #[test]
    fn intern_channel_returns_stable_static_str() {
        let a = intern_channel("phase_93_8a_test_alpha");
        let b = intern_channel("phase_93_8a_test_alpha");
        let c = intern_channel("phase_93_8a_test_beta");
        // Same id → same pointer.
        assert_eq!(a.as_ptr(), b.as_ptr());
        // Different id → different pointer.
        assert_ne!(a.as_ptr(), c.as_ptr());
        // Returned strings match the input.
        assert_eq!(a, "phase_93_8a_test_alpha");
        assert_eq!(c, "phase_93_8a_test_beta");
    }

    /// Phase 93.8.a-daemon — `validate()` returns the default
    /// (empty) `ValidationReport` when no `list()` has populated
    /// the cache yet. Sync trait method exercise without spawning
    /// a subprocess.
    #[tokio::test]
    async fn validate_returns_default_when_cache_empty() {
        // Construct a `RemoteCredentialStore` with a dangling weak
        // ref — `validate()` should still return the default
        // report (the weak ref isn't touched by this trait method).
        let weak: std::sync::Weak<SubprocessNexoPlugin> = std::sync::Weak::new();
        let store = RemoteCredentialStore::new("test_plugin".to_string(), weak);
        let report = store.validate();
        assert_eq!(report.accounts_ok, 0);
        assert!(report.warnings.is_empty());
        assert!(report.errors.is_empty());
    }

    /// Phase 93.8.a-daemon — `list()` with a dangling weak ref
    /// returns an empty `Vec` rather than panicking. Exercises the
    /// "subprocess gone" branch without spawning a child.
    #[tokio::test]
    async fn list_returns_empty_when_subprocess_gone() {
        let weak: std::sync::Weak<SubprocessNexoPlugin> = std::sync::Weak::new();
        let store = RemoteCredentialStore::new("test_plugin".to_string(), weak);
        assert!(store.list().await.is_empty());
    }

    /// Phase 93.8.a-daemon — `issue()` with a dangling weak ref
    /// returns `CredentialError::InvalidSecret { message: contains
    /// "subprocess gone" }`.
    #[tokio::test]
    async fn issue_errors_when_subprocess_gone() {
        let weak: std::sync::Weak<SubprocessNexoPlugin> = std::sync::Weak::new();
        let store = RemoteCredentialStore::new("test_plugin".to_string(), weak);
        let err = store
            .issue("main", "alice")
            .await
            .expect_err("expected error");
        match err {
            CredentialError::InvalidSecret { message, .. } => {
                assert!(message.contains("subprocess gone"));
            }
            other => panic!("expected InvalidSecret subprocess gone, got {other:?}"),
        }
    }
}

