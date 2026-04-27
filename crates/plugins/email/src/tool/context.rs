//! Shared handle the email tools (Phase 48.7) read from.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use nexo_auth::email::EmailCredentialStore;
use nexo_auth::google::GoogleCredentialStore;
use nexo_config::types::plugins::EmailPluginConfig;

use crate::events::OutboundCommand;
use crate::inbound::HealthMap;

/// Outbound dispatcher façade. `OutboundDispatcher` implements it,
/// tests can stub it. Tools never reach into the dispatcher's
/// internals — they go through this trait so refactors of the
/// dispatcher don't ripple into every handler.
#[async_trait]
pub trait DispatcherHandle: Send + Sync {
    /// Build a Message-ID, render MIME, persist the job, return the
    /// id so the caller can correlate the eventual ack.
    async fn enqueue_for_instance(&self, instance: &str, cmd: OutboundCommand) -> Result<String>;

    /// Sorted list of declared instance ids. Tools call this to
    /// validate `instance` arguments before touching IMAP / SMTP.
    fn instance_ids(&self) -> Vec<String>;
}

pub struct EmailToolContext {
    pub creds: Arc<EmailCredentialStore>,
    pub google: Arc<GoogleCredentialStore>,
    pub config: Arc<EmailPluginConfig>,
    pub dispatcher: Arc<dyn DispatcherHandle>,
    pub health: HealthMap,
    /// Phase 48 follow-up #4 — persistent bounce history. `None`
    /// when the plugin couldn't open `bounces.db`; the tools
    /// degrade to "no warnings" rather than refusing to send.
    pub bounce_store: Option<Arc<crate::bounce_store::BounceStore>>,
    /// Phase 48 use cases — attachment dedup store. `None` when
    /// the SQLite file couldn't be opened at boot. `email_attachment_get`
    /// requires both this and `attachments_dir` to be set.
    pub attachment_store: Option<Arc<crate::attachment_store::AttachmentStore>>,
    /// Phase 48 use cases — resolved data directory for attachment
    /// blobs. `email_attachment_get` joins this with `<sha256>` to
    /// read bytes off disk after verifying the hash exists in
    /// `attachment_store`.
    pub attachments_dir: std::path::PathBuf,
}

impl EmailToolContext {
    /// Look up an account's `EmailAccountConfig` by instance id, or
    /// `None` when the operator hasn't declared it. Tools surface
    /// this as `unknown email instance: <id>` in the result envelope.
    pub fn account(
        &self,
        instance: &str,
    ) -> Option<&nexo_config::types::plugins::EmailAccountConfig> {
        self.config.accounts.iter().find(|a| a.instance == instance)
    }
}
