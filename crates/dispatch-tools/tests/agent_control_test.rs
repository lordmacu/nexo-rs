//! Phase 67.G.2 — cancel/pause/resume tool surface.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use nexo_agent_registry::{
    AgentHandle, AgentRegistry, AgentRunStatus, AgentSnapshot, MemoryAgentRegistryStore,
};
use nexo_dispatch_tools::{
    cancel_agent, pause_agent, resume_agent, CancelAgentInput, PauseAgentInput,
};
use nexo_driver_claude::{ClaudeConfig, ClaudeDefaultArgs, MemoryBindingStore, OutputFormat};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_driver_types::GoalId;
use uuid::Uuid;

async fn build_orch(socket: PathBuf, ws: PathBuf) -> Arc<DriverOrchestrator> {
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
    Arc::new(
        DriverOrchestrator::builder()
            .claude_config(claude_cfg)
            .binding_store(Arc::new(MemoryBindingStore::new())
                as Arc<dyn nexo_driver_claude::SessionBindingStore>)
            .decider(Arc::new(AllowAllDecider) as Arc<dyn PermissionDecider>)
            .workspace_manager(Arc::new(WorkspaceManager::new(&ws)))
            .event_sink(Arc::new(NoopEventSink))
            .bin_path(PathBuf::from("/usr/local/bin/nexo-driver-permission-mcp"))
            .socket_path(socket)
            .build()
            .await
            .unwrap(),
    )
}

fn handle(id: GoalId) -> AgentHandle {
    AgentHandle {
        goal_id: id,
        phase_id: "67.10".into(),
        status: AgentRunStatus::Running,
        origin: None,
        dispatcher: None,
        started_at: Utc::now(),
        finished_at: None,
        snapshot: AgentSnapshot::default(),
        plan_mode: None,
    }
}

#[tokio::test]
async fn cancel_unknown_goal_returns_cancelled_false() {
    let dir = tempfile::tempdir().unwrap();
    let orch = build_orch(dir.path().join("d.sock"), dir.path().join("ws")).await;
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let out = cancel_agent(
        CancelAgentInput {
            goal_id: GoalId(Uuid::new_v4()),
            reason: None,
        },
        orch,
        registry,
    )
    .await
    .unwrap();
    assert!(!out.cancelled);
    assert!(out.previous_status.is_none());
}

#[tokio::test]
async fn cancel_known_goal_flips_status_to_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let orch = build_orch(dir.path().join("d2.sock"), dir.path().join("ws2")).await;
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let id = GoalId(Uuid::new_v4());
    registry.admit(handle(id), true).await.unwrap();

    let out = cancel_agent(
        CancelAgentInput {
            goal_id: id,
            reason: Some("operator".into()),
        },
        orch,
        registry.clone(),
    )
    .await
    .unwrap();
    assert!(out.cancelled);
    assert_eq!(
        registry.handle(id).unwrap().status,
        AgentRunStatus::Cancelled
    );
}

#[tokio::test]
async fn pause_resume_unknown_goal_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let orch = build_orch(dir.path().join("d3.sock"), dir.path().join("ws3")).await;
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let id = GoalId(Uuid::new_v4());
    let p = pause_agent(
        PauseAgentInput { goal_id: id },
        orch.clone(),
        registry.clone(),
    )
    .await
    .unwrap();
    assert!(!p.paused);
    let r = resume_agent(PauseAgentInput { goal_id: id }, orch, registry)
        .await
        .unwrap();
    // resume returns paused=false when nothing to resume.
    assert!(r.paused);
}
