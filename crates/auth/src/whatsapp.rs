//! [`CredentialStore`] impl for WhatsApp session-based accounts.
//!
//! Each account owns a `session_dir` (Signal keystore + pairing state)
//! and a `media_dir`. The store does not touch the filesystem itself —
//! the WA plugin owns I/O. This crate only validates layout, records
//! the claim, and issues opaque handles the plugin can look up by
//! [`CredentialHandle::account_id_raw`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::CredentialError;
use crate::handle::{Channel, CredentialHandle, WHATSAPP};
use crate::store::{CredentialStore, ValidationReport};

#[derive(Debug, Clone)]
pub struct WhatsappAccount {
    pub instance: String,
    pub session_dir: PathBuf,
    pub media_dir: PathBuf,
    pub allow_agents: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct WhatsappCredentialStore {
    accounts: Arc<HashMap<String, WhatsappAccount>>,
}

impl WhatsappCredentialStore {
    pub fn new(accounts: Vec<WhatsappAccount>) -> Self {
        let mut map = HashMap::with_capacity(accounts.len());
        for a in accounts {
            map.insert(a.instance.clone(), a);
        }
        Self {
            accounts: Arc::new(map),
        }
    }

    pub fn empty() -> Self {
        Self {
            accounts: Arc::new(HashMap::new()),
        }
    }

    pub fn account(&self, instance: &str) -> Option<&WhatsappAccount> {
        self.accounts.get(instance)
    }
}

impl CredentialStore for WhatsappCredentialStore {
    type Account = WhatsappAccount;

    fn channel(&self) -> Channel {
        WHATSAPP
    }

    fn get(&self, handle: &CredentialHandle) -> Result<Self::Account, CredentialError> {
        let id = handle.account_id_raw();
        self.accounts
            .get(id)
            .cloned()
            .ok_or_else(|| CredentialError::NotFound {
                channel: WHATSAPP,
                account: id.to_string(),
            })
    }

    fn issue(&self, account_id: &str, agent_id: &str) -> Result<CredentialHandle, CredentialError> {
        let account = self
            .accounts
            .get(account_id)
            .ok_or_else(|| CredentialError::NotFound {
                channel: WHATSAPP,
                account: account_id.to_string(),
            })?;
        if !account.allow_agents.is_empty() && !account.allow_agents.iter().any(|a| a == agent_id) {
            let handle = CredentialHandle::new(WHATSAPP, account_id, agent_id);
            return Err(CredentialError::NotPermitted {
                channel: WHATSAPP,
                agent: agent_id.to_string(),
                fp: handle.fingerprint(),
            });
        }
        Ok(CredentialHandle::new(WHATSAPP, account_id, agent_id))
    }

    fn list(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.accounts.keys().cloned().collect();
        ids.sort();
        ids
    }

    fn allow_agents(&self, account_id: &str) -> Vec<String> {
        self.accounts
            .get(account_id)
            .map(|a| a.allow_agents.clone())
            .unwrap_or_default()
    }

    fn validate(&self) -> ValidationReport {
        ValidationReport {
            accounts_ok: self.accounts.len(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(instance: &str, allow: &[&str]) -> WhatsappAccount {
        WhatsappAccount {
            instance: instance.into(),
            session_dir: PathBuf::from(format!("/tmp/wa-{instance}")),
            media_dir: PathBuf::from(format!("/tmp/wa-{instance}/media")),
            allow_agents: allow.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn issue_returns_handle_when_permitted() {
        let store = WhatsappCredentialStore::new(vec![mk("personal", &["ana"])]);
        let h = store.issue("personal", "ana").unwrap();
        assert_eq!(h.channel(), WHATSAPP);
        assert_eq!(h.agent_id(), "ana");
    }

    #[test]
    fn issue_rejects_non_allowed_agent() {
        let store = WhatsappCredentialStore::new(vec![mk("personal", &["ana"])]);
        let err = store.issue("personal", "kate").unwrap_err();
        assert!(matches!(err, CredentialError::NotPermitted { .. }));
    }

    #[test]
    fn empty_allow_list_accepts_anyone() {
        let store = WhatsappCredentialStore::new(vec![mk("personal", &[])]);
        assert!(store.issue("personal", "kate").is_ok());
        assert!(store.issue("personal", "ana").is_ok());
    }

    #[test]
    fn issue_missing_instance_errors() {
        let store = WhatsappCredentialStore::empty();
        let err = store.issue("nope", "ana").unwrap_err();
        assert!(matches!(err, CredentialError::NotFound { .. }));
    }

    #[test]
    fn list_is_sorted_and_stable() {
        let store = WhatsappCredentialStore::new(vec![mk("b", &[]), mk("a", &[]), mk("c", &[])]);
        assert_eq!(store.list(), vec!["a", "b", "c"]);
    }
}
