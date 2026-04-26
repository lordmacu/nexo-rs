//! Phase 67.E.2 — `dispatch_followup` decision paths. Same shape
//! as the program_phase tests: queue / cap / capability checks
//! exercise without requiring a real subprocess.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexo_agent_registry::{AgentRegistry, MemoryAgentRegistryStore};
use nexo_config::{DispatchCapability, DispatchPolicy};
use nexo_dispatch_tools::policy_gate::CapSnapshot;
use nexo_dispatch_tools::{
    dispatch_followup_call, DispatchFollowupInput, DispatchFollowupOutput,
};
use nexo_driver_claude::{
    ClaudeConfig, ClaudeDefaultArgs, DispatcherIdentity, MemoryBindingStore, OutputFormat,
};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_project_tracker::FsProjectTracker;

fn write_followups(dir: &std::path::Path) {
    std::fs::write(
        dir.join("PHASES.md"),
        "## Phase 99 — Test\n\n#### 99.1 — One-liner phase   ⬜\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("FOLLOWUPS.md"),
        "\
### Phase 99 — Test follow-ups

H-1. **Hardening item**
- body line a
- body line b

H-2. ~~**Already done item**~~  ✅ shipped
- body
",
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
    let binding: Arc<dyn nexo_driver_claude::SessionBindingStore> =
        Arc::new(MemoryBindingStore::new());
    Arc::new(
        DriverOrchestrator::builder()
            .claude_config(claude_cfg)
            .binding_store(binding)
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
async fn unknown_code_returns_not_found() {
    let dir = tempfile::tempdir().unwrap();
    write_followups(dir.path());
    let tracker = FsProjectTracker::open(dir.path()).unwrap();
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let orch = build_orch(dir.path().join("d.sock"), dir.path().join("ws")).await;

    let out = dispatch_followup_call(
        DispatchFollowupInput {
            code: "Z-99".into(),
            acceptance_override: None,
            budget_override: None,
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
        None,
    )
    .await
    .unwrap();
    match out {
        DispatchFollowupOutput::NotFound { code } => assert_eq!(code, "Z-99"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn resolved_code_returns_already_resolved() {
    let dir = tempfile::tempdir().unwrap();
    write_followups(dir.path());
    let tracker = FsProjectTracker::open(dir.path()).unwrap();
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let orch = build_orch(dir.path().join("d2.sock"), dir.path().join("ws2")).await;

    let out = dispatch_followup_call(
        DispatchFollowupInput {
            code: "H-2".into(),
            acceptance_override: None,
            budget_override: None,
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
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    match out {
        DispatchFollowupOutput::AlreadyResolved { code } => assert_eq!(code, "H-2"),
        other => panic!("expected AlreadyResolved, got {other:?}"),
    }
    assert_eq!(registry.count_running(), 0);
}

#[tokio::test]
async fn capability_none_blocks_followup_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    write_followups(dir.path());
    let tracker = FsProjectTracker::open(dir.path()).unwrap();
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let orch = build_orch(dir.path().join("d3.sock"), dir.path().join("ws3")).await;

    let mut policy = full_policy();
    policy.mode = DispatchCapability::None;

    let out = dispatch_followup_call(
        DispatchFollowupInput {
            code: "H-1".into(),
            acceptance_override: None,
            budget_override: None,
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
        None,
    )
    .await
    .unwrap();
    assert!(matches!(out, DispatchFollowupOutput::Forbidden { .. }));
    assert_eq!(registry.count_running(), 0);
}

#[tokio::test]
async fn forbidden_phase_namespace_blocks_followups() {
    let dir = tempfile::tempdir().unwrap();
    write_followups(dir.path());
    let tracker = FsProjectTracker::open(dir.path()).unwrap();
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let orch = build_orch(dir.path().join("d4.sock"), dir.path().join("ws4")).await;

    // forbidden_phase_ids: ["followup:*"] → all follow-ups blocked.
    let mut policy = full_policy();
    policy.forbidden_phase_ids = vec!["followup:*".into()];

    let out = dispatch_followup_call(
        DispatchFollowupInput {
            code: "H-1".into(),
            acceptance_override: None,
            budget_override: None,
        },
        &tracker,
        orch,
        registry,
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
    assert!(matches!(out, DispatchFollowupOutput::Forbidden { .. }));
}
