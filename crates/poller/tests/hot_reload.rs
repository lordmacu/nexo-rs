//! Step 10: hot-reload diff/apply behaviour.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use nexo_auth::resolver::CredentialStores;
use nexo_auth::{AgentCredentialResolver, BreakerRegistry, CredentialsBundle};
use nexo_broker::AnyBroker;
use nexo_config::types::pollers::{PollerJob, PollersConfig};
use nexo_poller::poller::{PollContext, Poller, TickOutcome};
use nexo_poller::{PollState, PollerError, PollerRunner};
use async_trait::async_trait;
use tokio::sync::Mutex;

fn empty_creds() -> Arc<CredentialsBundle> {
    Arc::new(CredentialsBundle {
        stores: CredentialStores::empty(),
        resolver: Arc::new(AgentCredentialResolver::empty()),
        breakers: Arc::new(BreakerRegistry::default()),
        warnings: Vec::new(),
    })
}

struct Mock;

#[async_trait]
impl Poller for Mock {
    fn kind(&self) -> &'static str { "mock" }
    async fn tick(&self, _ctx: &PollContext) -> Result<TickOutcome, PollerError> {
        Ok(TickOutcome::default())
    }
}

fn job(id: &str, every_secs: u64) -> PollerJob {
    PollerJob {
        id: id.into(),
        kind: "mock".into(),
        agent: "ana".into(),
        schedule: serde_yaml::from_str(&format!("every_secs: {every_secs}")).unwrap(),
        config: serde_yaml::Value::Null,
        failure_to: None,
        paused_on_boot: false,
        extra: Default::default(),
    }
}

#[tokio::test]
async fn diff_classifies_add_remove_keep_replace() {
    let cfg_v1 = PollersConfig {
        jobs: vec![job("a", 60), job("b", 60)],
        ..PollersConfig::default()
    };
    let state = Arc::new(PollState::open_in_memory().await.unwrap());
    let runner = PollerRunner::new(cfg_v1, Arc::clone(&state), AnyBroker::local(), empty_creds());
    runner.register(Arc::new(Mock));
    runner.start().await.unwrap();

    // a kept, b replaced (interval changed), c added, d removed (was b
    // → wait, d was never live; remove path needs a job that's live
    // but missing in the new config). Build the v2 to match.
    let cfg_v2 = PollersConfig {
        jobs: vec![
            job("a", 60),  // keep
            job("b", 120), // replace
            job("c", 60),  // add
            // (no entry for any prior id) — there is nothing else live
        ],
        ..PollersConfig::default()
    };
    let plan = runner.diff(&cfg_v2).await;
    assert_eq!(plan.keep, vec!["a"]);
    assert_eq!(plan.replace, vec!["b"]);
    assert_eq!(plan.add, vec!["c"]);
    assert!(plan.remove.is_empty());

    runner.shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_drops_removed_jobs() {
    let cfg_v1 = PollersConfig {
        jobs: vec![job("a", 60), job("b", 60)],
        ..PollersConfig::default()
    };
    let state = Arc::new(PollState::open_in_memory().await.unwrap());
    let runner = PollerRunner::new(cfg_v1, Arc::clone(&state), AnyBroker::local(), empty_creds());
    runner.register(Arc::new(Mock));
    runner.start().await.unwrap();

    let cfg_v2 = PollersConfig {
        jobs: vec![job("a", 60)], // b removed
        ..PollersConfig::default()
    };
    let plan = runner.reload(cfg_v2).await.unwrap();
    assert_eq!(plan.remove, vec!["b"]);
    assert_eq!(plan.keep, vec!["a"]);

    runner.shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_rejects_invalid_kind_atomically() {
    let cfg_v1 = PollersConfig {
        jobs: vec![job("a", 60)],
        ..PollersConfig::default()
    };
    let state = Arc::new(PollState::open_in_memory().await.unwrap());
    let runner = PollerRunner::new(cfg_v1, Arc::clone(&state), AnyBroker::local(), empty_creds());
    runner.register(Arc::new(Mock));
    runner.start().await.unwrap();

    let mut bad = job("c", 60);
    bad.kind = "nonexistent_kind".into();
    let cfg_v2 = PollersConfig {
        jobs: vec![job("a", 60), bad],
        ..PollersConfig::default()
    };
    let err = runner.reload(cfg_v2).await.unwrap_err();
    let chained = format!("{err:#}");
    assert!(chained.contains("unknown kind"), "got: {chained}");

    // a still listed in current config — siblings unaffected.
    let plan = runner
        .diff(&PollersConfig {
            jobs: vec![job("a", 60)],
            ..PollersConfig::default()
        })
        .await;
    assert!(plan.keep.contains(&"a".to_string()));

    runner.shutdown().await.unwrap();
}

// Silence unused imports under cfg(test).
#[allow(dead_code)]
fn _unused() {
    let _ = AtomicU32::new(0);
    let _: Mutex<()> = Mutex::new(());
}
