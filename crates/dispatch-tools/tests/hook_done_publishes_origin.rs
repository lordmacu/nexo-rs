//! Phase 67.F.1 — completion hook fires NotifyOrigin via the
//! pairing adapter registered for the origin's plugin id, plus
//! failure isolation between hooks in a list.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nexo_dispatch_tools::hooks::dispatcher::{NatsHookPublisher, NoopNatsHookPublisher};
use nexo_dispatch_tools::{
    CompletionHook, DefaultHookDispatcher, HookAction, HookDispatcher, HookError, HookPayload,
    HookTransition, HookTrigger,
};
use nexo_driver_claude::OriginChannel;
use nexo_driver_types::GoalId;
use nexo_pairing::{PairingAdapterRegistry, PairingChannelAdapter};
use uuid::Uuid;

#[derive(Default)]
struct CapturingAdapter {
    sent: Mutex<Vec<(String, String, String)>>,
}

#[async_trait]
impl PairingChannelAdapter for CapturingAdapter {
    fn channel_id(&self) -> &'static str {
        "telegram"
    }
    fn normalize_sender(&self, raw: &str) -> Option<String> {
        Some(raw.to_string())
    }
    async fn send_reply(&self, account: &str, to: &str, text: &str) -> anyhow::Result<()> {
        self.sent
            .lock()
            .unwrap()
            .push((account.into(), to.into(), text.into()));
        Ok(())
    }
}

#[derive(Default)]
struct FailingAdapter;

#[async_trait]
impl PairingChannelAdapter for FailingAdapter {
    fn channel_id(&self) -> &'static str {
        "whatsapp"
    }
    fn normalize_sender(&self, raw: &str) -> Option<String> {
        Some(raw.to_string())
    }
    async fn send_reply(&self, _account: &str, _to: &str, _text: &str) -> anyhow::Result<()> {
        anyhow::bail!("simulated adapter failure")
    }
}

#[derive(Default)]
struct CapturingNats {
    sent: Mutex<Vec<(String, Vec<u8>)>>,
}

#[async_trait]
impl NatsHookPublisher for CapturingNats {
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), String> {
        self.sent
            .lock()
            .unwrap()
            .push((subject.into(), payload.to_vec()));
        Ok(())
    }
}

fn payload(transition: HookTransition) -> HookPayload {
    HookPayload {
        goal_id: GoalId(Uuid::new_v4()),
        phase_id: "67.10".into(),
        transition,
        summary: "12 turns ok".into(),
        elapsed: "23m".into(),
        diff_stat: Some("+340/-89".into()),
        origin: Some(OriginChannel {
            plugin: "telegram".into(),
            instance: "family".into(),
            sender_id: "@cris".into(),
            correlation_id: Some("msg-7".into()),
        }),
    }
}

#[tokio::test]
async fn notify_origin_uses_registered_adapter() {
    let pairing = PairingAdapterRegistry::new();
    let cap = Arc::new(CapturingAdapter::default());
    pairing.register(cap.clone() as Arc<dyn PairingChannelAdapter>);

    let nats: Arc<dyn NatsHookPublisher> = Arc::new(NoopNatsHookPublisher);
    let dispatcher = DefaultHookDispatcher::new(pairing, nats);

    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::NotifyOrigin,
    };
    dispatcher
        .dispatch(&hook, &payload(HookTransition::Done))
        .await
        .unwrap();

    let sent = cap.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "family");
    assert_eq!(sent[0].1, "@cris");
    assert!(sent[0].2.contains("67.10"));
    assert!(sent[0].2.contains("✅"));
    assert!(sent[0].2.contains("+340/-89"));
}

#[tokio::test]
async fn notify_origin_returns_missing_origin_when_unset() {
    let pairing = PairingAdapterRegistry::new();
    let dispatcher = DefaultHookDispatcher::new(pairing, Arc::new(NoopNatsHookPublisher));
    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::NotifyOrigin,
    };
    let mut p = payload(HookTransition::Done);
    p.origin = None;
    let err = dispatcher.dispatch(&hook, &p).await.unwrap_err();
    assert_eq!(err, HookError::MissingOrigin);
}

#[tokio::test]
async fn console_origin_is_a_noop_for_notify_origin() {
    let pairing = PairingAdapterRegistry::new();
    let dispatcher = DefaultHookDispatcher::new(pairing, Arc::new(NoopNatsHookPublisher));
    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::NotifyOrigin,
    };
    let mut p = payload(HookTransition::Done);
    p.origin = Some(OriginChannel {
        plugin: "console".into(),
        instance: "host-a".into(),
        sender_id: "uid:1000".into(),
        correlation_id: None,
    });
    dispatcher.dispatch(&hook, &p).await.unwrap();
}

#[tokio::test]
async fn nats_publish_action_sends_payload_json() {
    let pairing = PairingAdapterRegistry::new();
    let nats = Arc::new(CapturingNats::default());
    let nats_dyn: Arc<dyn NatsHookPublisher> = nats.clone();
    let dispatcher = DefaultHookDispatcher::new(pairing, nats_dyn);

    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::NatsPublish {
            subject: "agent.driver.hook.test".into(),
        },
    };
    dispatcher
        .dispatch(&hook, &payload(HookTransition::Done))
        .await
        .unwrap();

    let sent = nats.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "agent.driver.hook.test");
    let s = std::str::from_utf8(&sent[0].1).unwrap();
    assert!(s.contains("67.10"));
}

#[tokio::test]
async fn unknown_channel_returns_unknown_channel_error() {
    let pairing = PairingAdapterRegistry::new();
    let dispatcher = DefaultHookDispatcher::new(pairing, Arc::new(NoopNatsHookPublisher));
    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::NotifyChannel {
            plugin: "slack".into(),
            instance: "main".into(),
            recipient: "#general".into(),
        },
    };
    let err = dispatcher
        .dispatch(&hook, &payload(HookTransition::Done))
        .await
        .unwrap_err();
    assert_eq!(err, HookError::UnknownChannel("slack".into()));
}

#[tokio::test]
async fn adapter_failure_surfaces_as_adapter_error() {
    let pairing = PairingAdapterRegistry::new();
    pairing.register(Arc::new(FailingAdapter) as Arc<dyn PairingChannelAdapter>);
    let dispatcher = DefaultHookDispatcher::new(pairing, Arc::new(NoopNatsHookPublisher));

    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::NotifyChannel {
            plugin: "whatsapp".into(),
            instance: "main".into(),
            recipient: "+57...".into(),
        },
    };
    let err = dispatcher
        .dispatch(&hook, &payload(HookTransition::Done))
        .await
        .unwrap_err();
    assert!(matches!(err, HookError::Adapter(_)));
}

#[tokio::test]
async fn shell_action_is_disabled_by_default() {
    let pairing = PairingAdapterRegistry::new();
    let dispatcher = DefaultHookDispatcher::new(pairing, Arc::new(NoopNatsHookPublisher));
    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::Shell {
            cmd: "echo hi".into(),
            timeout: Duration::from_secs(1),
        },
    };
    let err = dispatcher
        .dispatch(&hook, &payload(HookTransition::Done))
        .await
        .unwrap_err();
    assert_eq!(err, HookError::ShellDisabled);
}

#[tokio::test]
async fn hook_trigger_matches_only_relevant_transitions() {
    let done = HookTrigger::Done;
    assert!(done.matches(HookTransition::Done));
    assert!(!done.matches(HookTransition::Failed));

    let prog = HookTrigger::Progress { every_turns: 5 };
    assert!(prog.matches(HookTransition::Progress));
    assert!(!prog.matches(HookTransition::Done));

    let off = HookTrigger::Progress { every_turns: 0 };
    assert!(!off.matches(HookTransition::Progress));
}
