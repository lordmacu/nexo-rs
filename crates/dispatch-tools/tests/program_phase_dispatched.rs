//! Phase 67.E.1 — `program_phase` happy / denied paths.
//!
//! We don't actually spawn a Claude subprocess in these tests. The
//! gate-denied + tracker-not-found paths don't reach `spawn_goal` at
//! all; the cap-rejected path doesn't either. The "happy admit"
//! path is exercised in the multi-agent end-to-end test in 67.H.x
//! once the full mock-claude harness is wired through dispatch-tools.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexo_agent_registry::{AgentRegistry, MemoryAgentRegistryStore};
use nexo_config::{DispatchCapability, DispatchPolicy};
use nexo_dispatch_tools::policy_gate::CapSnapshot;
use nexo_dispatch_tools::{program_phase_dispatch, ProgramPhaseInput, ProgramPhaseOutput};
use nexo_driver_claude::{
    ClaudeConfig, ClaudeDefaultArgs, DispatcherIdentity, MemoryBindingStore, OutputFormat,
};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_project_tracker::FsProjectTracker;

fn write_phases(dir: &std::path::Path) {
    std::fs::write(
        dir.join("PHASES.md"),
        "## Phase 99 — Test\n\n#### 99.1 — One-liner phase   ⬜\n",
    )
    .unwrap();
}

async fn build_orch(socket: PathBuf, workspace_root: PathBuf) -> Arc<DriverOrchestrator> {
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
    let workspace_manager = Arc::new(WorkspaceManager::new(&workspace_root));
    let decider: Arc<dyn PermissionDecider> = Arc::new(AllowAllDecider);
    let event_sink: Arc<dyn nexo_driver_loop::DriverEventSink> = Arc::new(NoopEventSink);
    Arc::new(
        DriverOrchestrator::builder()
            .claude_config(claude_cfg)
            .binding_store(binding)
            .decider(decider)
            .workspace_manager(workspace_manager)
            .event_sink(event_sink)
            .bin_path(PathBuf::from("/usr/local/bin/nexo-driver-permission-mcp"))
            .socket_path(socket)
            .build()
            .await
            .unwrap(),
    )
}

fn full_policy() -> DispatchPolicy {
    DispatchPolicy {
        mode: DispatchCapability::Full,
        max_concurrent_per_dispatcher: 0,
        allowed_phase_ids: Vec::new(),
        forbidden_phase_ids: Vec::new(),
    }
}

fn dispatcher() -> DispatcherIdentity {
    DispatcherIdentity {
        agent_id: "tester".into(),
        sender_id: Some("@cris".into()),
        parent_goal_id: None,
        chain_depth: 0,
    }
}

#[tokio::test]
async fn capability_none_returns_forbidden() {
    let dir = tempfile::tempdir().unwrap();
    write_phases(dir.path());
    let tracker = FsProjectTracker::open(dir.path()).unwrap();
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let orch = build_orch(dir.path().join("d.sock"), dir.path().join("ws")).await;

    let mut policy = full_policy();
    policy.mode = DispatchCapability::None;

    let out = program_phase_dispatch(
        ProgramPhaseInput {
            phase_id: "99.1".into(),
            acceptance_override: None,
            budget_override: None,
            hooks: Vec::new(),
        },
        &tracker,
        orch.clone(),
        registry.clone(),
        &policy,
        true,
        true,
        dispatcher(),
        None,
        CapSnapshot {
            queue_when_full: true,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();

    match out {
        ProgramPhaseOutput::Forbidden { phase_id, .. } => assert_eq!(phase_id, "99.1"),
        other => panic!("expected Forbidden, got {other:?}"),
    }
    assert_eq!(registry.count_running(), 0);
    let _ = Arc::try_unwrap(orch).map(|o| o.shutdown());
}

#[tokio::test]
async fn unknown_phase_returns_not_found() {
    let dir = tempfile::tempdir().unwrap();
    write_phases(dir.path());
    let tracker = FsProjectTracker::open(dir.path()).unwrap();
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let orch = build_orch(dir.path().join("d2.sock"), dir.path().join("ws2")).await;

    let out = program_phase_dispatch(
        ProgramPhaseInput {
            phase_id: "9999.999".into(),
            acceptance_override: None,
            budget_override: None,
            hooks: Vec::new(),
        },
        &tracker,
        orch.clone(),
        registry.clone(),
        &full_policy(),
        true,
        true,
        dispatcher(),
        None,
        CapSnapshot {
            queue_when_full: true,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();

    match out {
        ProgramPhaseOutput::NotFound { phase_id } => assert_eq!(phase_id, "9999.999"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn untrusted_sender_returns_forbidden_when_required() {
    let dir = tempfile::tempdir().unwrap();
    write_phases(dir.path());
    let tracker = FsProjectTracker::open(dir.path()).unwrap();
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let orch = build_orch(dir.path().join("d3.sock"), dir.path().join("ws3")).await;

    let out = program_phase_dispatch(
        ProgramPhaseInput {
            phase_id: "99.1".into(),
            acceptance_override: None,
            budget_override: None,
            hooks: Vec::new(),
        },
        &tracker,
        orch,
        registry.clone(),
        &full_policy(),
        true,  // require_trusted
        false, // sender_trusted
        dispatcher(),
        None,
        CapSnapshot {
            queue_when_full: true,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();

    assert!(matches!(out, ProgramPhaseOutput::Forbidden { .. }));
    assert_eq!(registry.count_running(), 0);
}

#[tokio::test]
async fn cap_reached_with_queue_returns_queued_outcome() {
    let dir = tempfile::tempdir().unwrap();
    write_phases(dir.path());
    let tracker = FsProjectTracker::open(dir.path()).unwrap();
    // Cap=0 forces every admission into the queue; queue_when_full
    // is true so the gate passes and AgentRegistry::admit returns
    // Queued without ever calling spawn_goal.
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        0,
    ));
    let orch = build_orch(dir.path().join("d4.sock"), dir.path().join("ws4")).await;

    let out = program_phase_dispatch(
        ProgramPhaseInput {
            phase_id: "99.1".into(),
            acceptance_override: None,
            budget_override: None,
            hooks: Vec::new(),
        },
        &tracker,
        orch,
        registry.clone(),
        &full_policy(),
        true,
        true,
        dispatcher(),
        None,
        CapSnapshot {
            queue_when_full: true,
            global_max: 0,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();

    match out {
        ProgramPhaseOutput::Queued { position, .. } => assert_eq!(position, 1),
        other => panic!("expected Queued, got {other:?}"),
    }
}
