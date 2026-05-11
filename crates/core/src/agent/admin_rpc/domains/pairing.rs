//! `nexo/admin/pairing/*` handlers + notification
//! shape.
//!
//! Async pairing flow abstracted via [`PairingChallengeStore`]
//! (challenge state) and [`PairingNotifier`] (push notifications
//! to the microapp). Production wires the existing
//! `crates/pairing/` SQLite session_store + a NATS-bridged
//! notifier.

use serde_json::Value;

use nexo_tool_meta::admin::pairing::{
    PairingCancelParams, PairingCancelResponse, PairingStartInput, PairingStartResponse,
    PairingState, PairingStatus, PairingStatusData, PairingStatusParams,
};
use uuid::Uuid;

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Notification topic the daemon emits on. SDK-side subscriber
/// matches against this exact string.
pub const PAIRING_STATUS_NOTIFY_METHOD: &str = "nexo/notify/pairing_status_changed";

/// Default challenge TTL — operators override at boot via
/// `pairing.yaml.<channel>.ttl_secs` (existing knob from Phase
/// 26). 5 minutes mirrors WhatsApp QR expiry.
pub const DEFAULT_CHALLENGE_TTL_SECS: u64 = 5 * 60;

/// Storage abstraction for in-flight pairing challenges. Production
/// adapter wraps `crates/pairing::session_store::SqliteSessionStore`.
pub trait PairingChallengeStore: Send + Sync {
    /// Create a new challenge keyed by a freshly generated
    /// `challenge_id`. Returns the id + epoch-ms expiry.
    fn create_challenge(
        &self,
        agent_id: &str,
        channel: &str,
        instance: Option<&str>,
        ttl_secs: u64,
    ) -> anyhow::Result<(Uuid, u64)>;
    /// Read the current state of a challenge. `None` when the
    /// id is unknown.
    fn read_challenge(&self, challenge_id: Uuid) -> anyhow::Result<Option<PairingStatus>>;
    /// Mark a challenge cancelled. Returns `false` when already
    /// terminal (linked / expired / cancelled) — idempotent.
    fn cancel_challenge(&self, challenge_id: Uuid) -> anyhow::Result<bool>;

    /// Replace the QR snapshot for an in-flight
    /// challenge. Called by `PairingChannelTrigger` impls on each
    /// onQr callback. Idempotent — overwriting an existing QR is
    /// the expected hot path (WhatsApp rotates pairing refs
    /// every ~20s).
    ///
    /// Implicit state transition: stores SHOULD flip
    /// `state` to [`PairingState::QrReady`] when called from
    /// `Pending`. Already-QrReady → re-QrReady is fine
    /// (just data swap).
    ///
    /// Returns `Ok(true)` when the challenge existed + was
    /// updated. Returns `Ok(false)` when the entry is already
    /// terminal (`Linked` / `Cancelled` / `Expired`) — trigger
    /// SHOULD honor by stopping its loop. `Ok(false)` is also
    /// returned for unknown ids (defensive — race against
    /// `cancel_challenge`).
    fn update_qr(
        &self,
        challenge_id: Uuid,
        qr_png_base64: String,
        qr_ascii: String,
        expires_at_ms: u64,
    ) -> anyhow::Result<bool>;

    /// Transition the state of an in-flight
    /// challenge. Triggers push `AwaitingUser` after delivering
    /// QR, `Linked` on confirmation. Transport failures are
    /// surfaced via `data.error` (the wire enum has no `Error`
    /// state today; UIs branch on `data.error.is_some()`).
    /// Terminal states (`Linked` / `Cancelled` / `Expired`)
    /// are sticky — subsequent calls return `Ok(false)`.
    ///
    /// `data` REPLACES the existing data field — callers should
    /// merge upstream (e.g. preserve `qr_png_base64` from the
    /// last `update_qr` if still relevant). `data` carries
    /// `device_jid` on `Linked`, `error` on `Error`, etc.
    fn update_state(
        &self,
        challenge_id: Uuid,
        state: PairingState,
        data: PairingStatusData,
    ) -> anyhow::Result<bool>;
}

/// Notification sender — pushes
/// `nexo/notify/pairing_status_changed` frames to the microapp
/// stdio. Production wires a writer that shares the same stdout
/// as `tools/call` responses.
pub trait PairingNotifier: Send + Sync {
    /// Push one status frame. Errors are logged-only; the daemon
    /// never blocks on notification delivery.
    fn notify_status(&self, status: &PairingStatus);
}

/// `nexo/admin/pairing/start` — register a new challenge,
/// trigger the channel plugin's pairing flow, return the
/// `challenge_id` for subsequent polls.
pub fn start(store: &dyn PairingChallengeStore, params: Value) -> AdminRpcResult {
    let input: PairingStartInput = match serde_json::from_value(params) {
        Ok(i) => i,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };

    if input.agent_id.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("agent_id is empty".into()));
    }
    if input.channel.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("channel is empty".into()));
    }

    let (challenge_id, expires_at_ms) = match store.create_challenge(
        &input.agent_id,
        &input.channel,
        input.instance.as_deref(),
        DEFAULT_CHALLENGE_TTL_SECS,
    ) {
        Ok(v) => v,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!("create_challenge: {e}")));
        }
    };

    let response = PairingStartResponse {
        challenge_id,
        expires_at_ms,
        instructions: pairing_instructions_for(&input.channel),
    };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/pairing/status` — return current state of a
/// challenge.
pub fn status(store: &dyn PairingChallengeStore, params: Value) -> AdminRpcResult {
    let p: PairingStatusParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    match store.read_challenge(p.challenge_id) {
        Ok(Some(status)) => AdminRpcResult::ok(serde_json::to_value(status).unwrap_or(Value::Null)),
        Ok(None) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "not_found: challenge `{}` unknown",
            p.challenge_id
        ))),
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!("read_challenge: {e}"))),
    }
}

/// Async wrapper that performs the trigger
/// lookup + spawn AFTER the legacy `start` path created the
/// challenge. Production dispatcher routes here. The stale
/// `start` (above) is kept for unit tests that don't care
/// about the trigger map.
pub async fn start_with_trigger(
    store: std::sync::Arc<dyn PairingChallengeStore>,
    notifier: Option<std::sync::Arc<dyn PairingNotifier>>,
    triggers: &super::super::pairing_trigger::PairingChannelTriggers,
    handles: std::sync::Arc<dashmap::DashMap<Uuid, super::super::pairing_trigger::PairingHandle>>,
    cancel_root: &tokio_util::sync::CancellationToken,
    params: Value,
) -> AdminRpcResult {
    use super::super::pairing_trigger::{
        PairingContext, PairingTriggerError, PAIRING_DEFAULT_TIMEOUT,
    };

    let input: PairingStartInput = match serde_json::from_value(params) {
        Ok(i) => i,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if input.agent_id.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("agent_id is empty".into()));
    }
    if input.channel.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("channel is empty".into()));
    }

    // Fail-fast if no trigger registered. No
    // garbage row in store; operator gets a clean error.
    let Some(trigger) = triggers.get(&input.channel) else {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
            "channel `{}` not supported",
            input.channel
        )));
    };

    let (challenge_id, expires_at_ms) = match store.create_challenge(
        &input.agent_id,
        &input.channel,
        input.instance.as_deref(),
        DEFAULT_CHALLENGE_TTL_SECS,
    ) {
        Ok(v) => v,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!("create_challenge: {e}")));
        }
    };

    let cancel = cancel_root.child_token();
    let ctx = PairingContext {
        challenge_id,
        agent_id: input.agent_id.clone(),
        instance: input.instance.clone(),
        store: store.clone(),
        notifier: notifier.clone(),
        timeout: PAIRING_DEFAULT_TIMEOUT,
        cancel: cancel.clone(),
    };

    match trigger.start(ctx).await {
        Ok(handle) => {
            handles.insert(challenge_id, handle);
            let response = PairingStartResponse {
                challenge_id,
                expires_at_ms,
                instructions: pairing_instructions_for(&input.channel),
            };
            AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
        }
        Err(e) => {
            // Roll back the row — trigger refused before spawn.
            let _ = store.cancel_challenge(challenge_id);
            let admin_err = match e {
                PairingTriggerError::ChannelNotSupported(c) => {
                    AdminRpcError::InvalidParams(format!("channel `{c}` not supported"))
                }
                PairingTriggerError::AlreadyPaired(i) => {
                    AdminRpcError::InvalidParams(format!("instance `{i}` already paired"))
                }
                PairingTriggerError::InstanceNotConfigured(i) => {
                    AdminRpcError::InvalidParams(format!("instance `{i}` not configured"))
                }
                PairingTriggerError::Transport(m) => {
                    AdminRpcError::Internal(format!("transport: {m}"))
                }
                PairingTriggerError::Internal(err) => AdminRpcError::Internal(err.to_string()),
            };
            AdminRpcResult::err(admin_err)
        }
    }
}

/// `nexo/admin/pairing/cancel` — abort a pending challenge.
pub fn cancel(
    store: &dyn PairingChallengeStore,
    notifier: Option<&dyn PairingNotifier>,
    params: Value,
) -> AdminRpcResult {
    let p: PairingCancelParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    let cancelled = match store.cancel_challenge(p.challenge_id) {
        Ok(c) => c,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!("cancel_challenge: {e}")));
        }
    };

    // Push a final `cancelled` notification when the cancel
    // actually changed state — mirrors what the daemon would
    // emit if the challenge had been cancelled by another path.
    if cancelled {
        if let Some(n) = notifier {
            n.notify_status(&PairingStatus {
                challenge_id: p.challenge_id,
                state: PairingState::Cancelled,
                data: PairingStatusData::default(),
            });
        }
    }

    AdminRpcResult::ok(
        serde_json::to_value(PairingCancelResponse { cancelled }).unwrap_or(Value::Null),
    )
}

/// Wrapper that aborts the spawned trigger
/// task BEFORE flipping the store entry. Production dispatcher
/// routes here. Same semantics as `cancel` for callers that
/// have no trigger handles to abort.
pub fn cancel_with_handles(
    store: &dyn PairingChallengeStore,
    notifier: Option<&dyn PairingNotifier>,
    handles: &dashmap::DashMap<Uuid, super::super::pairing_trigger::PairingHandle>,
    params: Value,
) -> AdminRpcResult {
    let p: PairingCancelParams = match serde_json::from_value(params.clone()) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    // Abort the in-flight task first so the trigger stops
    // pushing updates while we mutate the store.
    if let Some((_, handle)) = handles.remove(&p.challenge_id) {
        handle.abort();
    }
    cancel(store, notifier, params)
}

/// Instruction copy per channel. Operator UIs render verbatim.
fn pairing_instructions_for(channel: &str) -> String {
    match channel {
        "whatsapp" => "Open WhatsApp on your phone → Settings → Linked Devices → Link a Device → \
             scan the QR code shown in the next status update."
            .into(),
        // Channel-agnostic fallback for future telegram / email /
        // custom channels — operator UI customises by channel id.
        other => format!(
            "Pairing started for channel `{other}`. Watch for status updates with the \
             channel-specific artifact (QR / link / token)."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Test-only `PairingChallengeStore`. Stores active challenges
    /// keyed by id; supports flipping state via `set_state` to
    /// simulate plugin progress.
    #[derive(Default)]
    struct MockStore {
        challenges: Mutex<std::collections::HashMap<Uuid, PairingStatus>>,
        next_id_counter: AtomicU64,
        next_expires: AtomicU64,
    }

    impl MockStore {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        fn set_state(&self, id: Uuid, state: PairingState, data: PairingStatusData) {
            self.challenges.lock().unwrap().insert(
                id,
                PairingStatus {
                    challenge_id: id,
                    state,
                    data,
                },
            );
        }
    }

    impl PairingChallengeStore for MockStore {
        fn create_challenge(
            &self,
            _agent_id: &str,
            _channel: &str,
            _instance: Option<&str>,
            ttl_secs: u64,
        ) -> anyhow::Result<(Uuid, u64)> {
            // Deterministic ids for test assertions.
            let n = self.next_id_counter.fetch_add(1, Ordering::Relaxed);
            let id = Uuid::from_u128(0xC0DE_0000_0000_0000_0000_0000_0000_0000 + n as u128);
            let expires = self.next_expires.fetch_add(0, Ordering::Relaxed) + ttl_secs * 1000;
            self.challenges.lock().unwrap().insert(
                id,
                PairingStatus {
                    challenge_id: id,
                    state: PairingState::Pending,
                    data: PairingStatusData::default(),
                },
            );
            Ok((id, expires))
        }
        fn read_challenge(&self, challenge_id: Uuid) -> anyhow::Result<Option<PairingStatus>> {
            Ok(self.challenges.lock().unwrap().get(&challenge_id).cloned())
        }
        fn cancel_challenge(&self, challenge_id: Uuid) -> anyhow::Result<bool> {
            let mut map = self.challenges.lock().unwrap();
            let Some(current) = map.get_mut(&challenge_id) else {
                return Ok(false);
            };
            if matches!(
                current.state,
                PairingState::Linked | PairingState::Expired | PairingState::Cancelled
            ) {
                return Ok(false);
            }
            current.state = PairingState::Cancelled;
            current.data = PairingStatusData::default();
            Ok(true)
        }
        fn update_qr(
            &self,
            challenge_id: Uuid,
            qr_png_base64: String,
            qr_ascii: String,
            _expires_at_ms: u64,
        ) -> anyhow::Result<bool> {
            let mut map = self.challenges.lock().unwrap();
            let Some(current) = map.get_mut(&challenge_id) else {
                return Ok(false);
            };
            if matches!(
                current.state,
                PairingState::Linked | PairingState::Expired | PairingState::Cancelled
            ) {
                return Ok(false);
            }
            current.state = PairingState::QrReady;
            current.data.qr_png_base64 = Some(qr_png_base64);
            current.data.qr_ascii = Some(qr_ascii);
            Ok(true)
        }
        fn update_state(
            &self,
            challenge_id: Uuid,
            state: PairingState,
            data: PairingStatusData,
        ) -> anyhow::Result<bool> {
            let mut map = self.challenges.lock().unwrap();
            let Some(current) = map.get_mut(&challenge_id) else {
                return Ok(false);
            };
            if matches!(
                current.state,
                PairingState::Linked | PairingState::Expired | PairingState::Cancelled
            ) {
                return Ok(false);
            }
            current.state = state;
            current.data = data;
            Ok(true)
        }
    }

    /// In-memory notifier — captures pushed statuses for assertion.
    #[derive(Default)]
    struct MockNotifier {
        pushed: Mutex<Vec<PairingStatus>>,
        push_count: AtomicUsize,
    }

    impl PairingNotifier for MockNotifier {
        fn notify_status(&self, status: &PairingStatus) {
            self.pushed.lock().unwrap().push(status.clone());
            self.push_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn whatsapp_start_params(agent: &str) -> Value {
        serde_json::json!({
            "agent_id": agent,
            "channel": "whatsapp",
            "instance": "personal"
        })
    }

    #[test]
    fn pairing_start_creates_challenge_and_returns_id() {
        let store = MockStore::new();
        let result = start(&*store, whatsapp_start_params("ana"));
        let response: PairingStartResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.expires_at_ms > 0);
        assert!(response.instructions.contains("WhatsApp"));
        // State immediately persisted.
        let read = store
            .read_challenge(response.challenge_id)
            .unwrap()
            .unwrap();
        assert_eq!(read.state, PairingState::Pending);
    }

    #[test]
    fn pairing_start_rejects_empty_agent_id() {
        let store = MockStore::new();
        let result = start(
            &*store,
            serde_json::json!({ "agent_id": "", "channel": "whatsapp" }),
        );
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn pairing_start_rejects_empty_channel() {
        let store = MockStore::new();
        let result = start(
            &*store,
            serde_json::json!({ "agent_id": "ana", "channel": "" }),
        );
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn pairing_status_returns_current_state() {
        let store = MockStore::new();
        // Allocate a challenge then flip it to qr_ready.
        let start_result = start(&*store, whatsapp_start_params("ana"));
        let response: PairingStartResponse =
            serde_json::from_value(start_result.result.unwrap()).unwrap();
        store.set_state(
            response.challenge_id,
            PairingState::QrReady,
            PairingStatusData {
                qr_ascii: Some("##".into()),
                ..Default::default()
            },
        );

        let status_result = status(
            &*store,
            serde_json::json!({ "challenge_id": response.challenge_id }),
        );
        let status: PairingStatus = serde_json::from_value(status_result.result.unwrap()).unwrap();
        assert_eq!(status.state, PairingState::QrReady);
        assert_eq!(status.data.qr_ascii.as_deref(), Some("##"));
    }

    #[test]
    fn pairing_status_unknown_id_returns_not_found() {
        let store = MockStore::new();
        let result = status(&*store, serde_json::json!({ "challenge_id": Uuid::nil() }));
        let err = result.error.expect("error");
        match err {
            AdminRpcError::Internal(m) => assert!(m.contains("not_found")),
            other => panic!("expected Internal/not_found, got {other:?}"),
        }
    }

    #[test]
    fn pairing_cancel_idempotent_on_unknown_id() {
        let store = MockStore::new();
        let notifier = Arc::new(MockNotifier::default());
        let result = cancel(
            &*store,
            Some(&*notifier),
            serde_json::json!({ "challenge_id": Uuid::nil() }),
        );
        let response: PairingCancelResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(!response.cancelled);
        // No notification emitted for no-op cancel.
        assert_eq!(notifier.push_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn pairing_cancel_pushes_cancelled_notification_when_cancellable() {
        let store = MockStore::new();
        let notifier = Arc::new(MockNotifier::default());

        let start_result = start(&*store, whatsapp_start_params("ana"));
        let response: PairingStartResponse =
            serde_json::from_value(start_result.result.unwrap()).unwrap();

        let cancel_result = cancel(
            &*store,
            Some(&*notifier),
            serde_json::json!({ "challenge_id": response.challenge_id }),
        );
        let cancel_response: PairingCancelResponse =
            serde_json::from_value(cancel_result.result.unwrap()).unwrap();
        assert!(cancel_response.cancelled);

        // Notification emitted with cancelled state.
        let pushed = notifier.pushed.lock().unwrap();
        assert_eq!(pushed.len(), 1);
        assert_eq!(pushed[0].state, PairingState::Cancelled);
        assert_eq!(pushed[0].challenge_id, response.challenge_id);
    }

    #[test]
    fn pairing_cancel_already_terminal_is_idempotent_no_notification() {
        let store = MockStore::new();
        let notifier = Arc::new(MockNotifier::default());
        let start_result = start(&*store, whatsapp_start_params("ana"));
        let response: PairingStartResponse =
            serde_json::from_value(start_result.result.unwrap()).unwrap();
        // Mark the challenge as already linked.
        store.set_state(
            response.challenge_id,
            PairingState::Linked,
            PairingStatusData {
                device_jid: Some("wa.42".into()),
                ..Default::default()
            },
        );

        let result = cancel(
            &*store,
            Some(&*notifier),
            serde_json::json!({ "challenge_id": response.challenge_id }),
        );
        let response: PairingCancelResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(!response.cancelled);
        assert_eq!(notifier.push_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn pairing_notify_method_constant() {
        assert_eq!(
            PAIRING_STATUS_NOTIFY_METHOD,
            "nexo/notify/pairing_status_changed"
        );
    }

    // ── start_with_trigger / cancel_with_handles ──

    use crate::agent::admin_rpc::pairing_trigger::{
        PairingChannelTrigger, PairingChannelTriggers, PairingContext, PairingHandle,
        PairingTriggerError,
    };
    use async_trait::async_trait;
    use dashmap::DashMap;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use tokio_util::sync::CancellationToken;

    /// Test trigger that records every `start` invocation and
    /// returns either a happy handle or an error per fixture.
    #[derive(Debug)]
    struct MockTrigger {
        channel: String,
        called: AtomicUsize,
        error: Option<PairingTriggerError>,
        last_cancel: Mutex<Option<CancellationToken>>,
    }

    impl MockTrigger {
        fn happy(channel: &str) -> Arc<Self> {
            Arc::new(Self {
                channel: channel.into(),
                called: AtomicUsize::new(0),
                error: None,
                last_cancel: Mutex::new(None),
            })
        }
        fn rejects(channel: &str, err: PairingTriggerError) -> Arc<Self> {
            Arc::new(Self {
                channel: channel.into(),
                called: AtomicUsize::new(0),
                error: Some(err),
                last_cancel: Mutex::new(None),
            })
        }
    }

    #[async_trait]
    impl PairingChannelTrigger for MockTrigger {
        fn channel_id(&self) -> &str {
            &self.channel
        }
        async fn start(&self, ctx: PairingContext) -> Result<PairingHandle, PairingTriggerError> {
            self.called.fetch_add(1, Ordering::Relaxed);
            if let Some(ref e) = self.error {
                return Err(match e {
                    PairingTriggerError::ChannelNotSupported(s) => {
                        PairingTriggerError::ChannelNotSupported(s.clone())
                    }
                    PairingTriggerError::AlreadyPaired(s) => {
                        PairingTriggerError::AlreadyPaired(s.clone())
                    }
                    PairingTriggerError::InstanceNotConfigured(s) => {
                        PairingTriggerError::InstanceNotConfigured(s.clone())
                    }
                    PairingTriggerError::Transport(s) => PairingTriggerError::Transport(s.clone()),
                    PairingTriggerError::Internal(_) => {
                        PairingTriggerError::Transport("test internal".into())
                    }
                });
            }
            *self.last_cancel.lock().unwrap() = Some(ctx.cancel.clone());
            Ok(PairingHandle {
                challenge_id: ctx.challenge_id,
                channel: self.channel.clone(),
                cancel: ctx.cancel,
            })
        }
    }

    fn empty_handles() -> Arc<DashMap<Uuid, PairingHandle>> {
        Arc::new(DashMap::new())
    }

    #[tokio::test]
    async fn start_with_trigger_inserts_handle_on_happy_path() {
        let store: Arc<dyn PairingChallengeStore> = MockStore::new();
        let mut triggers: PairingChannelTriggers = HashMap::new();
        let trigger = MockTrigger::happy("whatsapp");
        triggers.insert("whatsapp".into(), trigger.clone());
        let handles = empty_handles();
        let root = CancellationToken::new();
        let result = start_with_trigger(
            store.clone(),
            None,
            &triggers,
            handles.clone(),
            &root,
            whatsapp_start_params("ana"),
        )
        .await;
        assert!(
            result.error.is_none(),
            "expected ok, got {:?}",
            result.error
        );
        assert_eq!(trigger.called.load(Ordering::Relaxed), 1);
        assert_eq!(handles.len(), 1, "handle must be registered");
    }

    #[tokio::test]
    async fn start_with_trigger_unknown_channel_returns_invalid_params_no_row() {
        let store = MockStore::new();
        let store_dyn: Arc<dyn PairingChallengeStore> = store.clone();
        let triggers: PairingChannelTriggers = HashMap::new();
        let handles = empty_handles();
        let root = CancellationToken::new();
        let result = start_with_trigger(
            store_dyn,
            None,
            &triggers,
            handles.clone(),
            &root,
            whatsapp_start_params("ana"),
        )
        .await;
        assert!(matches!(
            result.error,
            Some(AdminRpcError::InvalidParams(_))
        ));
        assert_eq!(
            store.challenges.lock().unwrap().len(),
            0,
            "no row must be created when channel unsupported"
        );
        assert_eq!(handles.len(), 0);
    }

    #[tokio::test]
    async fn start_with_trigger_rolls_back_row_when_trigger_rejects() {
        let store = MockStore::new();
        let store_dyn: Arc<dyn PairingChallengeStore> = store.clone();
        let mut triggers: PairingChannelTriggers = HashMap::new();
        let trigger =
            MockTrigger::rejects("whatsapp", PairingTriggerError::AlreadyPaired("ana".into()));
        triggers.insert("whatsapp".into(), trigger.clone());
        let handles = empty_handles();
        let root = CancellationToken::new();
        let result = start_with_trigger(
            store_dyn,
            None,
            &triggers,
            handles.clone(),
            &root,
            whatsapp_start_params("ana"),
        )
        .await;
        assert!(matches!(
            result.error,
            Some(AdminRpcError::InvalidParams(_))
        ));
        assert_eq!(trigger.called.load(Ordering::Relaxed), 1);
        assert_eq!(handles.len(), 0, "handle must NOT be registered on reject");
        // Row was created but rolled back to Cancelled.
        let row = store.challenges.lock().unwrap().values().next().cloned();
        assert!(
            matches!(row.map(|r| r.state), Some(PairingState::Cancelled)),
            "store row must be cancelled after rollback",
        );
    }

    #[tokio::test]
    async fn cancel_with_handles_aborts_trigger_then_cancels_store() {
        let store: Arc<dyn PairingChallengeStore> = MockStore::new();
        let mut triggers: PairingChannelTriggers = HashMap::new();
        let trigger = MockTrigger::happy("whatsapp");
        triggers.insert("whatsapp".into(), trigger.clone());
        let handles = empty_handles();
        let root = CancellationToken::new();
        // Start a pairing so a handle exists.
        let _ = start_with_trigger(
            store.clone(),
            None,
            &triggers,
            handles.clone(),
            &root,
            whatsapp_start_params("ana"),
        )
        .await;
        let challenge_id = *handles.iter().next().unwrap().key();
        // Pull the trigger's stored token so we can verify it
        // gets cancelled by cancel_with_handles.
        let observed_cancel = trigger.last_cancel.lock().unwrap().clone().unwrap();
        assert!(!observed_cancel.is_cancelled());

        let cancel_params = serde_json::to_value(PairingCancelParams { challenge_id }).unwrap();
        let result = cancel_with_handles(store.as_ref(), None, handles.as_ref(), cancel_params);
        assert!(result.error.is_none());
        assert!(
            observed_cancel.is_cancelled(),
            "trigger token must be cancelled"
        );
        assert_eq!(handles.len(), 0, "handle entry must be removed");
    }

    /// Defensive: `cancel_with_handles` MUST still flip the
    /// store entry to Cancelled even when no handle was
    /// registered (e.g. challenge id unknown to the trigger
    /// registry — operator cancels something that was never
    /// paired through the trigger path).
    #[tokio::test]
    async fn cancel_with_handles_handles_missing_handle_gracefully() {
        let store: Arc<dyn PairingChallengeStore> = MockStore::new();
        let (id, _) = store.create_challenge("ana", "whatsapp", None, 60).unwrap();
        let handles = empty_handles();
        let cancel_params = serde_json::to_value(PairingCancelParams { challenge_id: id }).unwrap();
        let result = cancel_with_handles(store.as_ref(), None, handles.as_ref(), cancel_params);
        assert!(result.error.is_none());
        let status = store.read_challenge(id).unwrap().unwrap();
        assert_eq!(status.state, PairingState::Cancelled);
    }

    // Silence unused-warning when the module hits an
    // intermediate state where an `AtomicBool` import isn't
    // referenced by an active test.
    #[allow(dead_code)]
    fn _touch_atomic_bool(_: AtomicBool) {}
}
