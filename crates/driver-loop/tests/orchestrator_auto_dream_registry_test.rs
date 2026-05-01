//! Phase 80.1.b.b.b.c — multi-runner auto_dream registry surface
//! tests. Verifies `register_auto_dream` / `unregister_auto_dream` /
//! `auto_dream_agents` / `has_auto_dream` keep the per-agent map
//! coherent and that the deprecated `set_auto_dream` shim still
//! routes to the sentinel `_default` key.

#![cfg(unix)]
#![allow(deprecated)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nexo_driver_claude::{ClaudeConfig, ClaudeDefaultArgs, MemoryBindingStore, OutputFormat};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_driver_types::auto_dream::{AutoDreamHook, AutoDreamOutcomeKind, DreamContext};

struct DummyHook;

#[async_trait]
impl AutoDreamHook for DummyHook {
    async fn check_and_run(&self, _ctx: &DreamContext) -> AutoDreamOutcomeKind {
        AutoDreamOutcomeKind::SkippedDisabled
    }
}

async fn build_orch() -> DriverOrchestrator {
    let dir = tempfile::tempdir().unwrap();
    let claude_cfg = ClaudeConfig {
        binary: Some(PathBuf::from("bash")),
        default_args: ClaudeDefaultArgs {
            output_format: OutputFormat::StreamJson,
            permission_prompt_tool: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            model: None,
        },
        mcp_config: None,
        forced_kill_after: Duration::from_secs(1),
        turn_timeout: Duration::from_secs(10),
    };
    let binding: Arc<dyn nexo_driver_claude::SessionBindingStore> =
        Arc::new(MemoryBindingStore::new());
    let workspace_manager = Arc::new(WorkspaceManager::new(dir.path()));
    let decider: Arc<dyn PermissionDecider> = Arc::new(AllowAllDecider);
    let event_sink: Arc<dyn nexo_driver_loop::DriverEventSink> = Arc::new(NoopEventSink);
    DriverOrchestrator::builder()
        .claude_config(claude_cfg)
        .binding_store(binding)
        .decider(decider)
        .workspace_manager(workspace_manager)
        .event_sink(event_sink)
        .bin_path(PathBuf::from("/usr/local/bin/nexo-driver-permission-mcp"))
        .socket_path(dir.path().join("driver.sock"))
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn register_auto_dream_returns_none_when_first() {
    let orch = build_orch().await;
    let hook: Arc<dyn AutoDreamHook> = Arc::new(DummyHook);
    let prev = orch.register_auto_dream("ana".to_string(), hook);
    assert!(prev.is_none());
    assert!(orch.has_auto_dream());
    assert_eq!(orch.auto_dream_agents(), vec!["ana".to_string()]);
    let _ = orch.shutdown().await;
}

#[tokio::test]
async fn register_auto_dream_returns_previous_when_overwriting() {
    let orch = build_orch().await;
    let first: Arc<dyn AutoDreamHook> = Arc::new(DummyHook);
    let second: Arc<dyn AutoDreamHook> = Arc::new(DummyHook);
    assert!(
        orch.register_auto_dream("ana".to_string(), first.clone())
            .is_none()
    );
    let prev = orch.register_auto_dream("ana".to_string(), second);
    assert!(prev.is_some(), "overwrite must return the displaced hook");
    assert!(Arc::ptr_eq(&prev.unwrap(), &first));
    assert_eq!(orch.auto_dream_agents(), vec!["ana".to_string()]);
    let _ = orch.shutdown().await;
}

#[tokio::test]
async fn unregister_auto_dream_returns_previous_or_none() {
    let orch = build_orch().await;
    let hook: Arc<dyn AutoDreamHook> = Arc::new(DummyHook);
    orch.register_auto_dream("ana".to_string(), hook.clone());

    let removed = orch.unregister_auto_dream("ana");
    assert!(removed.is_some());
    assert!(Arc::ptr_eq(&removed.unwrap(), &hook));
    assert!(!orch.has_auto_dream());

    let absent = orch.unregister_auto_dream("ghost");
    assert!(absent.is_none());
    let _ = orch.shutdown().await;
}

#[tokio::test]
async fn auto_dream_agents_returns_sorted_keys() {
    let orch = build_orch().await;
    for id in ["zara", "ana", "milo", "beto"] {
        let h: Arc<dyn AutoDreamHook> = Arc::new(DummyHook);
        orch.register_auto_dream(id.to_string(), h);
    }
    assert_eq!(
        orch.auto_dream_agents(),
        vec![
            "ana".to_string(),
            "beto".to_string(),
            "milo".to_string(),
            "zara".to_string(),
        ]
    );
    let _ = orch.shutdown().await;
}

#[tokio::test]
async fn set_auto_dream_compat_shim_uses_default_key() {
    let orch = build_orch().await;
    let hook: Arc<dyn AutoDreamHook> = Arc::new(DummyHook);
    orch.set_auto_dream(Some(hook));
    assert_eq!(orch.auto_dream_agents(), vec!["_default".to_string()]);
    assert!(orch.has_auto_dream());
    orch.set_auto_dream(None);
    assert!(!orch.has_auto_dream());
    let _ = orch.shutdown().await;
}
