//! Phase 67.F.4 — Shell action: disabled by default, runs cmd via
//! `sh -c` when `allow_shell=true`, surfaces exit + timeout errors.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use nexo_dispatch_tools::hooks::dispatcher::{NatsHookPublisher, NoopNatsHookPublisher};
use nexo_dispatch_tools::{
    CompletionHook, DefaultHookDispatcher, HookAction, HookDispatcher, HookError, HookPayload,
    HookTransition, HookTrigger,
};
use nexo_driver_types::GoalId;
use nexo_pairing::PairingAdapterRegistry;
use uuid::Uuid;

fn payload() -> HookPayload {
    HookPayload {
        goal_id: GoalId(Uuid::new_v4()),
        phase_id: "67.10".into(),
        transition: HookTransition::Done,
        summary: "ok".into(),
        elapsed: "1m".into(),
        diff_stat: None,
        origin: None,
    }
}

fn build_dispatcher(allow_shell: bool) -> DefaultHookDispatcher {
    DefaultHookDispatcher::new(
        PairingAdapterRegistry::new(),
        Arc::new(NoopNatsHookPublisher) as Arc<dyn NatsHookPublisher>,
    )
    .with_allow_shell(allow_shell)
}

#[tokio::test]
async fn shell_off_returns_disabled() {
    let d = build_dispatcher(false);
    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::Shell {
            cmd: "true".into(),
            timeout: Duration::from_secs(2),
        },
    };
    let err = d.dispatch(&hook, &payload()).await.unwrap_err();
    assert_eq!(err, HookError::ShellDisabled);
}

#[tokio::test]
async fn shell_on_runs_true_successfully() {
    let d = build_dispatcher(true);
    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::Shell {
            cmd: "true".into(),
            timeout: Duration::from_secs(2),
        },
    };
    d.dispatch(&hook, &payload()).await.unwrap();
}

#[tokio::test]
async fn shell_non_zero_exit_surfaces_error() {
    let d = build_dispatcher(true);
    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::Shell {
            cmd: "echo nope >&2; exit 7".into(),
            timeout: Duration::from_secs(2),
        },
    };
    let err = d.dispatch(&hook, &payload()).await.unwrap_err();
    match err {
        HookError::ShellExitNonZero { code, stderr } => {
            assert_eq!(code, Some(7));
            assert!(stderr.contains("nope"));
        }
        other => panic!("expected ShellExitNonZero, got {other:?}"),
    }
}

#[tokio::test]
async fn shell_timeout_returns_timeout() {
    let d = build_dispatcher(true);
    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::Shell {
            cmd: "sleep 5".into(),
            timeout: Duration::from_millis(150),
        },
    };
    let err = d.dispatch(&hook, &payload()).await.unwrap_err();
    assert!(matches!(err, HookError::Timeout(_)));
}

#[tokio::test]
async fn shell_receives_payload_env_vars() {
    let d = build_dispatcher(true);
    let mut p = payload();
    p.phase_id = "ENV-TEST".into();
    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::Shell {
            // Fail unless the env-var contains the expected phase id.
            cmd: r#"[ "$NEXO_HOOK_PHASE_ID" = "ENV-TEST" ]"#.into(),
            timeout: Duration::from_secs(2),
        },
    };
    d.dispatch(&hook, &p).await.unwrap();
}
