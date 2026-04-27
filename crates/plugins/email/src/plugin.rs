use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use nexo_auth::email::EmailCredentialStore;
use nexo_auth::google::GoogleCredentialStore;
use nexo_broker::AnyBroker;
use nexo_config::types::plugins::EmailPluginConfig;
use nexo_core::agent::plugin::{Command, Plugin, Response};
use tokio::sync::{Mutex, OnceCell};
use tracing::info;

use crate::attachment_store::AttachmentStore;
use crate::bounce_store::BounceStore;
use crate::cursor::CursorStore;
use crate::inbound::{HealthMap, InboundManager};
use crate::outbound::OutboundDispatcher;

pub const TOPIC_INBOUND: &str = "plugin.inbound.email";
pub const TOPIC_OUTBOUND: &str = "plugin.outbound.email";

/// Build the inbound topic for a given account instance. Mirrors the
/// telegram pattern so per-account agent bindings can target a specific
/// mailbox via `inbound_bindings: [{plugin: email, instance: <id>}]`.
pub fn inbound_topic_for(instance: &str) -> String {
    if instance.is_empty() {
        TOPIC_INBOUND.to_string()
    } else {
        format!("{}.{}", TOPIC_INBOUND, instance)
    }
}

pub fn outbound_topic_for(instance: &str) -> String {
    if instance.is_empty() {
        TOPIC_OUTBOUND.to_string()
    } else {
        format!("{}.{}", TOPIC_OUTBOUND, instance)
    }
}

/// Phase 48.3 — wires the inbound IMAP IDLE workers. Outbound (SMTP) +
/// MIME parse + tools land in 48.4..48.7.
pub struct EmailPlugin {
    cfg: Arc<EmailPluginConfig>,
    creds: Arc<EmailCredentialStore>,
    google: Arc<GoogleCredentialStore>,
    data_dir: PathBuf,
    inbound: Mutex<Option<InboundManager>>,
    outbound: Mutex<Option<OutboundDispatcher>>,
    cursor: OnceCell<Arc<CursorStore>>,
    bounce: OnceCell<Arc<BounceStore>>,
    attachments: OnceCell<Arc<AttachmentStore>>,
    gc_cancel: OnceCell<tokio_util::sync::CancellationToken>,
}

impl EmailPlugin {
    /// Construct the plugin with its dependencies. `data_dir` is the
    /// daemon's runtime directory; the cursor SQLite lives at
    /// `<data_dir>/email/cursor.db`.
    pub fn new(
        cfg: EmailPluginConfig,
        creds: Arc<EmailCredentialStore>,
        google: Arc<GoogleCredentialStore>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            cfg: Arc::new(cfg),
            creds,
            google,
            data_dir,
            inbound: Mutex::new(None),
            outbound: Mutex::new(None),
            cursor: OnceCell::new(),
            bounce: OnceCell::new(),
            attachments: OnceCell::new(),
            gc_cancel: OnceCell::new(),
        }
    }

    /// Lazily-opened attachment ref store. Failures degrade — a
    /// missing store just means GC doesn't run; attachments still
    /// land on disk.
    async fn attachment_store(&self) -> Option<Arc<AttachmentStore>> {
        if let Some(s) = self.attachments.get() {
            return Some(s.clone());
        }
        let path = self.data_dir.join("email").join("attachments.db");
        match AttachmentStore::open_path(&path).await {
            Ok(store) => {
                let arc = Arc::new(store);
                let _ = self.attachments.set(arc.clone());
                Some(arc)
            }
            Err(e) => {
                tracing::warn!(
                    target: "plugin.email",
                    path = %path.display(),
                    error = %e,
                    "attachment store unavailable — GC disabled this run"
                );
                None
            }
        }
    }

    /// Lazily-opened persistent bounce history. Path is
    /// `<data_dir>/email/bounces.db`. Failures degrade gracefully —
    /// a bounce that can't be persisted still publishes its event
    /// over the broker.
    async fn bounce_store(&self) -> Option<Arc<BounceStore>> {
        if let Some(s) = self.bounce.get() {
            return Some(s.clone());
        }
        let path = self.data_dir.join("email").join("bounces.db");
        match BounceStore::open_path(&path).await {
            Ok(store) => {
                let arc = Arc::new(store);
                let _ = self.bounce.set(arc.clone());
                Some(arc)
            }
            Err(e) => {
                tracing::warn!(
                    target: "plugin.email",
                    path = %path.display(),
                    error = %e,
                    "bounce store unavailable — bounce events will publish but won't persist"
                );
                None
            }
        }
    }

    pub fn config(&self) -> &EmailPluginConfig {
        &self.cfg
    }

    /// Phase 48 follow-up #5 — surgical hot-reload entry point.
    /// Computes the diff between the plugin's *current* config and
    /// `new_cfg`, then teardown / respawn / spawn one worker pair
    /// per affected account on both the inbound and outbound sides.
    /// Sibling accounts keep their IDLE / drain loops untouched.
    /// Returns the diff so the caller can log / metric.
    ///
    /// Order matters: process `removed` first, then `changed`
    /// (teardown of stale + spawn fresh), then `added`. This
    /// matches the natural lifecycle and avoids transient
    /// instance-id collisions when a `changed` account hasn't
    /// finished its old worker before the new one tries to claim
    /// the queue file.
    pub async fn apply_account_diff(
        &self,
        new_cfg: &EmailPluginConfig,
        broker: AnyBroker,
    ) -> anyhow::Result<crate::reload::AccountDiff> {
        let diff = crate::reload::compute_account_diff(&self.cfg, new_cfg);
        if diff.is_empty() {
            return Ok(diff);
        }
        let cursor = self.cursor_store().await?;
        let bounce = self.bounce_store().await;
        let attachments = self.attachment_store().await;

        // Removed: tear down inbound + outbound. Order is
        // outbound-first so any in-flight job lands on disk before
        // the inbound worker that read it disappears.
        for instance in &diff.removed {
            if let Some(disp) = self.outbound.lock().await.as_mut() {
                if disp.remove_account(instance).await {
                    tracing::info!(
                        target: "plugin.email",
                        %instance,
                        "hot-reload removed outbound worker"
                    );
                }
            }
            if let Some(mgr) = self.inbound.lock().await.as_mut() {
                if mgr.remove_account(instance).await {
                    tracing::info!(
                        target: "plugin.email",
                        %instance,
                        "hot-reload removed inbound worker"
                    );
                }
            }
        }

        // Changed: teardown then respawn. Same outbound-first
        // ordering on the teardown leg.
        for account_cfg in &diff.changed {
            let instance = &account_cfg.instance;
            if let Some(disp) = self.outbound.lock().await.as_mut() {
                let _ = disp.remove_account(instance).await;
            }
            if let Some(mgr) = self.inbound.lock().await.as_mut() {
                let _ = mgr.remove_account(instance).await;
            }
            self.spawn_account(
                new_cfg,
                account_cfg,
                cursor.clone(),
                broker.clone(),
                bounce.clone(),
                attachments.clone(),
            )
            .await?;
            tracing::info!(
                target: "plugin.email",
                %instance,
                "hot-reload respawned changed account"
            );
        }

        // Added: brand-new accounts. No teardown needed.
        for account_cfg in &diff.added {
            self.spawn_account(
                new_cfg,
                account_cfg,
                cursor.clone(),
                broker.clone(),
                bounce.clone(),
                attachments.clone(),
            )
            .await?;
            tracing::info!(
                target: "plugin.email",
                instance = %account_cfg.instance,
                "hot-reload spawned new account"
            );
        }
        Ok(diff)
    }

    /// Backward-compat alias retained for callers who only need
    /// the add-only behaviour. Equivalent to
    /// `apply_account_diff` since this commit shipped surgical
    /// teardown — the additional removed/changed branches are
    /// no-ops for an unchanged account set.
    #[deprecated(note = "use apply_account_diff; behaviour is identical now")]
    pub async fn apply_added_accounts(
        &self,
        new_cfg: &EmailPluginConfig,
        broker: AnyBroker,
    ) -> anyhow::Result<crate::reload::AccountDiff> {
        self.apply_account_diff(new_cfg, broker).await
    }

    /// Internal helper used by both `added` and `changed` branches
    /// of `apply_account_diff`. Spawns inbound + outbound workers
    /// for the supplied account, sharing the per-plugin stores.
    #[allow(clippy::too_many_arguments)]
    async fn spawn_account(
        &self,
        new_cfg: &EmailPluginConfig,
        account_cfg: &nexo_config::types::plugins::EmailAccountConfig,
        cursor: Arc<CursorStore>,
        broker: AnyBroker,
        bounce: Option<Arc<BounceStore>>,
        attachments: Option<Arc<AttachmentStore>>,
    ) -> anyhow::Result<()> {
        if let Some(mgr) = self.inbound.lock().await.as_mut() {
            mgr.add_account(
                new_cfg,
                account_cfg,
                self.creds.clone(),
                self.google.clone(),
                cursor,
                broker,
                bounce,
                attachments,
            );
        } else {
            anyhow::bail!("inbound manager not running — call start first");
        }
        if let Some(disp) = self.outbound.lock().await.as_mut() {
            disp.add_account(account_cfg).await?;
        } else {
            anyhow::bail!("outbound dispatcher not running — call start first");
        }
        Ok(())
    }

    /// Persistent bounce history handle. Returns `None` if the
    /// SQLite file couldn't be opened. Outlives `start`/`stop`
    /// since it's `OnceCell`-backed.
    pub fn bounce_store_handle(&self) -> Option<Arc<BounceStore>> {
        self.bounce.get().cloned()
    }

    /// Cheap shared handle the email tools (Phase 48.7) need. Returns
    /// `None` until `start` has armed the outbound dispatcher; the
    /// runtime should invoke this *after* `plugins.start_all()` so
    /// the registry build sees a primed handle.
    pub async fn dispatcher_handle(
        &self,
    ) -> Option<std::sync::Arc<dyn crate::tool::DispatcherHandle>> {
        self.outbound
            .lock()
            .await
            .as_ref()
            .map(|d| d.core() as std::sync::Arc<dyn crate::tool::DispatcherHandle>)
    }

    /// Snapshot of every account's worker health. `None` until `start`
    /// has been called.
    pub async fn health_map(&self) -> Option<HealthMap> {
        self.inbound
            .lock()
            .await
            .as_ref()
            .map(|m| m.health_map())
    }

    async fn cursor_store(&self) -> anyhow::Result<Arc<CursorStore>> {
        if let Some(c) = self.cursor.get() {
            return Ok(c.clone());
        }
        let path = self.data_dir.join("email").join("cursor.db");
        let store = Arc::new(CursorStore::open_path(&path).await?);
        let _ = self.cursor.set(store.clone());
        Ok(store)
    }
}

#[async_trait]
impl Plugin for EmailPlugin {
    fn name(&self) -> &str {
        "email"
    }

    async fn start(&self, broker: AnyBroker) -> anyhow::Result<()> {
        if !self.cfg.enabled {
            info!(target: "plugin.email", "plugin disabled — skipping start");
            return Ok(());
        }
        if self.cfg.accounts.is_empty() {
            info!(
                target: "plugin.email",
                "no accounts configured — start is a noop until 48.10 hot-reload adds one"
            );
            return Ok(());
        }
        let cursor = self.cursor_store().await?;
        let bounce = self.bounce_store().await;
        let attachments = self.attachment_store().await;
        let manager = InboundManager::start(
            &self.cfg,
            self.creds.clone(),
            self.google.clone(),
            cursor,
            broker.clone(),
            bounce,
            attachments.clone(),
        );

        // Phase 48 follow-up #10 — daily attachment GC. Kicks off
        // when retention > 0; otherwise the operator opted into
        // unbounded storage. The cancel token lives in `gc_cancel`
        // so `stop()` shuts it down cleanly.
        if self.cfg.attachment_retention_days > 0 {
            if let Some(store) = attachments {
                let cancel = tokio_util::sync::CancellationToken::new();
                let _ = self.gc_cancel.set(cancel.clone());
                let attachments_dir = self.data_dir.join(&self.cfg.attachments_dir);
                let retention_secs =
                    (self.cfg.attachment_retention_days as i64).saturating_mul(86_400);
                tokio::spawn(async move {
                    // Run an initial sweep then daily.
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_secs(86_400));
                    loop {
                        tokio::select! {
                            _ = cancel.cancelled() => return,
                            _ = interval.tick() => {
                                match store.gc(&attachments_dir, retention_secs).await {
                                    Ok(n) if n > 0 => tracing::info!(
                                        target: "plugin.email",
                                        files_removed = n,
                                        "attachment GC swept stale files"
                                    ),
                                    Ok(_) => {}
                                    Err(e) => tracing::warn!(
                                        target: "plugin.email",
                                        error = %e,
                                        "attachment GC failed (will retry tomorrow)"
                                    ),
                                }
                            }
                        }
                    }
                });
            }
        }
        let health = manager.health_map();
        *self.inbound.lock().await = Some(manager);

        let dispatcher = OutboundDispatcher::start(
            &self.cfg,
            self.creds.clone(),
            self.google.clone(),
            broker,
            &self.data_dir,
            health,
        )
        .await?;
        *self.outbound.lock().await = Some(dispatcher);

        // Phase 48.9 — boot-time SPF/DKIM check, one task per account.
        // Non-blocking: a private domain or DNS flake produces a WARN
        // log, not a daemon-aborting error.
        if self.cfg.spf_dkim_warn {
            for acct in self.cfg.accounts.iter().cloned() {
                let domain = acct
                    .address
                    .split_once('@')
                    .map(|(_, d)| d.to_string())
                    .unwrap_or_default();
                if domain.is_empty() {
                    continue;
                }
                let smtp_host = acct.smtp.host.clone();
                let instance = acct.instance.clone();
                tokio::spawn(async move {
                    let report = crate::spf_dkim::check_alignment(
                        &domain,
                        Some(&smtp_host),
                        std::time::Duration::from_secs(3),
                    )
                    .await;
                    for tag in crate::spf_dkim::decide_warns(&report) {
                        match tag {
                            "spf_missing" => tracing::warn!(
                                target: "plugin.email",
                                instance = %instance,
                                domain = %domain,
                                "email.spf.missing — no v=spf1 TXT at apex"
                            ),
                            "spf_misalignment" => tracing::warn!(
                                target: "plugin.email",
                                instance = %instance,
                                domain = %domain,
                                sending_host = %smtp_host,
                                "email.spf.misalignment — sending_host not in SPF policy; recipients may flag as spoof"
                            ),
                            "dkim_missing" => tracing::warn!(
                                target: "plugin.email",
                                instance = %instance,
                                domain = %domain,
                                selectors_probed = ?crate::spf_dkim::DKIM_SELECTORS,
                                "email.dkim.missing — no TXT at any probed selector; if your domain rotates a custom selector, publish a DKIM record there"
                            ),
                            "dns_error" => tracing::warn!(
                                target: "plugin.email",
                                instance = %instance,
                                domain = %domain,
                                "email.spf_dkim.dns_unavailable — skipping alignment check this boot"
                            ),
                            _ => {}
                        }
                    }
                });
            }
        }

        info!(
            target: "plugin.email",
            accounts = self.cfg.accounts.len(),
            "email inbound + outbound workers spawned"
        );
        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        if let Some(cancel) = self.gc_cancel.get() {
            cancel.cancel();
        }
        if let Some(dispatcher) = self.outbound.lock().await.take() {
            dispatcher.stop().await;
        }
        if let Some(manager) = self.inbound.lock().await.take() {
            manager.stop().await;
        }
        info!(target: "plugin.email", "email workers stopped");
        Ok(())
    }

    async fn send_command(&self, _cmd: Command) -> anyhow::Result<Response> {
        Ok(Response::Error {
            message: "email plugin send_command not implemented yet (Phase 48.7)".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_config::types::plugins::EmailPluginConfigFile;

    fn cfg_no_accounts() -> EmailPluginConfig {
        let yaml = r#"
email:
  accounts: []
"#;
        let f: EmailPluginConfigFile = serde_yaml::from_str(yaml).unwrap();
        f.email
    }

    #[test]
    fn topic_helpers() {
        assert_eq!(inbound_topic_for(""), "plugin.inbound.email");
        assert_eq!(inbound_topic_for("ops"), "plugin.inbound.email.ops");
        assert_eq!(outbound_topic_for(""), "plugin.outbound.email");
        assert_eq!(outbound_topic_for("ops"), "plugin.outbound.email.ops");
    }

    #[tokio::test]
    async fn lifecycle_noop_does_not_panic() {
        let p = EmailPlugin::new(
            cfg_no_accounts(),
            Arc::new(EmailCredentialStore::empty()),
            Arc::new(GoogleCredentialStore::empty()),
            std::env::temp_dir().join("nexo-email-test"),
        );
        assert_eq!(p.name(), "email");
        // start with zero accounts is a noop — no broker needed.
        // Only stop is exercised here; the start path requires a real
        // broker which integration tests will wire up.
        p.stop().await.unwrap();
    }
}
