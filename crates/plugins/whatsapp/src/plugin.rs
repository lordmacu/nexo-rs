//! `WhatsappPlugin` — implementation of the core `Plugin` trait.
//!
//! Phases wired as of 6.3:
//!   - 6.2 boot checks + directory layout (pre-flight in `start`)
//!   - 6.3 inbound bridge: `Session::run_agent_with` + `bridge::build_handler`
//!
//! Pending:
//!   - 6.4 outbound dispatcher (reactive oneshot resolve + proactive send)
//!   - 6.5 media
//!   - 6.6 lifecycle/health events
//!   - 6.7 transcriber

use std::sync::Arc;

use agent_broker::{AnyBroker, BrokerHandle, Event};
use agent_config::WhatsappPluginConfig;
use agent_core::agent::plugin::{Command, Plugin, Response};
use anyhow::{Context, Result};
use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::{oneshot, Mutex, OnceCell};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::lifecycle::{LifecycleState, PluginHealth, SharedLifecycle};
use crate::pairing::{PairingState, SharedPairingState};

/// Map of session ids → oneshot senders waiting for an outbound reply.
///
/// Shared between the inbound bridge (which inserts) and the outbound
/// dispatcher (Phase 6.4, which resolves). The dispatcher also falls
/// back to direct `Session::send_text` when no sender matches — that's
/// how proactive Heartbeat / A2A outputs reach the user.
pub(crate) type PendingMap = Arc<DashMap<Uuid, oneshot::Sender<String>>>;

pub struct WhatsappPlugin {
    cfg: Arc<WhatsappPluginConfig>,
    /// Per-instance registry name. `"whatsapp"` (legacy single-account)
    /// when `cfg.instance` is unset; `"whatsapp.<instance>"` otherwise.
    /// `PluginRegistry` keys on this string — multi-account setups need
    /// distinct names or the second registration overwrites the first.
    registry_name: String,
    session: Arc<OnceCell<Arc<whatsapp_rs::Session>>>,
    broker: Arc<OnceCell<AnyBroker>>,
    pending: PendingMap,
    lifecycle: SharedLifecycle,
    pairing: SharedPairingState,
    shutdown: CancellationToken,
    /// Background loops (inbound, dispatch, lifecycle) registered at
    /// `start()` so `stop()` can join them instead of racing shutdown.
    spawned: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl WhatsappPlugin {
    pub fn new(cfg: WhatsappPluginConfig) -> Self {
        let registry_name = match cfg.instance.as_deref() {
            Some(inst) if !inst.is_empty() => format!("whatsapp.{inst}"),
            _ => "whatsapp".to_string(),
        };
        Self {
            cfg: Arc::new(cfg),
            registry_name,
            session: Arc::new(OnceCell::new()),
            broker: Arc::new(OnceCell::new()),
            pending: Arc::new(DashMap::new()),
            lifecycle: Arc::new(Mutex::new(LifecycleState::default())),
            pairing: PairingState::new(),
            shutdown: CancellationToken::new(),
            spawned: Mutex::new(Vec::new()),
        }
    }

    /// External accessor so the HTTP server can poll pairing state
    /// without holding a reference to the whole plugin.
    pub fn pairing_state(&self) -> SharedPairingState {
        self.pairing.clone()
    }

    /// Snapshot plugin health — safe to call at any time, including
    /// before `start`. Integrates with core `/health` (Phase 9.3) via
    /// the plugin registry lookup.
    pub async fn health(&self) -> PluginHealth {
        let session = self.session.get().cloned();
        crate::lifecycle::snapshot(&self.lifecycle, &session).await
    }

    pub fn config(&self) -> &WhatsappPluginConfig {
        &self.cfg
    }

    #[allow(dead_code)]
    pub(crate) fn pending(&self) -> PendingMap {
        self.pending.clone()
    }
}

/// Translate a core `Command` into the JSON payload shape our
/// dispatcher understands. Returns an error for variants the plugin
/// does not yet route.
fn command_to_payload(cmd: &Command) -> Result<serde_json::Value> {
    match cmd {
        Command::SendMessage { to, text } => Ok(serde_json::json!({
            "kind": "text",
            "to":   to,
            "text": text,
        })),
        Command::SendMedia { to, url, caption } => Ok(serde_json::json!({
            "kind":    "media",
            "to":      to,
            "url":     url,
            "caption": caption,
        })),
        Command::Custom { name, payload } => {
            let mut obj = match payload {
                serde_json::Value::Object(m) => m.clone(),
                _ => serde_json::Map::new(),
            };
            obj.insert("kind".into(), serde_json::Value::String(name.clone()));
            Ok(serde_json::Value::Object(obj))
        }
    }
}

#[async_trait]
impl Plugin for WhatsappPlugin {
    fn name(&self) -> &str {
        &self.registry_name
    }

    async fn start(&self, broker: AnyBroker) -> Result<()> {
        if !self.cfg.enabled {
            tracing::info!("whatsapp plugin disabled — skipping start");
            return Ok(());
        }

        // Data-dir isolation is now a parameter on `Client::new_in_dir`
        // inside `connect_session`; no more process-wide XDG mutation.
        // `ensure_session_dirs` still runs here so the path exists
        // before we hand it to the library (lets us surface any mkdir
        // error before the slower connect step).
        crate::session::ensure_session_dirs(&self.cfg)?;
        crate::session::check_daemon_collision(&self.cfg)?;

        let session =
            crate::session::connect_session(&self.cfg, broker.clone(), self.pairing.clone())
                .await
                .context("wa-agent Session bootstrap failed")?;
        let session = Arc::new(session);
        self.session
            .set(session.clone())
            .map_err(|_| anyhow::anyhow!("session already initialised"))?;
        self.broker
            .set(broker.clone())
            .map_err(|_| anyhow::anyhow!("broker already initialised"))?;

        let handler = crate::bridge::build_handler(
            broker.clone(),
            self.pending.clone(),
            self.cfg.clone(),
            session.clone(),
        );
        let acl = crate::bridge::build_acl(&self.cfg);
        let session_for_task = session.clone();
        let cancel = self.shutdown.clone();
        let broker_for_transcribe = broker.clone();
        let cfg_for_transcribe = self.cfg.clone();

        let inbound_handle = tokio::spawn(async move {
            tracing::info!(our_jid = %session_for_task.our_jid, "whatsapp inbound loop starting");
            let run = async {
                if cfg_for_transcribe.transcriber.enabled {
                    let t = crate::transcriber::NatsTranscriber::new(
                        broker_for_transcribe,
                        &cfg_for_transcribe.transcriber.skill,
                        std::time::Duration::from_millis(cfg_for_transcribe.transcriber.timeout_ms),
                    );
                    session_for_task
                        .run_agent_with_transcribe(acl, t, handler)
                        .await
                } else {
                    session_for_task.run_agent_with(acl, handler).await
                }
            };
            tokio::select! {
                res = run => {
                    if let Err(e) = res {
                        tracing::error!(error = %e, "run_agent loop exited");
                    }
                }
                _ = cancel.cancelled() => {
                    tracing::info!("whatsapp inbound loop cancelled");
                }
            }
        });

        let instance = self.cfg.instance.as_deref();
        let inbound_topic = crate::bridge::inbound_topic_for(instance);
        let outbound_topic = crate::bridge::outbound_topic_for(instance);
        let dispatch_handle = crate::dispatch::spawn(
            broker.clone(),
            session.clone(),
            self.pending.clone(),
            self.shutdown.clone(),
            outbound_topic,
        );

        let lifecycle_handle = crate::lifecycle::spawn(
            broker,
            session,
            self.lifecycle.clone(),
            self.pairing.clone(),
            self.shutdown.clone(),
            inbound_topic,
        );

        *self.spawned.lock().await = vec![inbound_handle, dispatch_handle, lifecycle_handle];

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.shutdown.cancel();
        self.pending.clear();
        // Wait up to 5s per task for graceful exit. Anything still
        // running after that gets dropped — mostly belt-and-suspenders
        // because `cancel` already signals the loops to break.
        let handles = std::mem::take(&mut *self.spawned.lock().await);
        for h in handles {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), h).await;
        }
        Ok(())
    }

    async fn send_command(&self, cmd: Command) -> Result<Response> {
        // Route programmatic calls through the broker so the normal
        // outbound dispatcher handles them — single code path for both
        // tool-driven and core-driven sends.
        let broker = self
            .broker
            .get()
            .ok_or_else(|| anyhow::anyhow!("whatsapp plugin not started"))?;
        let payload = command_to_payload(&cmd)?;
        let outbound_topic = crate::bridge::outbound_topic_for(self.cfg.instance.as_deref());
        let event = Event::new(&outbound_topic, self.name(), payload);
        broker
            .publish(&outbound_topic, event)
            .await
            .map_err(|e| anyhow::anyhow!("broker publish failed: {e}"))?;
        Ok(Response::Ok)
    }
}
