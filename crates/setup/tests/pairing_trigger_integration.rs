//! Integration test — wire a `MockTrigger`
//! through the dispatcher's `PairingChannelTriggers` map and
//! verify a `nexo/admin/pairing/start` end-to-end:
//!
//! 1. Trigger receives the call (channel lookup works).
//! 2. Store is mutated (challenge inserted + transitions).
//! 3. Handle is registered (`pairing_handles_len() == 1`).
//! 4. Cancel routes through the handle's CancellationToken.
//! 5. Unsupported channels short-circuit with no row + no handle.
//!
//! The wire-shape unit tests (in `crates/core`) cover the
//! dispatcher branch logic; this file proves the production
//! `with_pairing_triggers` builder + dispatcher async path
//! compose correctly when called via the documented public
//! `dispatch(microapp_id, method, params)` entry point.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nexo_core::agent::admin_rpc::pairing_trigger::{
    PairingChannelTrigger, PairingChannelTriggers, PairingContext, PairingHandle,
    PairingTriggerError,
};
use nexo_core::agent::admin_rpc::{AdminRpcDispatcher, CapabilitySet};
use nexo_setup::admin_adapters::{DeferredPairingNotifier, InMemoryPairingChallengeStore};
use nexo_tool_meta::admin::pairing::{PairingState, PairingStatusParams};
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn grants(microapp: &str, caps: &[&str]) -> Arc<CapabilitySet> {
    let mut g: HashMap<String, HashSet<String>> = HashMap::new();
    g.insert(
        microapp.into(),
        caps.iter().map(|c| c.to_string()).collect(),
    );
    CapabilitySet::from_grants(g)
}

/// Test-only trigger that records every `start` invocation,
/// captures the cancel token, and returns either a happy
/// handle or a configured error per fixture.
#[derive(Debug)]
struct MockTrigger {
    channel: String,
    called: AtomicUsize,
    error: Option<&'static str>,
    captured_cancel: Mutex<Option<CancellationToken>>,
}

impl MockTrigger {
    fn happy(channel: &str) -> Arc<Self> {
        Arc::new(Self {
            channel: channel.into(),
            called: AtomicUsize::new(0),
            error: None,
            captured_cancel: Mutex::new(None),
        })
    }
    fn rejects(channel: &str, err_kind: &'static str) -> Arc<Self> {
        Arc::new(Self {
            channel: channel.into(),
            called: AtomicUsize::new(0),
            error: Some(err_kind),
            captured_cancel: Mutex::new(None),
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
        if let Some(kind) = self.error {
            return Err(match kind {
                "already_paired" => PairingTriggerError::AlreadyPaired("test".into()),
                _ => PairingTriggerError::Transport("simulated".into()),
            });
        }
        // Capture the cancel token so the test can assert it
        // gets cancelled when the operator hits pairing/cancel.
        *self.captured_cancel.lock().unwrap() = Some(ctx.cancel.clone());
        Ok(PairingHandle {
            challenge_id: ctx.challenge_id,
            channel: self.channel.clone(),
            cancel: ctx.cancel,
        })
    }
}

fn build_dispatcher(triggers: PairingChannelTriggers) -> AdminRpcDispatcher {
    let store = Arc::new(InMemoryPairingChallengeStore::new(Duration::from_secs(60)));
    let notifier = Arc::new(DeferredPairingNotifier::new());
    AdminRpcDispatcher::new()
        .with_capabilities(grants("test_app", &["pairing_initiate"]))
        .with_pairing_domain(store, Some(notifier))
        .with_pairing_triggers(triggers)
}

#[tokio::test]
async fn pairing_start_registers_handle_when_trigger_succeeds() {
    let trigger = MockTrigger::happy("whatsapp");
    let mut triggers: PairingChannelTriggers = HashMap::new();
    triggers.insert("whatsapp".into(), trigger.clone());
    let dispatcher = build_dispatcher(triggers);

    let result = dispatcher
        .dispatch(
            "test_app",
            "nexo/admin/pairing/start",
            json!({"agent_id": "ana", "channel": "whatsapp"}),
        )
        .await;
    assert!(
        result.error.is_none(),
        "expected ok, got error: {:?}",
        result.error
    );
    assert_eq!(trigger.called.load(Ordering::Relaxed), 1);
    assert_eq!(
        dispatcher.pairing_handles_len(),
        1,
        "handle must be registered after successful start",
    );
}

#[tokio::test]
async fn pairing_start_unsupported_channel_returns_invalid_params_no_handle() {
    let triggers: PairingChannelTriggers = HashMap::new();
    let dispatcher = build_dispatcher(triggers);

    let result = dispatcher
        .dispatch(
            "test_app",
            "nexo/admin/pairing/start",
            json!({"agent_id": "ana", "channel": "telegram"}),
        )
        .await;
    assert!(
        result.error.is_some(),
        "expected invalid_params for unsupported channel"
    );
    assert_eq!(
        dispatcher.pairing_handles_len(),
        0,
        "no handle on unsupported channel"
    );
}

#[tokio::test]
async fn pairing_start_rolls_back_when_trigger_rejects() {
    let trigger = MockTrigger::rejects("whatsapp", "already_paired");
    let mut triggers: PairingChannelTriggers = HashMap::new();
    triggers.insert("whatsapp".into(), trigger.clone());
    let dispatcher = build_dispatcher(triggers);

    let result = dispatcher
        .dispatch(
            "test_app",
            "nexo/admin/pairing/start",
            json!({"agent_id": "ana", "channel": "whatsapp"}),
        )
        .await;
    assert!(result.error.is_some());
    assert_eq!(trigger.called.load(Ordering::Relaxed), 1);
    assert_eq!(
        dispatcher.pairing_handles_len(),
        0,
        "rejected trigger must NOT leave a registered handle",
    );
}

#[tokio::test]
async fn pairing_cancel_aborts_handle_and_flips_store_to_cancelled() {
    let trigger = MockTrigger::happy("whatsapp");
    let mut triggers: PairingChannelTriggers = HashMap::new();
    triggers.insert("whatsapp".into(), trigger.clone());
    let dispatcher = build_dispatcher(triggers);

    // Start a pairing.
    let start_result = dispatcher
        .dispatch(
            "test_app",
            "nexo/admin/pairing/start",
            json!({"agent_id": "ana", "channel": "whatsapp"}),
        )
        .await;
    let response = start_result
        .result
        .expect("start ok")
        .as_object()
        .cloned()
        .unwrap();
    let challenge_id: Uuid =
        serde_json::from_value(response.get("challenge_id").unwrap().clone()).unwrap();
    assert_eq!(dispatcher.pairing_handles_len(), 1);

    // Capture the cancel token from the trigger before the dispatcher
    // cancels it via cancel_with_handles.
    let observed_cancel = trigger
        .captured_cancel
        .lock()
        .unwrap()
        .clone()
        .expect("trigger captured cancel");
    assert!(!observed_cancel.is_cancelled());

    // Cancel.
    let cancel_result = dispatcher
        .dispatch(
            "test_app",
            "nexo/admin/pairing/cancel",
            serde_json::to_value(PairingCancelParamsLocal { challenge_id }).unwrap(),
        )
        .await;
    assert!(cancel_result.error.is_none());
    assert!(
        observed_cancel.is_cancelled(),
        "trigger task token must be cancelled by pairing/cancel"
    );
    assert_eq!(
        dispatcher.pairing_handles_len(),
        0,
        "handle map must be empty after cancel"
    );

    // Status reflects Cancelled.
    let status = dispatcher
        .dispatch(
            "test_app",
            "nexo/admin/pairing/status",
            serde_json::to_value(PairingStatusParams { challenge_id }).unwrap(),
        )
        .await;
    let status_value = status.result.expect("status ok");
    let state_str = status_value.get("state").and_then(|v| v.as_str()).unwrap();
    assert_eq!(
        state_str,
        format!("{:?}", PairingState::Cancelled).to_lowercase()
    );
}

/// Minimal local mirror of `PairingCancelParams` with only the
/// fields the test exercises. Avoids pulling in the full
/// upstream type when serializing the wire payload.
#[derive(serde::Serialize)]
struct PairingCancelParamsLocal {
    challenge_id: Uuid,
}
