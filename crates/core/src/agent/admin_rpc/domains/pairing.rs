//! Phase 82.10.e — `nexo/admin/pairing/*` handlers + notification
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
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "create_challenge: {e}"
            )));
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
        Ok(Some(status)) => {
            AdminRpcResult::ok(serde_json::to_value(status).unwrap_or(Value::Null))
        }
        Ok(None) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "not_found: challenge `{}` unknown",
            p.challenge_id
        ))),
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "read_challenge: {e}"
        ))),
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
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "cancel_challenge: {e}"
            )));
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
        serde_json::to_value(PairingCancelResponse { cancelled })
            .unwrap_or(Value::Null),
    )
}

/// Instruction copy per channel. Operator UIs render verbatim.
fn pairing_instructions_for(channel: &str) -> String {
    match channel {
        "whatsapp" => {
            "Open WhatsApp on your phone → Settings → Linked Devices → Link a Device → \
             scan the QR code shown in the next status update."
                .into()
        }
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
        fn read_challenge(
            &self,
            challenge_id: Uuid,
        ) -> anyhow::Result<Option<PairingStatus>> {
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
        let read = store.read_challenge(response.challenge_id).unwrap().unwrap();
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
        let status: PairingStatus =
            serde_json::from_value(status_result.result.unwrap()).unwrap();
        assert_eq!(status.state, PairingState::QrReady);
        assert_eq!(status.data.qr_ascii.as_deref(), Some("##"));
    }

    #[test]
    fn pairing_status_unknown_id_returns_not_found() {
        let store = MockStore::new();
        let result = status(
            &*store,
            serde_json::json!({ "challenge_id": Uuid::nil() }),
        );
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
}
