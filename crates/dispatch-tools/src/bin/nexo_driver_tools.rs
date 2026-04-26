//! Phase 67.H.1 — operator-facing CLI mirroring the chat tool
//! surface. Lets the operator dispatch / inspect goals from a
//! shell without having to send a chat message.
//!
//! Usage:
//!   nexo-driver-tools status [--phase <id> | --followups]
//!   nexo-driver-tools dispatch <phase_id>
//!   nexo-driver-tools agents list [--filter running|queued|...]
//!   nexo-driver-tools agents show <goal_id>
//!   nexo-driver-tools agents cancel <goal_id> [--reason "..."]
//!
//! Lives in `dispatch-tools` (not the existing `nexo-driver` bin)
//! because adding the dispatch-tools dep to `driver-loop` would
//! create a cycle (`driver-loop → dispatch-tools → driver-loop`).
//! 67.H.x can fold the surfaces into one binary once the workspace
//! tops the registry.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use nexo_agent_registry::{AgentRegistry, MemoryAgentRegistryStore};
use nexo_config::DispatchPolicy;
use nexo_dispatch_tools::policy_gate::CapSnapshot;
use nexo_dispatch_tools::{
    agent_status, cancel_agent, list_agents, program_phase_dispatch, AgentStatusInput,
    CancelAgentInput, ListAgentsInput, ProgramPhaseInput, ProgramPhaseOutput,
};
use nexo_driver_claude::{
    ClaudeConfig, ClaudeDefaultArgs, DispatcherIdentity, MemoryBindingStore,
};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_driver_types::GoalId;
use nexo_project_tracker::{FsProjectTracker, ProjectTracker};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt::init();
    match run().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("nexo-driver-tools: {e:#}");
            ExitCode::from(2)
        }
    }
}

async fn run() -> Result<ExitCode> {
    let mut args = std::env::args().skip(1);
    let cmd = args
        .next()
        .ok_or_else(|| anyhow!("missing subcommand (status | dispatch | agents)"))?;
    match cmd.as_str() {
        "status" => cmd_status(args).await,
        "dispatch" => cmd_dispatch(args).await,
        "agents" => cmd_agents(args).await,
        "-h" | "--help" => {
            print_help();
            Ok(ExitCode::SUCCESS)
        }
        other => Err(anyhow!("unknown subcommand: {other}")),
    }
}

fn print_help() {
    eprintln!(
        "nexo-driver-tools status [--phase <id> | --followups]\n\
         nexo-driver-tools dispatch <phase_id>\n\
         nexo-driver-tools agents list [--filter <status>]\n\
         nexo-driver-tools agents show <goal_id>\n\
         nexo-driver-tools agents cancel <goal_id> [--reason <text>]"
    );
}

fn workspace_root() -> PathBuf {
    std::env::var_os("NEXO_PROJECT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

fn open_tracker() -> Result<FsProjectTracker> {
    Ok(FsProjectTracker::open(workspace_root())?)
}

async fn cmd_status(mut args: impl Iterator<Item = String>) -> Result<ExitCode> {
    let mut phase: Option<String> = None;
    let mut followups = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--phase" => {
                phase = Some(args.next().ok_or_else(|| anyhow!("--phase needs id"))?);
            }
            "--followups" => followups = true,
            other => return Err(anyhow!("unknown flag: {other}")),
        }
    }
    let tracker = open_tracker()?;
    if let Some(id) = phase {
        match tracker.phase_detail(&id).await? {
            Some(s) => {
                println!("{} — {}", s.id, s.title);
                if let Some(b) = s.body {
                    println!();
                    println!("{b}");
                }
            }
            None => {
                eprintln!("phase {id} not found");
                return Ok(ExitCode::from(1));
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    if followups {
        let items = tracker.followups().await?;
        for i in items
            .iter()
            .filter(|i| i.status == nexo_project_tracker::FollowUpStatus::Open)
        {
            println!("{} [{}] — {}", i.code, i.section, i.title);
        }
        return Ok(ExitCode::SUCCESS);
    }
    match tracker.current_phase().await? {
        Some(s) => println!("{} {} — {}", glyph(s.status), s.id, s.title),
        None => println!("everything done — no active or pending phase"),
    }
    Ok(ExitCode::SUCCESS)
}

fn glyph(s: nexo_project_tracker::PhaseStatus) -> &'static str {
    match s {
        nexo_project_tracker::PhaseStatus::Done => "✅",
        nexo_project_tracker::PhaseStatus::InProgress => "🔄",
        nexo_project_tracker::PhaseStatus::Pending => "⬜",
    }
}

async fn build_orch_default() -> Result<Arc<DriverOrchestrator>> {
    let socket =
        std::env::temp_dir().join(format!("nexo-driver-tools-{}.sock", std::process::id()));
    let workspace_root = std::env::var_os("NEXO_DRIVER_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("claude-runs"));
    let claude_cfg = ClaudeConfig {
        binary: None,
        default_args: ClaudeDefaultArgs::default(),
        mcp_config: None,
        forced_kill_after: std::time::Duration::from_secs(1),
        turn_timeout: std::time::Duration::from_secs(60 * 10),
    };
    Ok(Arc::new(
        DriverOrchestrator::builder()
            .claude_config(claude_cfg)
            .binding_store(Arc::new(MemoryBindingStore::new())
                as Arc<dyn nexo_driver_claude::SessionBindingStore>)
            .decider(Arc::new(AllowAllDecider) as Arc<dyn PermissionDecider>)
            .workspace_manager(Arc::new(WorkspaceManager::new(&workspace_root)))
            .event_sink(Arc::new(NoopEventSink))
            .bin_path(PathBuf::from("/usr/local/bin/nexo-driver-permission-mcp"))
            .socket_path(socket)
            .build()
            .await?,
    ))
}

async fn cmd_dispatch(mut args: impl Iterator<Item = String>) -> Result<ExitCode> {
    let phase_id = args
        .next()
        .ok_or_else(|| anyhow!("dispatch: <phase_id> required"))?;
    while args.next().is_some() {
        // Reserved: --budget-turns / --hook flags — wired in 67.H.x.
    }
    let tracker = open_tracker()?;
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let orch = build_orch_default().await?;

    let policy = DispatchPolicy {
        mode: nexo_config::DispatchCapability::Full,
        ..Default::default()
    };
    let dispatcher = DispatcherIdentity {
        agent_id: "console".into(),
        sender_id: std::env::var("USER").ok(),
        parent_goal_id: None,
        chain_depth: 0,
    };
    let out = program_phase_dispatch(
        ProgramPhaseInput {
            phase_id: phase_id.clone(),
            acceptance_override: None,
            budget_override: None,
        },
        &tracker,
        orch,
        registry,
        &policy,
        false, // require_trusted=false for console — already authorised by shell access.
        true,
        dispatcher,
        Some(nexo_driver_claude::OriginChannel {
            plugin: "console".into(),
            instance: hostname().unwrap_or_else(|| "localhost".into()),
            sender_id: std::env::var("USER").unwrap_or_else(|_| "uid".into()),
            correlation_id: None,
        }),
        CapSnapshot {
            queue_when_full: true,
            ..Default::default()
        },
    )
    .await
    .map_err(|e| anyhow!("dispatch failed: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&out)?);
    let code = match out {
        ProgramPhaseOutput::Dispatched { .. } | ProgramPhaseOutput::Queued { .. } => {
            ExitCode::SUCCESS
        }
        _ => ExitCode::from(1),
    };
    Ok(code)
}

fn hostname() -> Option<String> {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
}

async fn cmd_agents(mut args: impl Iterator<Item = String>) -> Result<ExitCode> {
    let sub = args
        .next()
        .ok_or_else(|| anyhow!("agents: subcommand required (list | show | cancel)"))?;
    // The CLI can't share a registry with a long-running daemon
    // here — that's the binary refactor in 67.H.x. For now this
    // helper renders against a fresh in-memory registry just to
    // exercise the tool plumbing end-to-end.
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    match sub.as_str() {
        "list" => {
            let mut filter: Option<String> = None;
            while let Some(a) = args.next() {
                if a == "--filter" {
                    filter = args.next();
                }
            }
            let out = list_agents(
                ListAgentsInput {
                    filter,
                    phase_prefix: None,
                },
                registry,
            )
            .await;
            println!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        "show" => {
            let id = args
                .next()
                .ok_or_else(|| anyhow!("agents show: <goal_id> required"))?;
            let goal = parse_goal(&id)?;
            let out = agent_status(AgentStatusInput { goal_id: goal }, registry).await;
            println!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        "cancel" => {
            let id = args
                .next()
                .ok_or_else(|| anyhow!("agents cancel: <goal_id> required"))?;
            let mut reason: Option<String> = None;
            while let Some(a) = args.next() {
                if a == "--reason" {
                    reason = args.next();
                }
            }
            let goal = parse_goal(&id)?;
            let orch = build_orch_default().await?;
            let out = cancel_agent(
                CancelAgentInput {
                    goal_id: goal,
                    reason,
                },
                orch,
                registry,
            )
            .await
            .map_err(|e| anyhow!("cancel failed: {e}"))?;
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(ExitCode::SUCCESS)
        }
        other => Err(anyhow!("unknown agents subcommand: {other}")),
    }
}

fn parse_goal(s: &str) -> Result<GoalId> {
    let u = uuid::Uuid::parse_str(s)?;
    Ok(GoalId(u))
}
