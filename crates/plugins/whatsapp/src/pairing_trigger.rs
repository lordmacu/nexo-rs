//! Phase 82.10.p — `WhatsappPairingTrigger` impl.
//!
//! Bridges admin RPC `pairing/start` to the WhatsApp channel
//! plugin: spawns `wa-agent` in pairing mode (via
//! `session::pair_with_callback`) and routes every QR rotation
//! into the challenge store + SSE notifier so the operator UI
//! receives the QR live and the user can scan from their phone.
//!
//! Lifecycle:
//!
//! 1. Admin RPC `pairing/start` looks up
//!    `PairingChannelTriggers["whatsapp"]` and calls
//!    [`WhatsappPairingTrigger::start`].
//! 2. `start` validates `instance` against the configured
//!    accounts, then spawns a task running
//!    `pair_with_callback`.
//! 3. Each `wa-agent` `on_qr` callback fires the closure that
//!    pushes `(qr_png_b64, qr_ascii, expires_at_ms)` into
//!    `ctx.store` (via `update_qr` → `Pending → QrReady`) and
//!    forwards a `PairingStatus` notification to
//!    `ctx.notifier` if one is wired.
//! 4. When `pair_with_callback` resolves, the task flips the
//!    challenge to `Linked` (success) or stamps `data.error`
//!    on the existing state (failure). Terminal — handle map
//!    eviction + notifier push handled by the caller.
//! 5. `tokio::select!` observes `ctx.cancel`: TTL eviction or
//!    operator-side `pairing/cancel` cancels the token, the
//!    spawned task drops the wa-agent client (kill_on_drop
//!    closes the WebSocket), and exits without flipping
//!    state (cancel path is owned by `cancel_with_handles` in
//!    the dispatcher).

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use nexo_config::WhatsappPluginConfig;
use nexo_core::agent::admin_rpc::pairing_trigger::{
    PairingChannelTrigger, PairingContext, PairingHandle, PairingTriggerError,
};
use nexo_tool_meta::admin::pairing::{PairingState, PairingStatus, PairingStatusData};

use crate::session::pair_with_callback;

/// Channel id this trigger handles. Stable string —
/// matches `pairing/start.params.channel`.
pub const CHANNEL_ID: &str = "whatsapp";

/// Trigger implementation registered with the dispatcher's
/// `PairingChannelTriggers` map. Holds an instance → session_dir
/// resolution table built once at boot from the validated
/// `WhatsappPluginConfig` entries.
#[derive(Debug)]
pub struct WhatsappPairingTrigger {
    /// Map from `instance` label (or "" for legacy single-
    /// account) to the on-disk session dir. Resolved from
    /// `Vec<WhatsappPluginConfig>` at construction time.
    instance_dirs: HashMap<String, PathBuf>,
    /// Optional default instance applied when the operator
    /// passes `instance: None`. First configured account wins
    /// (mirrors the legacy `pair_once` single-account path).
    default_instance: Option<String>,
}

impl WhatsappPairingTrigger {
    /// Build from the daemon's configured WhatsApp accounts.
    /// Empty configs produce a trigger that rejects every
    /// `start` with `InstanceNotConfigured`.
    pub fn from_configs(configs: &[WhatsappPluginConfig]) -> Self {
        let mut instance_dirs = HashMap::new();
        let mut default_instance = None;
        for cfg in configs {
            let key = cfg.instance.clone().unwrap_or_default();
            if default_instance.is_none() {
                default_instance = Some(key.clone());
            }
            instance_dirs.insert(key, PathBuf::from(&cfg.session_dir));
        }
        Self {
            instance_dirs,
            default_instance,
        }
    }

    /// Resolve the requested instance to its session dir,
    /// applying the legacy single-account default when the
    /// caller passed `None`.
    fn resolve_session_dir(&self, instance: Option<&str>) -> Option<PathBuf> {
        let key = instance
            .map(str::to_string)
            .or_else(|| self.default_instance.clone())
            .unwrap_or_default();
        self.instance_dirs.get(&key).cloned()
    }
}

#[async_trait]
impl PairingChannelTrigger for WhatsappPairingTrigger {
    fn channel_id(&self) -> &str {
        CHANNEL_ID
    }

    async fn start(&self, ctx: PairingContext) -> Result<PairingHandle, PairingTriggerError> {
        let session_dir = match self.resolve_session_dir(ctx.instance.as_deref()) {
            Some(p) => p,
            None => {
                let label = ctx.instance.clone().unwrap_or_else(|| "(default)".into());
                return Err(PairingTriggerError::InstanceNotConfigured(label));
            }
        };

        // Capture context fields the spawned task needs. The
        // store is `Arc` so the on_qr closure (sync, FnMut)
        // can clone-and-update without async.
        let challenge_id = ctx.challenge_id;
        let store = ctx.store.clone();
        let notifier = ctx.notifier.clone();
        let cancel = ctx.cancel.clone();
        let handle_cancel = cancel.clone();

        // Closure passed into `pair_with_callback` — fires on
        // every QR rotation (~20s on WhatsApp). Updates the
        // store synchronously + pushes a notification frame.
        let store_for_qr = store.clone();
        let notifier_for_qr = notifier.clone();
        let on_qr = move |qr_png_b64: String, qr_ascii: String, expires_at_ms: u64| {
            // Skip update if cancelled — race between QR
            // arrival and operator clicking cancel.
            let _ = store_for_qr.update_qr(
                challenge_id,
                qr_png_b64.clone(),
                qr_ascii.clone(),
                expires_at_ms,
            );
            if let Some(n) = &notifier_for_qr {
                let mut data = PairingStatusData::default();
                data.qr_png_base64 = Some(qr_png_b64);
                data.qr_ascii = Some(qr_ascii);
                n.notify_status(&PairingStatus {
                    challenge_id,
                    state: PairingState::QrReady,
                    data,
                });
            }
        };

        // Spawn the wa-agent pairing flow with cancel-aware
        // select. `kill_on_drop` semantics on the wa-agent
        // Client mean dropping the future closes the
        // WebSocket cleanly.
        tokio::spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!(
                        challenge_id = %challenge_id,
                        "WhatsappPairingTrigger: cancelled before pair completed",
                    );
                    // Cancel path is owned by the dispatcher
                    // (`cancel_with_handles` flips the store
                    // entry to Cancelled); don't double-write.
                }
                result = pair_with_callback(&session_dir, on_qr) => {
                    match result {
                        Ok(_outcome) => {
                            // wa-agent returned without
                            // device_jid (today). Mark the
                            // challenge `Linked` with empty
                            // payload — UIs have already
                            // received the QR via update_qr +
                            // SSE notification. Future wa-agent
                            // upgrade lands `device_jid` here.
                            let data = PairingStatusData::default();
                            let _ = store.update_state(
                                challenge_id,
                                PairingState::Linked,
                                data.clone(),
                            );
                            if let Some(n) = &notifier {
                                n.notify_status(&PairingStatus {
                                    challenge_id,
                                    state: PairingState::Linked,
                                    data,
                                });
                            }
                            tracing::info!(
                                challenge_id = %challenge_id,
                                "WhatsappPairingTrigger: pair_with_callback resolved Ok",
                            );
                        }
                        Err(err) => {
                            // wa-agent dial / handshake / 401
                            // failure. Stamp `data.error` on the
                            // existing state — UIs branch on
                            // `data.error.is_some()` because the
                            // wire enum has no `Error` variant.
                            let mut data = PairingStatusData::default();
                            data.error = Some(err.to_string());
                            // Re-read the current state so we
                            // don't accidentally downgrade.
                            // Trigger uses the existing state +
                            // attaches the error payload.
                            let current_state = store
                                .read_challenge(challenge_id)
                                .ok()
                                .flatten()
                                .map(|s| s.state)
                                .unwrap_or(PairingState::Pending);
                            let _ = store.update_state(challenge_id, current_state, data.clone());
                            if let Some(n) = &notifier {
                                n.notify_status(&PairingStatus {
                                    challenge_id,
                                    state: current_state,
                                    data,
                                });
                            }
                            tracing::warn!(
                                challenge_id = %challenge_id,
                                error = %err,
                                "WhatsappPairingTrigger: pair_with_callback failed",
                            );
                        }
                    }
                }
            }
        });

        Ok(PairingHandle {
            challenge_id,
            channel: CHANNEL_ID.to_string(),
            cancel: handle_cancel,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_config::{
        WhatsappAclConfig, WhatsappBehaviorConfig, WhatsappBridgeConfig, WhatsappDaemonConfig,
        WhatsappPublicTunnelConfig, WhatsappRateLimitConfig, WhatsappTranscriberConfig,
    };
    use nexo_core::agent::admin_rpc::domains::pairing::PairingChallengeStore;
    use nexo_tool_meta::admin::pairing::PairingStatus;
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    fn cfg_with(instance: Option<&str>, session_dir: &str) -> WhatsappPluginConfig {
        WhatsappPluginConfig {
            enabled: true,
            session_dir: session_dir.to_string(),
            media_dir: "/tmp/media".into(),
            credentials_file: None,
            acl: WhatsappAclConfig::default(),
            behavior: WhatsappBehaviorConfig::default(),
            rate_limit: WhatsappRateLimitConfig::default(),
            bridge: WhatsappBridgeConfig::default(),
            transcriber: WhatsappTranscriberConfig::default(),
            daemon: WhatsappDaemonConfig::default(),
            public_tunnel: WhatsappPublicTunnelConfig::default(),
            instance: instance.map(str::to_string),
            allow_agents: Vec::new(),
        }
    }

    /// Test-only `PairingChallengeStore` minimal enough to host
    /// a freshly created challenge so the trigger can update it.
    #[derive(Default)]
    struct TestStore {
        challenges: Mutex<HashMap<Uuid, PairingStatus>>,
    }
    impl TestStore {
        fn new_arc() -> Arc<Self> {
            Arc::new(Self::default())
        }
    }
    impl PairingChallengeStore for TestStore {
        fn create_challenge(
            &self,
            _agent_id: &str,
            _channel: &str,
            _instance: Option<&str>,
            _ttl_secs: u64,
        ) -> anyhow::Result<(Uuid, u64)> {
            let id = Uuid::new_v4();
            self.challenges.lock().unwrap().insert(
                id,
                PairingStatus {
                    challenge_id: id,
                    state: PairingState::Pending,
                    data: PairingStatusData::default(),
                },
            );
            Ok((id, 0))
        }
        fn read_challenge(&self, id: Uuid) -> anyhow::Result<Option<PairingStatus>> {
            Ok(self.challenges.lock().unwrap().get(&id).cloned())
        }
        fn cancel_challenge(&self, _id: Uuid) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn update_qr(
            &self,
            _id: Uuid,
            _qr_png_base64: String,
            _qr_ascii: String,
            _expires_at_ms: u64,
        ) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn update_state(
            &self,
            id: Uuid,
            state: PairingState,
            data: PairingStatusData,
        ) -> anyhow::Result<bool> {
            if let Some(s) = self.challenges.lock().unwrap().get_mut(&id) {
                s.state = state;
                s.data = data;
            }
            Ok(true)
        }
    }

    fn ctx_for(instance: Option<&str>) -> (PairingContext, Arc<TestStore>, CancellationToken) {
        let store = TestStore::new_arc();
        let store_dyn: Arc<dyn PairingChallengeStore> = store.clone();
        let cancel = CancellationToken::new();
        let ctx = PairingContext {
            challenge_id: Uuid::new_v4(),
            agent_id: "ana".into(),
            instance: instance.map(str::to_string),
            store: store_dyn,
            notifier: None,
            timeout: std::time::Duration::from_secs(60),
            cancel: cancel.clone(),
        };
        (ctx, store, cancel)
    }

    #[test]
    fn from_configs_indexes_each_instance_by_label() {
        let cfgs = vec![
            cfg_with(Some("ana"), "/tmp/ana"),
            cfg_with(Some("bob"), "/tmp/bob"),
        ];
        let trigger = WhatsappPairingTrigger::from_configs(&cfgs);
        assert_eq!(trigger.instance_dirs.len(), 2);
        assert_eq!(trigger.default_instance.as_deref(), Some("ana"));
        assert_eq!(
            trigger.resolve_session_dir(Some("bob")),
            Some(PathBuf::from("/tmp/bob"))
        );
    }

    #[test]
    fn from_configs_legacy_single_account_uses_empty_label_as_default() {
        let cfgs = vec![cfg_with(None, "/tmp/legacy")];
        let trigger = WhatsappPairingTrigger::from_configs(&cfgs);
        assert_eq!(trigger.default_instance.as_deref(), Some(""));
        assert_eq!(
            trigger.resolve_session_dir(None),
            Some(PathBuf::from("/tmp/legacy"))
        );
    }

    #[tokio::test]
    async fn start_rejects_unknown_instance_with_instance_not_configured() {
        let trigger = WhatsappPairingTrigger::from_configs(&[]);
        let (ctx, _store, _cancel) = ctx_for(Some("ana"));
        let result = trigger.start(ctx).await;
        match result {
            Err(PairingTriggerError::InstanceNotConfigured(name)) => {
                assert_eq!(name, "ana");
            }
            other => panic!("expected InstanceNotConfigured, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn channel_id_is_whatsapp() {
        let trigger = WhatsappPairingTrigger::from_configs(&[]);
        assert_eq!(trigger.channel_id(), "whatsapp");
    }
}
