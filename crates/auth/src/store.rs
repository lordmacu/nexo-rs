use std::path::PathBuf;

use crate::error::{BuildError, CredentialError};
use crate::handle::{Channel, CredentialHandle};

/// Per-channel credential store. Implementations own the raw account
/// data (session dirs, tokens) and issue opaque [`CredentialHandle`]s
/// that agent tools can use to publish outbound traffic without ever
/// touching the account id in logs.
pub trait CredentialStore: Send + Sync + 'static {
    type Account: Clone + Send + Sync;

    fn channel(&self) -> Channel;

    /// Materialise the account data for the handle. Returns
    /// [`CredentialError::NotFound`] if the handle refers to an
    /// account that was removed since issuance (hot-reload edge case).
    fn get(&self, handle: &CredentialHandle) -> Result<Self::Account, CredentialError>;

    /// Create a handle after checking that `agent_id` is permitted on
    /// the account's `allow_agents` list (empty list = accept all).
    /// Called by the resolver at boot — never from hot paths.
    fn issue(&self, account_id: &str, agent_id: &str) -> Result<CredentialHandle, CredentialError>;

    /// Enumerate every account id known to this store. Used by the
    /// gauntlet to diagnose missing `credentials.<channel>` bindings.
    fn list(&self) -> Vec<String>;

    /// `allow_agents` for an account, for boot-time cross-validation.
    /// Empty vec means the account accepts any agent.
    fn allow_agents(&self, account_id: &str) -> Vec<String>;

    /// Run the store's internal invariants (permissions, missing
    /// files). Errors are non-fatal on their own — the gauntlet
    /// merges them with cross-store checks before failing boot.
    fn validate(&self) -> ValidationReport;
}

/// Outcome of a per-store validation pass. Warnings are advisory;
/// errors fail boot once collected across all stores.
#[derive(Debug, Default)]
pub struct ValidationReport {
    pub accounts_ok: usize,
    pub warnings: Vec<String>,
    pub insecure_paths: Vec<PathBuf>,
    pub unused: Vec<String>,
    pub errors: Vec<BuildError>,
}

impl ValidationReport {
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty() && self.insecure_paths.is_empty()
    }
}
