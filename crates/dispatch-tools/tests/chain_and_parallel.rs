//! Phase 67.G.1 — chain + parallel orchestration helpers.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexo_agent_registry::{AgentRegistry, MemoryAgentRegistryStore};
use nexo_config::{DispatchCapability, DispatchPolicy};
use nexo_dispatch_tools::policy_gate::CapSnapshot;
use nexo_dispatch_tools::{
    program_phase_chain, program_phase_parallel, HookAction, ProgramPhaseChainInput,
    ProgramPhaseOutput, ProgramPhaseParallelInput,
};
use nexo_driver_claude::{
    ClaudeConfig, ClaudeDefaultArgs, DispatcherIdentity, MemoryBindingStore, OutputFormat,
};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_project_tracker::FsProjectTracker;

fn write_phases(dir: &std::path::Path) {
    std::fs::write(
        dir.join("PHASES.md"),
        "## Phase 99 — Test\n\n#### 99.1 — A   ⬜\n#### 99.2 — B   ⬜\n#### 99.3 — C   ⬜\n",
    )
    .unwrap();
}

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

fn full_policy() -> DispatchPolicy {
    DispatchPolicy {
        mode: DispatchCapability::Full,
        ..Default::default()
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
async fn parallel_dispatches_each_phase_until_local_cap() {
    let dir = tempfile::tempdir().unwrap();
    write_phases(dir.path());
    let tracker = FsProjectTracker::open(dir.path()).unwrap();
    // Cap=0 forces every dispatch into the queue without ever
    // calling spawn_goal (no real Claude needed).
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        0,
    ));
    let orch = build_orch(dir.path().join("d.sock"), dir.path().join("ws")).await;

    let out = program_phase_parallel(
        ProgramPhaseParallelInput {
            phases: vec!["99.1".into(), "99.2".into(), "99.3".into()],
            max_concurrent: Some(2),
        },
        &tracker,
        orch,
        registry,
        &full_policy(),
        true,
        true,
        dispatcher(),
        None,
        CapSnapshot {
            queue_when_full: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(out.results.len(), 3);
    // First two queued, third Rejected by local-cap.
    assert!(matches!(out.results[0], ProgramPhaseOutput::Queued { .. }));
    assert!(matches!(out.results[1], ProgramPhaseOutput::Queued { .. }));
    assert!(matches!(
        out.results[2],
        ProgramPhaseOutput::Rejected { .. }
    ));
}

#[tokio::test]
async fn chain_returns_first_dispatch_plus_dispatch_phase_hooks() {
    let dir = tempfile::tempdir().unwrap();
    write_phases(dir.path());
    let tracker = FsProjectTracker::open(dir.path()).unwrap();
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        0,
    ));
    let orch = build_orch(dir.path().join("d2.sock"), dir.path().join("ws2")).await;

    let out = program_phase_chain(
        ProgramPhaseChainInput {
            phases: vec!["99.1".into(), "99.2".into(), "99.3".into()],
            stop_on_fail: true,
        },
        &tracker,
        orch,
        registry,
        &full_policy(),
        true,
        true,
        dispatcher(),
        None,
        CapSnapshot {
            queue_when_full: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // First phase parked.
    assert!(matches!(out.first, ProgramPhaseOutput::Queued { .. }));
    // Two follow-up hooks pointing to 99.2 and 99.3.
    assert_eq!(out.chain_hooks.len(), 2);
    let phases: Vec<&str> = out
        .chain_hooks
        .iter()
        .filter_map(|h| match &h.action {
            HookAction::DispatchPhase { phase_id, .. } => Some(phase_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(phases, vec!["99.2", "99.3"]);
    assert!(out.stop_on_fail);
}

#[tokio::test]
async fn parallel_capability_none_blocks_every_phase() {
    let dir = tempfile::tempdir().unwrap();
    write_phases(dir.path());
    let tracker = FsProjectTracker::open(dir.path()).unwrap();
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let orch = build_orch(dir.path().join("d3.sock"), dir.path().join("ws3")).await;

    let mut policy = full_policy();
    policy.mode = DispatchCapability::None;

    let out = program_phase_parallel(
        ProgramPhaseParallelInput {
            phases: vec!["99.1".into(), "99.2".into()],
            max_concurrent: None,
        },
        &tracker,
        orch,
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
    )
    .await
    .unwrap();
    for r in &out.results {
        assert!(matches!(r, ProgramPhaseOutput::Forbidden { .. }));
    }
    assert_eq!(registry.count_running(), 0);
}
