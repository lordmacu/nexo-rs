//! Phase 67.F.3 — `try_claim` is idempotent across pool reopens
//! (i.e. across daemon restarts).

use std::path::PathBuf;

use nexo_dispatch_tools::hooks::idempotency::HookIdempotencyStore;
use nexo_dispatch_tools::{HookAction, HookTransition};
use nexo_driver_types::GoalId;
use uuid::Uuid;

fn tmp_db() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "nexo-hook-idem-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

#[tokio::test]
async fn first_claim_succeeds_second_claim_skipped() {
    let store = HookIdempotencyStore::open_memory().await.unwrap();
    let g = GoalId(Uuid::new_v4());
    let action = HookAction::NotifyOrigin;

    assert!(store
        .try_claim(g, HookTransition::Done, &action, "h1")
        .await
        .unwrap());
    assert!(!store
        .try_claim(g, HookTransition::Done, &action, "h1")
        .await
        .unwrap());
    assert!(store
        .was_dispatched(g, HookTransition::Done, &action, "h1")
        .await
        .unwrap());
}

#[tokio::test]
async fn different_action_id_is_independent_slot() {
    let store = HookIdempotencyStore::open_memory().await.unwrap();
    let g = GoalId(Uuid::new_v4());
    let action = HookAction::NotifyChannel {
        plugin: "telegram".into(),
        instance: "main".into(),
        recipient: "@cris".into(),
    };
    assert!(store
        .try_claim(g, HookTransition::Done, &action, "h1")
        .await
        .unwrap());
    // Same kind, same goal, same transition, different action_id → still claims.
    assert!(store
        .try_claim(g, HookTransition::Done, &action, "h2")
        .await
        .unwrap());
}

#[tokio::test]
async fn claim_persists_across_pool_reopen() {
    let path = tmp_db();
    let path_str = path.to_string_lossy().into_owned();
    let g = GoalId(Uuid::new_v4());
    let action = HookAction::NotifyOrigin;

    {
        let store = HookIdempotencyStore::open(&path_str).await.unwrap();
        assert!(store
            .try_claim(g, HookTransition::Done, &action, "h1")
            .await
            .unwrap());
    }
    // Daemon restart — reopen the same file.
    let store = HookIdempotencyStore::open(&path_str).await.unwrap();
    assert!(!store
        .try_claim(g, HookTransition::Done, &action, "h1")
        .await
        .unwrap());

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn dispatcher_skips_second_firing_when_idempotency_wired() {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use nexo_dispatch_tools::hooks::dispatcher::{NatsHookPublisher, NoopNatsHookPublisher};
    use nexo_dispatch_tools::{
        CompletionHook, DefaultHookDispatcher, HookAction, HookDispatcher, HookError, HookPayload,
        HookTrigger,
    };
    use nexo_driver_claude::OriginChannel;
    use nexo_pairing::{PairingAdapterRegistry, PairingChannelAdapter};

    #[derive(Default)]
    struct Counting {
        sent: Mutex<u32>,
    }
    #[async_trait]
    impl PairingChannelAdapter for Counting {
        fn channel_id(&self) -> &'static str {
            "telegram"
        }
        fn normalize_sender(&self, raw: &str) -> Option<String> {
            Some(raw.to_string())
        }
        async fn send_reply(&self, _a: &str, _t: &str, _x: &str) -> anyhow::Result<()> {
            *self.sent.lock().unwrap() += 1;
            Ok(())
        }
    }

    let pairing = PairingAdapterRegistry::new();
    let cap = Arc::new(Counting::default());
    pairing.register(cap.clone() as Arc<dyn PairingChannelAdapter>);

    let store = Arc::new(HookIdempotencyStore::open_memory().await.unwrap());
    let nats: Arc<dyn NatsHookPublisher> = Arc::new(NoopNatsHookPublisher);
    let dispatcher = DefaultHookDispatcher::new(pairing, nats).with_idempotency(store);

    let hook = CompletionHook {
        id: "h1".into(),
        on: HookTrigger::Done,
        action: HookAction::NotifyOrigin,
    };
    let payload = HookPayload {
        goal_id: GoalId(Uuid::new_v4()),
        phase_id: "67.10".into(),
        transition: HookTransition::Done,
        summary: "ok".into(),
        elapsed: "1m".into(),
        diff_stat: None,
        origin: Some(OriginChannel {
            plugin: "telegram".into(),
            instance: "x".into(),
            sender_id: "@y".into(),
            correlation_id: None,
        }),
    };

    dispatcher.dispatch(&hook, &payload).await.unwrap();
    let err = dispatcher.dispatch(&hook, &payload).await.unwrap_err();
    assert_eq!(err, HookError::AlreadyDispatched);
    assert_eq!(*cap.sent.lock().unwrap(), 1, "side effect ran exactly once");
}

#[tokio::test]
async fn forget_goal_drops_every_slot() {
    let store = HookIdempotencyStore::open_memory().await.unwrap();
    let g = GoalId(Uuid::new_v4());
    let action = HookAction::NotifyOrigin;
    store
        .try_claim(g, HookTransition::Done, &action, "h1")
        .await
        .unwrap();
    store
        .try_claim(g, HookTransition::Failed, &action, "h2")
        .await
        .unwrap();
    let n = store.forget_goal(g).await.unwrap();
    assert_eq!(n, 2);
    assert!(!store
        .was_dispatched(g, HookTransition::Done, &action, "h1")
        .await
        .unwrap());
}
