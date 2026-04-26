//! Phase 67.F.2 — DispatchPhase action calls the registered
//! DispatchPhaseChainer with the parent payload + child phase_id.
//! Guards: only_if must match the firing transition; if it does
//! not, the chain is skipped (returns ChainGuardSkipped).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nexo_dispatch_tools::hooks::dispatcher::{NatsHookPublisher, NoopNatsHookPublisher};
use nexo_dispatch_tools::{
    CompletionHook, DefaultHookDispatcher, DispatchPhaseChainer, HookAction, HookDispatcher,
    HookError, HookPayload, HookTransition, HookTrigger,
};
use nexo_driver_claude::OriginChannel;
use nexo_driver_types::GoalId;
use nexo_pairing::PairingAdapterRegistry;
use uuid::Uuid;

#[derive(Default)]
struct CapturingChainer {
    calls: Mutex<Vec<(String, GoalId, Option<OriginChannel>)>>,
}

#[async_trait]
impl DispatchPhaseChainer for CapturingChainer {
    async fn chain(&self, parent: &HookPayload, phase_id: &str) -> Result<GoalId, String> {
        let id = GoalId(Uuid::new_v4());
        self.calls.lock().unwrap().push((
            phase_id.to_string(),
            parent.goal_id,
            parent.origin.clone(),
        ));
        Ok(id)
    }
}

fn payload(transition: HookTransition) -> HookPayload {
    HookPayload {
        goal_id: GoalId(Uuid::new_v4()),
        phase_id: "67.10".into(),
        transition,
        summary: "ok".into(),
        elapsed: "1m".into(),
        diff_stat: None,
        origin: Some(OriginChannel {
            plugin: "telegram".into(),
            instance: "family".into(),
            sender_id: "@cris".into(),
            correlation_id: None,
        }),
    }
}

#[tokio::test]
async fn dispatch_phase_calls_chainer_with_parent_origin() {
    let chainer = Arc::new(CapturingChainer::default());
    let chainer_dyn: Arc<dyn DispatchPhaseChainer> = chainer.clone();
    let dispatcher = DefaultHookDispatcher::new(
        PairingAdapterRegistry::new(),
        Arc::new(NoopNatsHookPublisher) as Arc<dyn NatsHookPublisher>,
    )
    .with_chainer(chainer_dyn);

    let hook = CompletionHook {
        id: "chain-1".into(),
        on: HookTrigger::Done,
        action: HookAction::DispatchPhase {
            phase_id: "67.11".into(),
            only_if: HookTrigger::Done,
        },
    };
    dispatcher
        .dispatch(&hook, &payload(HookTransition::Done))
        .await
        .unwrap();

    let calls = chainer.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "67.11");
    let origin = calls[0].2.as_ref().unwrap();
    assert_eq!(origin.plugin, "telegram");
    assert_eq!(origin.sender_id, "@cris");
}

#[tokio::test]
async fn dispatch_phase_guard_skips_when_only_if_not_matched() {
    let chainer: Arc<dyn DispatchPhaseChainer> = Arc::new(CapturingChainer::default());
    let dispatcher = DefaultHookDispatcher::new(
        PairingAdapterRegistry::new(),
        Arc::new(NoopNatsHookPublisher) as Arc<dyn NatsHookPublisher>,
    )
    .with_chainer(chainer);

    // only_if = Done, but goal Failed → guard skips.
    let hook = CompletionHook {
        id: "chain-1".into(),
        on: HookTrigger::Failed,
        action: HookAction::DispatchPhase {
            phase_id: "67.11".into(),
            only_if: HookTrigger::Done,
        },
    };
    let err = dispatcher
        .dispatch(&hook, &payload(HookTransition::Failed))
        .await
        .unwrap_err();
    assert!(matches!(err, HookError::ChainGuardSkipped(_, _)));
}

#[tokio::test]
async fn dispatch_phase_returns_chaining_not_wired_when_no_chainer() {
    let dispatcher = DefaultHookDispatcher::new(
        PairingAdapterRegistry::new(),
        Arc::new(NoopNatsHookPublisher) as Arc<dyn NatsHookPublisher>,
    );
    let hook = CompletionHook {
        id: "chain-1".into(),
        on: HookTrigger::Done,
        action: HookAction::DispatchPhase {
            phase_id: "67.11".into(),
            only_if: HookTrigger::Done,
        },
    };
    let err = dispatcher
        .dispatch(&hook, &payload(HookTransition::Done))
        .await
        .unwrap_err();
    assert_eq!(err, HookError::ChainingNotWired);
}

#[tokio::test]
async fn chainer_failure_surfaces_as_chain_dispatch_error() {
    struct Failing;
    #[async_trait]
    impl DispatchPhaseChainer for Failing {
        async fn chain(&self, _p: &HookPayload, _phase: &str) -> Result<GoalId, String> {
            Err("registry rejected".into())
        }
    }
    let chainer: Arc<dyn DispatchPhaseChainer> = Arc::new(Failing);
    let dispatcher = DefaultHookDispatcher::new(
        PairingAdapterRegistry::new(),
        Arc::new(NoopNatsHookPublisher) as Arc<dyn NatsHookPublisher>,
    )
    .with_chainer(chainer);

    let hook = CompletionHook {
        id: "chain-1".into(),
        on: HookTrigger::Done,
        action: HookAction::DispatchPhase {
            phase_id: "67.11".into(),
            only_if: HookTrigger::Done,
        },
    };
    let err = dispatcher
        .dispatch(&hook, &payload(HookTransition::Done))
        .await
        .unwrap_err();
    assert!(matches!(err, HookError::ChainDispatch(_)));
}
