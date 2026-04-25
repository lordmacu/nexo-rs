//! Step 9 verification: PollerError variants drive the right runner
//! reaction — backoff bookkeeping for Transient, auto-pause for
//! Permanent, breaker open after threshold.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use nexo_auth::resolver::CredentialStores;
use nexo_auth::{AgentCredentialResolver, BreakerRegistry, CredentialsBundle};
use nexo_broker::AnyBroker;
use nexo_config::types::pollers::{PollerJob, PollersConfig};
use nexo_poller::poller::{PollContext, Poller, TickOutcome};
use nexo_poller::{PollState, PollerError, PollerRunner};

fn empty_creds() -> Arc<CredentialsBundle> {
    Arc::new(CredentialsBundle {
        stores: CredentialStores::empty(),
        resolver: Arc::new(AgentCredentialResolver::empty()),
        breakers: Arc::new(BreakerRegistry::default()),
        warnings: Vec::new(),
    })
}

struct AlwaysTransient {
    calls: Arc<AtomicU32>,
}

#[async_trait]
impl Poller for AlwaysTransient {
    fn kind(&self) -> &'static str {
        "always_transient"
    }
    async fn tick(&self, _ctx: &PollContext) -> Result<TickOutcome, PollerError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(PollerError::Transient(anyhow::anyhow!("503 from upstream")))
    }
}

struct AlwaysPermanent;

#[async_trait]
impl Poller for AlwaysPermanent {
    fn kind(&self) -> &'static str {
        "always_permanent"
    }
    async fn tick(&self, _ctx: &PollContext) -> Result<TickOutcome, PollerError> {
        Err(PollerError::Permanent(anyhow::anyhow!(
            "refresh_token revoked"
        )))
    }
}

fn job(id: &str, kind: &str) -> PollerJob {
    PollerJob {
        id: id.into(),
        kind: kind.into(),
        agent: "ana".into(),
        schedule: serde_yaml::from_str("every_secs: 60").unwrap(),
        config: serde_yaml::Value::Null,
        failure_to: None,
        paused_on_boot: false,
        extra: Default::default(),
    }
}

#[tokio::test]
async fn run_once_reports_transient_as_error() {
    let cfg = PollersConfig {
        jobs: vec![job("a", "always_transient")],
        ..PollersConfig::default()
    };
    let state = Arc::new(PollState::open_in_memory().await.unwrap());
    let runner = PollerRunner::new(cfg, Arc::clone(&state), AnyBroker::local(), empty_creds());
    let calls = Arc::new(AtomicU32::new(0));
    runner.register(Arc::new(AlwaysTransient {
        calls: Arc::clone(&calls),
    }));
    let err = runner.run_once("a").await.unwrap_err();
    assert!(err.to_string().contains("transient"));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn run_once_reports_permanent_as_error() {
    let cfg = PollersConfig {
        jobs: vec![job("a", "always_permanent")],
        ..PollersConfig::default()
    };
    let state = Arc::new(PollState::open_in_memory().await.unwrap());
    let runner = PollerRunner::new(cfg, Arc::clone(&state), AnyBroker::local(), empty_creds());
    runner.register(Arc::new(AlwaysPermanent));
    let err = runner.run_once("a").await.unwrap_err();
    assert!(err.to_string().contains("permanent"));
}
