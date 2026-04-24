//! [`CredentialStore`] impl for Google OAuth accounts. One account per
//! agent (`agent_id` is 1:1 for V1). Holds paths to the three files
//! the gmail-poller already uses (`client_id_path`, `client_secret_path`,
//! `token_path`), plus scopes.
//!
//! Token refresh is serialised per-account with a `tokio::Mutex` so
//! multiple jobs reading the same token file do not race and trigger
//! Google's concurrent-refresh 400.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;

use crate::error::CredentialError;
use crate::handle::{Channel, CredentialHandle, Fingerprint, GOOGLE};
use crate::store::{CredentialStore, ValidationReport};

#[derive(Debug, Clone)]
pub struct GoogleAccount {
    pub id: String,
    pub agent_id: String,
    pub client_id_path: PathBuf,
    pub client_secret_path: PathBuf,
    pub token_path: PathBuf,
    pub scopes: Vec<String>,
}

pub struct GoogleCredentialStore {
    accounts: Arc<HashMap<String, GoogleAccount>>,
    /// Per-fingerprint serialisation for token refresh. Lazily created
    /// the first time a refresh is requested for an account.
    refresh_locks: DashMap<Fingerprint, Arc<tokio::sync::Mutex<()>>>,
}

impl GoogleCredentialStore {
    pub fn new(accounts: Vec<GoogleAccount>) -> Self {
        let mut map = HashMap::with_capacity(accounts.len());
        for a in accounts {
            map.insert(a.id.clone(), a);
        }
        Self {
            accounts: Arc::new(map),
            refresh_locks: DashMap::new(),
        }
    }

    pub fn empty() -> Self {
        Self {
            accounts: Arc::new(HashMap::new()),
            refresh_locks: DashMap::new(),
        }
    }

    pub fn account(&self, id: &str) -> Option<&GoogleAccount> {
        self.accounts.get(id)
    }

    pub fn account_for_agent(&self, agent_id: &str) -> Option<&GoogleAccount> {
        self.accounts.values().find(|a| a.agent_id == agent_id)
    }

    /// Acquire the refresh mutex for the account behind `handle`. The
    /// lock lives for the lifetime of the returned guard; callers
    /// should hold it across the full HTTP roundtrip that rotates the
    /// refresh_token on the disk. `None` when the handle points at an
    /// unknown account (treat as NotFound).
    pub fn refresh_lock(
        &self,
        handle: &CredentialHandle,
    ) -> Option<Arc<tokio::sync::Mutex<()>>> {
        let fp = handle.fingerprint();
        let entry = self
            .refresh_locks
            .entry(fp)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())));
        Some(entry.clone())
    }
}

impl std::fmt::Debug for GoogleCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleCredentialStore")
            .field("account_count", &self.accounts.len())
            .field("active_refresh_locks", &self.refresh_locks.len())
            .finish()
    }
}

impl CredentialStore for GoogleCredentialStore {
    type Account = GoogleAccount;

    fn channel(&self) -> Channel {
        GOOGLE
    }

    fn get(&self, handle: &CredentialHandle) -> Result<Self::Account, CredentialError> {
        let id = handle.account_id_raw();
        self.accounts
            .get(id)
            .cloned()
            .ok_or_else(|| CredentialError::NotFound {
                channel: GOOGLE,
                account: id.to_string(),
            })
    }

    fn issue(
        &self,
        account_id: &str,
        agent_id: &str,
    ) -> Result<CredentialHandle, CredentialError> {
        let account = self
            .accounts
            .get(account_id)
            .ok_or_else(|| CredentialError::NotFound {
                channel: GOOGLE,
                account: account_id.to_string(),
            })?;
        // Google accounts are 1:1 — the declared agent_id must match.
        if account.agent_id != agent_id {
            let handle = CredentialHandle::new(GOOGLE, account_id, agent_id);
            return Err(CredentialError::NotPermitted {
                channel: GOOGLE,
                agent: agent_id.to_string(),
                fp: handle.fingerprint(),
            });
        }
        Ok(CredentialHandle::new(GOOGLE, account_id, agent_id))
    }

    fn list(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.accounts.keys().cloned().collect();
        ids.sort();
        ids
    }

    fn allow_agents(&self, account_id: &str) -> Vec<String> {
        self.accounts
            .get(account_id)
            .map(|a| vec![a.agent_id.clone()])
            .unwrap_or_default()
    }

    fn validate(&self) -> ValidationReport {
        let mut report = ValidationReport::default();
        for (id, a) in self.accounts.iter() {
            if a.scopes.is_empty() {
                report
                    .warnings
                    .push(format!("google account '{id}' has no scopes declared"));
            }
            if !a.client_id_path.exists() {
                report.errors.push(crate::error::BuildError::Credential {
                    channel: GOOGLE,
                    instance: id.clone(),
                    source: CredentialError::FileMissing {
                        path: a.client_id_path.clone(),
                    },
                });
            }
            if !a.client_secret_path.exists() {
                report.errors.push(crate::error::BuildError::Credential {
                    channel: GOOGLE,
                    instance: id.clone(),
                    source: CredentialError::FileMissing {
                        path: a.client_secret_path.clone(),
                    },
                });
            }
            // token_path is allowed to be absent — the setup wizard
            // writes it on first consent.
            report.accounts_ok += 1;
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(id: &str, agent: &str) -> GoogleAccount {
        GoogleAccount {
            id: id.into(),
            agent_id: agent.into(),
            client_id_path: PathBuf::from("/nonexistent/cid"),
            client_secret_path: PathBuf::from("/nonexistent/csec"),
            token_path: PathBuf::from("/nonexistent/tok"),
            scopes: vec!["https://www.googleapis.com/auth/gmail.readonly".into()],
        }
    }

    #[test]
    fn issue_rejects_mismatched_agent() {
        let store = GoogleCredentialStore::new(vec![mk("ana@x.com", "ana")]);
        assert!(store.issue("ana@x.com", "ana").is_ok());
        let err = store.issue("ana@x.com", "kate").unwrap_err();
        assert!(matches!(err, CredentialError::NotPermitted { .. }));
    }

    #[test]
    fn account_for_agent_lookup() {
        let store = GoogleCredentialStore::new(vec![
            mk("ana@x.com", "ana"),
            mk("kate@x.com", "kate"),
        ]);
        assert_eq!(store.account_for_agent("ana").unwrap().id, "ana@x.com");
        assert_eq!(store.account_for_agent("kate").unwrap().id, "kate@x.com");
        assert!(store.account_for_agent("nobody").is_none());
    }

    #[tokio::test]
    async fn refresh_lock_serialises_same_account() {
        let store = GoogleCredentialStore::new(vec![mk("ana@x.com", "ana")]);
        let h = store.issue("ana@x.com", "ana").unwrap();
        let l1 = store.refresh_lock(&h).unwrap();
        let l2 = store.refresh_lock(&h).unwrap();
        // Same Arc instance — both handles map to the same mutex.
        assert!(Arc::ptr_eq(&l1, &l2));
        let _guard = l1.lock().await;
        // Second lock attempt should time out while the first guard is held.
        let try_second =
            tokio::time::timeout(std::time::Duration::from_millis(50), l2.lock()).await;
        assert!(try_second.is_err(), "second lock should block");
    }

    #[tokio::test]
    async fn refresh_lock_distinct_for_different_accounts() {
        let store = GoogleCredentialStore::new(vec![
            mk("a@x.com", "ana"),
            mk("k@x.com", "kate"),
        ]);
        let ha = store.issue("a@x.com", "ana").unwrap();
        let hk = store.issue("k@x.com", "kate").unwrap();
        let la = store.refresh_lock(&ha).unwrap();
        let lk = store.refresh_lock(&hk).unwrap();
        assert!(!Arc::ptr_eq(&la, &lk));
    }

    #[test]
    fn validate_flags_missing_files() {
        let store = GoogleCredentialStore::new(vec![mk("ana@x.com", "ana")]);
        let report = store.validate();
        assert!(report.errors.len() >= 2, "missing files should surface");
    }
}
