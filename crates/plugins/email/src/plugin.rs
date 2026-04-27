use std::sync::Arc;

use async_trait::async_trait;
use nexo_broker::AnyBroker;
use nexo_config::types::plugins::EmailPluginConfig;
use nexo_core::agent::plugin::{Command, Plugin, Response};
use tokio::sync::OnceCell;
use tracing::info;

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

/// Phase 48.1 stub. Wires config + lifecycle no-ops so the plugin can
/// be registered at boot without panicking. Real IDLE/SMTP work lands
/// in 48.3+.
pub struct EmailPlugin {
    cfg: Arc<EmailPluginConfig>,
    broker: OnceCell<AnyBroker>,
}

impl EmailPlugin {
    pub fn new(cfg: EmailPluginConfig) -> Self {
        Self {
            cfg: Arc::new(cfg),
            broker: OnceCell::new(),
        }
    }

    pub fn config(&self) -> &EmailPluginConfig {
        &self.cfg
    }
}

#[async_trait]
impl Plugin for EmailPlugin {
    fn name(&self) -> &str {
        "email"
    }

    async fn start(&self, broker: AnyBroker) -> anyhow::Result<()> {
        let _ = self.broker.set(broker);
        info!(
            target: "plugin.email",
            accounts = self.cfg.accounts.len(),
            enabled = self.cfg.enabled,
            "email plugin started (48.1 scaffold — IDLE/SMTP land in 48.3+)"
        );
        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        info!(target: "plugin.email", "email plugin stopped");
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

    fn cfg() -> EmailPluginConfig {
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
        let p = EmailPlugin::new(cfg());
        assert_eq!(p.name(), "email");
        // start/stop without broker registration is exercised in the
        // higher-level integration tests once 48.3 lands. Here we just
        // confirm the plugin can be constructed and stopped cleanly.
        p.stop().await.unwrap();
    }
}
