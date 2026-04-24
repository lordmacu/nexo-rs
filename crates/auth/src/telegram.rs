//! [`CredentialStore`] impl for Telegram bots. One instance = one bot
//! token. The token material is held in-memory as a `String` so the
//! plugin can attach it to every HTTP call without hitting the
//! filesystem per request; the gauntlet is responsible for enforcing
//! 0o600 on the source file before the token ever reaches this store.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::CredentialError;
use crate::handle::{Channel, CredentialHandle, TELEGRAM};
use crate::store::{CredentialStore, ValidationReport};

#[derive(Clone)]
pub struct TelegramAccount {
    pub instance: String,
    pub token: String,
    pub allow_agents: Vec<String>,
    pub allowed_chat_ids: Vec<i64>,
}

impl std::fmt::Debug for TelegramAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print token even under `{:?}`.
        f.debug_struct("TelegramAccount")
            .field("instance", &self.instance)
            .field("allow_agents", &self.allow_agents)
            .field("allowed_chat_ids", &self.allowed_chat_ids)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct TelegramCredentialStore {
    accounts: Arc<HashMap<String, TelegramAccount>>,
}

impl TelegramCredentialStore {
    pub fn new(accounts: Vec<TelegramAccount>) -> Self {
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

    pub fn account(&self, instance: &str) -> Option<&TelegramAccount> {
        self.accounts.get(instance)
    }
}

impl CredentialStore for TelegramCredentialStore {
    type Account = TelegramAccount;

    fn channel(&self) -> Channel {
        TELEGRAM
    }

    fn get(&self, handle: &CredentialHandle) -> Result<Self::Account, CredentialError> {
        let id = handle.account_id_raw();
        self.accounts
            .get(id)
            .cloned()
            .ok_or_else(|| CredentialError::NotFound {
                channel: TELEGRAM,
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
                channel: TELEGRAM,
                account: account_id.to_string(),
            })?;
        if !account.allow_agents.is_empty()
            && !account.allow_agents.iter().any(|a| a == agent_id)
        {
            let handle = CredentialHandle::new(TELEGRAM, account_id, agent_id);
            return Err(CredentialError::NotPermitted {
                channel: TELEGRAM,
                agent: agent_id.to_string(),
                fp: handle.fingerprint(),
            });
        }
        Ok(CredentialHandle::new(TELEGRAM, account_id, agent_id))
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
        let mut report = ValidationReport::default();
        for (id, a) in self.accounts.iter() {
            if a.token.trim().is_empty() {
                report.warnings.push(format!(
                    "telegram instance '{id}' has an empty token; bot will 401 on every call"
                ));
            } else {
                report.accounts_ok += 1;
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(instance: &str, allow: &[&str]) -> TelegramAccount {
        TelegramAccount {
            instance: instance.into(),
            token: "123:ABC".into(),
            allow_agents: allow.iter().map(|s| s.to_string()).collect(),
            allowed_chat_ids: vec![],
        }
    }

    #[test]
    fn token_is_redacted_in_debug() {
        let a = mk("ana", &["ana"]);
        let rendered = format!("{a:?}");
        assert!(!rendered.contains("123:ABC"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn issue_and_list() {
        let store = TelegramCredentialStore::new(vec![mk("a", &["ana"]), mk("b", &[])]);
        assert_eq!(store.list(), vec!["a", "b"]);
        assert!(store.issue("a", "ana").is_ok());
        assert!(matches!(
            store.issue("a", "kate").unwrap_err(),
            CredentialError::NotPermitted { .. }
        ));
        assert!(store.issue("b", "kate").is_ok());
    }

    #[test]
    fn empty_token_warning() {
        let account = TelegramAccount {
            token: "   ".into(),
            ..mk("a", &[])
        };
        let store = TelegramCredentialStore::new(vec![account]);
        let report = store.validate();
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.accounts_ok, 0);
    }
}
