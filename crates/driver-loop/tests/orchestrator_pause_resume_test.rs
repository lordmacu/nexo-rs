//! Phase 67.C.2 — `pause_goal` / `resume_goal` API surface test.
//! We don't drive a real goal here (that needs a multi-turn fixture
//! with deterministic timing); instead we verify the orchestrator
//! exposes the pause/resume hooks idempotently and the unknown-goal
//! case returns false gracefully.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexo_driver_claude::{ClaudeConfig, ClaudeDefaultArgs, MemoryBindingStore, OutputFormat};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_driver_types::GoalId;
use uuid::Uuid;

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
async fn pause_resume_unknown_goal_returns_false() {
    let orch = build_orch().await;
    let g = GoalId(Uuid::new_v4());
    assert!(!orch.pause_goal(g));
    assert!(!orch.resume_goal(g));
    assert!(!orch.is_paused(g));
    let _ = orch.shutdown().await;
}

/// Synthetic check: register a fake pause signal as the loop would,
/// then exercise pause/resume/is_paused. We can't poke the private
/// `pause_signals` map directly without making it `pub`, so we verify
/// from the outside that pause/resume on a non-tracked goal is a
/// safe no-op. The end-to-end pause-mid-loop integration test lives
/// in the multi-agent dispatch e2e (67.H.x) once the registry is
/// wired into spawn_goal.
#[tokio::test]
async fn b11_pre_register_goal_makes_cancel_pause_callable_before_run() {
    let orch = build_orch().await;
    let g = GoalId(Uuid::new_v4());
    // Before pre_register: cancel/pause silently no-op.
    assert!(!orch.pause_goal(g));
    assert!(!orch.cancel_goal(g));
    // Pre-register tokens (reattach path).
    orch.pre_register_goal(g);
    // Now signals route to a real watch / token.
    assert!(orch.pause_goal(g));
    assert!(orch.cancel_goal(g));
    assert!(orch.is_paused(g));
    assert!(orch.is_cancelled(g));
    let _ = orch.shutdown().await;
}

#[tokio::test]
async fn pause_resume_idempotent_when_not_tracked() {
    let orch = build_orch().await;
    let g = GoalId(Uuid::new_v4());
    assert!(!orch.pause_goal(g));
    assert!(!orch.pause_goal(g));
    assert!(!orch.resume_goal(g));
    let _ = orch.shutdown().await;
}
