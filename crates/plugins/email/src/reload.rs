//! Hot-reload account-diff helpers (Phase 48 follow-up #5).
//!
//! Today a config reload restarts the whole email plugin — both
//! inbound IDLE workers and the outbound dispatcher. This module
//! computes which accounts were `added`, `removed`, or `changed`
//! between two `EmailPluginConfig` snapshots so the runtime can
//! eventually spawn / teardown / restart only the affected
//! workers.
//!
//! v1 ships the pure diff + add-only side. Removing a live worker
//! requires per-instance cancel tokens that the inbound /
//! outbound managers don't yet split out — the diff exposes the
//! `removed` set so the future implementation knows what to tear
//! down, but `EmailPlugin::apply_added_accounts` is the only side
//! that's wired today.

use nexo_config::types::plugins::{EmailAccountConfig, EmailPluginConfig};

#[derive(Debug, Clone, Default)]
pub struct AccountDiff {
    /// Accounts present in `new` but not in `old`. Safe to spawn
    /// workers for these without touching anything else.
    pub added: Vec<EmailAccountConfig>,
    /// Accounts in `old` but missing from `new`. Future work tears
    /// these down; today the operator restarts the daemon to pick
    /// up the removal.
    pub removed: Vec<String>,
    /// Accounts present in both snapshots whose config changed
    /// (host, port, TLS mode, folder names, filters, etc.). A
    /// surgical reload would tear down + respawn just these
    /// workers — for now they share the `removed` story.
    pub changed: Vec<EmailAccountConfig>,
}

impl AccountDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Compute the per-account diff between two plugin configs.
/// Equality is structural (every field on `EmailAccountConfig`):
/// even a port change moves the account into `changed` rather
/// than leaving the worker pointing at stale endpoints.
pub fn compute_account_diff(
    old: &EmailPluginConfig,
    new: &EmailPluginConfig,
) -> AccountDiff {
    let mut diff = AccountDiff::default();
    for new_acct in &new.accounts {
        match old
            .accounts
            .iter()
            .find(|a| a.instance == new_acct.instance)
        {
            None => diff.added.push(new_acct.clone()),
            Some(old_acct) if !accounts_equal(old_acct, new_acct) => {
                diff.changed.push(new_acct.clone())
            }
            Some(_) => {}
        }
    }
    for old_acct in &old.accounts {
        if !new
            .accounts
            .iter()
            .any(|a| a.instance == old_acct.instance)
        {
            diff.removed.push(old_acct.instance.clone());
        }
    }
    diff
}

/// Field-by-field equality on `EmailAccountConfig`. Derived
/// `PartialEq` would have worked if the upstream struct had it,
/// but it doesn't — and adding `PartialEq` to a config type
/// touched by half the workspace is more risk than this helper.
fn accounts_equal(a: &EmailAccountConfig, b: &EmailAccountConfig) -> bool {
    a.instance == b.instance
        && a.address == b.address
        && std::mem::discriminant(&a.provider) == std::mem::discriminant(&b.provider)
        && a.imap.host == b.imap.host
        && a.imap.port == b.imap.port
        && std::mem::discriminant(&a.imap.tls) == std::mem::discriminant(&b.imap.tls)
        && a.smtp.host == b.smtp.host
        && a.smtp.port == b.smtp.port
        && std::mem::discriminant(&a.smtp.tls) == std::mem::discriminant(&b.smtp.tls)
        && a.folders.inbox == b.folders.inbox
        && a.folders.sent == b.folders.sent
        && a.folders.archive == b.folders.archive
        && a.filters.from_allowlist == b.filters.from_allowlist
        && a.filters.from_denylist == b.filters.from_denylist
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_config::types::plugins::EmailPluginConfigFile;

    fn cfg(accounts_yaml: &str) -> EmailPluginConfig {
        let yaml = format!("email:\n  accounts:\n{accounts_yaml}");
        let f: EmailPluginConfigFile = serde_yaml::from_str(&yaml).unwrap();
        f.email
    }

    fn one(instance: &str) -> String {
        format!(
            "    - instance: {instance}\n      address: {instance}@example.com\n      imap: {{ host: imap.example.com, port: 993 }}\n      smtp: {{ host: smtp.example.com, port: 587 }}\n"
        )
    }

    #[test]
    fn empty_to_one_yields_one_added() {
        let d = compute_account_diff(&cfg(""), &cfg(&one("ops")));
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].instance, "ops");
        assert!(d.removed.is_empty());
        assert!(d.changed.is_empty());
    }

    #[test]
    fn one_to_empty_yields_one_removed() {
        let d = compute_account_diff(&cfg(&one("ops")), &cfg(""));
        assert_eq!(d.removed, vec!["ops".to_string()]);
        assert!(d.added.is_empty());
        assert!(d.changed.is_empty());
    }

    #[test]
    fn same_set_yields_empty_diff() {
        let a = cfg(&one("ops"));
        let b = cfg(&one("ops"));
        let d = compute_account_diff(&a, &b);
        assert!(d.is_empty());
    }

    #[test]
    fn port_change_lands_in_changed() {
        let a = cfg(&one("ops"));
        let mut b = cfg(&one("ops"));
        b.accounts[0].imap.port = 1993;
        let d = compute_account_diff(&a, &b);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].instance, "ops");
        assert!(d.added.is_empty());
        assert!(d.removed.is_empty());
    }

    #[test]
    fn add_remove_simultaneously() {
        let d = compute_account_diff(&cfg(&one("a")), &cfg(&one("b")));
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].instance, "b");
        assert_eq!(d.removed, vec!["a".to_string()]);
    }

    #[test]
    fn add_alongside_keep_yields_only_added() {
        let mut both = String::new();
        both.push_str(&one("ops"));
        both.push_str(&one("support"));
        let d = compute_account_diff(&cfg(&one("ops")), &cfg(&both));
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].instance, "support");
        assert!(d.removed.is_empty());
    }
}
