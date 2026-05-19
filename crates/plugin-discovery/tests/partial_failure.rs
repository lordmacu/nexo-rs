//! Phase 98.8 — integration test for `sources::run_all`.
//!
//! Drives 3 in-process `Source` impls (one healthy, one slow past
//! timeout, one returning a synthetic error) through the parallel
//! runner and asserts:
//!   - Healthy source's items land in `outcome.items`.
//!   - Slow source surfaces in `partial_failures` with a timeout
//!     message — does NOT block the other sources from finishing.
//!   - Errored source surfaces in `partial_failures` with its
//!     own message preserved.
//!
//! Tests live as an integration crate (`tests/`) so the runner
//! exercises `Box<dyn Source>` cleanly without leaking test-only
//! fakes into the public module surface.

use std::time::Duration;

use async_trait::async_trait;

use nexo_plugin_discovery::sources::{run_all, source_error, Source};
use nexo_plugin_discovery::types::{
    CompatStatus, DiscoveredPlugin, PluginCategory, PluginSource, SourceError, TrustTier,
};
use nexo_tool_meta::admin::plugin_install::{InstallSource, PluginsInstallParams};

/// Healthy fake: returns one synthetic plugin immediately.
struct HealthySource;

#[async_trait]
impl Source for HealthySource {
    fn name(&self) -> &'static str {
        "healthy_fake"
    }
    async fn fetch(&self) -> Result<Vec<DiscoveredPlugin>, SourceError> {
        Ok(vec![stub_plugin()])
    }
}

/// Slow fake: sleeps longer than the per-source timeout so the
/// runner aborts the future.
struct SlowSource {
    sleep_for: Duration,
}

#[async_trait]
impl Source for SlowSource {
    fn name(&self) -> &'static str {
        "slow_fake"
    }
    async fn fetch(&self) -> Result<Vec<DiscoveredPlugin>, SourceError> {
        tokio::time::sleep(self.sleep_for).await;
        Ok(vec![])
    }
}

/// Errored fake: returns a typed `SourceError` immediately.
struct ErroredSource;

#[async_trait]
impl Source for ErroredSource {
    fn name(&self) -> &'static str {
        "errored_fake"
    }
    async fn fetch(&self) -> Result<Vec<DiscoveredPlugin>, SourceError> {
        Err(source_error("errored_fake", "synthetic upstream failure"))
    }
}

fn stub_plugin() -> DiscoveredPlugin {
    DiscoveredPlugin {
        name: "nexo-plugin-stub".into(),
        version: Some("0.1.0".into()),
        description: None,
        owner: "lordmacu".into(),
        sources: vec![PluginSource::CratesIo],
        repo_url: None,
        homepage: None,
        tags: vec![],
        category: PluginCategory::Unknown,
        trust_tier: TrustTier::Unverified,
        compat: CompatStatus::Unknown,
        manifest_url: None,
        install_cmd: "cargo install nexo-plugin-stub --version 0.1.0".into(),
        install_params: PluginsInstallParams {
            crate_name: "nexo-plugin-stub".into(),
            version: Some("0.1.0".into()),
            repo: None,
            source: InstallSource::Release,
            force: false,
            require_signature: false,
            skip_signature_verify: false,
        },
    }
}

#[tokio::test]
async fn one_slow_source_does_not_block_others_and_surfaces_partial_failure() {
    let sources: Vec<Box<dyn Source>> = vec![
        Box::new(HealthySource),
        Box::new(SlowSource {
            // Sleeps 1.5s while the runner's timeout is 250ms — must
            // be aborted + surfaced.
            sleep_for: Duration::from_millis(1500),
        }),
        Box::new(ErroredSource),
    ];
    let started = std::time::Instant::now();
    let outcome = run_all(&sources, Duration::from_millis(250)).await;
    let elapsed = started.elapsed();

    // 1. Slow source DID NOT stall the runner. Tight upper bound on
    // wallclock: timeout 250ms + a generous 1s of scheduling margin.
    // Strictly less than `sleep_for` (1500ms).
    assert!(
        elapsed < Duration::from_millis(1250),
        "runner stalled — elapsed: {elapsed:?}, expected < 1250ms"
    );

    // 2. Healthy items present.
    assert_eq!(outcome.items.len(), 1, "healthy fake contributes 1 item");
    assert_eq!(outcome.items[0].name, "nexo-plugin-stub");

    // 3. Two partial failures — slow timeout + errored synthetic.
    assert_eq!(
        outcome.partial_failures.len(),
        2,
        "expected exactly 2 partial failures, got: {:?}",
        outcome.partial_failures
    );
    let slow_err = outcome
        .partial_failures
        .iter()
        .find(|e| e.source == "slow_fake")
        .expect("slow_fake must surface");
    assert!(
        slow_err.message.contains("timed out"),
        "slow timeout message lost: {}",
        slow_err.message
    );
    let err_err = outcome
        .partial_failures
        .iter()
        .find(|e| e.source == "errored_fake")
        .expect("errored_fake must surface");
    assert!(
        err_err.message.contains("synthetic upstream failure"),
        "errored message lost: {}",
        err_err.message
    );
}

#[tokio::test]
async fn all_sources_healthy_yields_empty_partial_failures() {
    let sources: Vec<Box<dyn Source>> = vec![Box::new(HealthySource), Box::new(HealthySource)];
    let outcome = run_all(&sources, Duration::from_secs(5)).await;
    assert!(outcome.partial_failures.is_empty());
    assert_eq!(outcome.items.len(), 2);
}
