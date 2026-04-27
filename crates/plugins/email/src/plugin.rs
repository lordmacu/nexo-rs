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
        }
    }

    pub fn config(&self) -> &EmailPluginConfig {
        &self.cfg
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
        let manager = InboundManager::start(
            &self.cfg,
            self.creds.clone(),
            self.google.clone(),
            cursor,
            broker.clone(),
        );
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

        info!(
            target: "plugin.email",
            accounts = self.cfg.accounts.len(),
            "email inbound + outbound workers spawned"
        );
        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
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
