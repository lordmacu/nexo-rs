//! `nexo-driver` CLI — runs goals defined in YAML against the
//! configured DriverOrchestrator.
//!
//! Usage:
//!   nexo-driver run <goal-yaml> [--config <claude.yaml>] [--no-events]
//!   nexo-driver list-active     [--config <claude.yaml>]
//!
//! Exit codes:
//!   0  Done
//!   1  BudgetExhausted | Escalate | NeedsRetry
//!   2  Cancelled | Continue | DriverError

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use nexo_driver_claude::{MemoryBindingStore, SessionBindingStore, SqliteBindingStore};
use nexo_driver_loop::{
    AcceptanceEvaluator, BindingStoreKind, DeciderConfig, DefaultAcceptanceEvaluator, DriverConfig,
    DriverOrchestrator, NoopEventSink, WorkspaceManager,
};
use nexo_driver_permission::{AllowAllDecider, DenyAllDecider, PermissionDecider};
use nexo_driver_types::Goal;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt::init();
    match run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("nexo-driver: {e:#}");
            ExitCode::from(2)
        }
    }
}

async fn run() -> Result<ExitCode> {
    let mut args = std::env::args().skip(1);
    let cmd = args
        .next()
        .ok_or_else(|| anyhow!("missing subcommand (run | list-active)"))?;
    match cmd.as_str() {
        "run" => cmd_run(args).await,
        "list-active" => cmd_list_active(args).await,
        "-h" | "--help" => {
            print_help();
            Ok(ExitCode::SUCCESS)
        }
        other => Err(anyhow!("unknown subcommand: {other}")),
    }
}

fn print_help() {
    eprintln!(
        "nexo-driver run <goal-yaml> [--config <claude.yaml>] [--no-events]\n\
         nexo-driver list-active [--config <claude.yaml>]"
    );
}

async fn cmd_run(mut args: impl Iterator<Item = String>) -> Result<ExitCode> {
    let goal_path = args
        .next()
        .ok_or_else(|| anyhow!("run: <goal-yaml> required"))?;
    let mut config_path: Option<PathBuf> = None;
    let mut no_events = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => {
                config_path = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--config requires path"))?
                        .into(),
                );
            }
            "--no-events" => no_events = true,
            other => return Err(anyhow!("unknown flag: {other}")),
        }
    }
    let cfg = load_config(config_path.as_deref())?;
    let goal_raw = std::fs::read_to_string(&goal_path)?;
    let goal: Goal = serde_yaml::from_str(&goal_raw).map_err(|e| anyhow!("goal yaml: {e}"))?;
    if goal.id.0.is_nil() {
        return Err(anyhow!("goal id must not be the nil UUID"));
    }
    let orchestrator = build_orchestrator(&cfg, no_events).await?;
    let outcome = orchestrator.run_goal(goal).await?;
    let json = serde_json::to_string_pretty(&outcome)?;
    println!("{json}");
    let code = match outcome.outcome {
        nexo_driver_types::AttemptOutcome::Done => ExitCode::SUCCESS,
        nexo_driver_types::AttemptOutcome::BudgetExhausted { .. }
        | nexo_driver_types::AttemptOutcome::Escalate { .. }
        | nexo_driver_types::AttemptOutcome::NeedsRetry { .. } => ExitCode::from(1),
        _ => ExitCode::from(2),
    };
    let _ = orchestrator.shutdown().await;
    Ok(code)
}

async fn cmd_list_active(mut args: impl Iterator<Item = String>) -> Result<ExitCode> {
    let mut config_path: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => {
                config_path = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--config requires path"))?
                        .into(),
                );
            }
            other => return Err(anyhow!("unknown flag: {other}")),
        }
    }
    let cfg = load_config(config_path.as_deref())?;
    let store = open_binding_store(&cfg.binding_store).await?;
    let active = store.list_active().await?;
    println!("{}", serde_json::to_string_pretty(&active)?);
    Ok(ExitCode::SUCCESS)
}

fn load_config(path: Option<&std::path::Path>) -> Result<DriverConfig> {
    let path = path
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("NEXO_DRIVER_CONFIG").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("config/driver/claude.yaml"));
    Ok(DriverConfig::from_yaml_file(&path)?)
}

async fn open_binding_store(
    cfg: &nexo_driver_loop::BindingStoreConfig,
) -> Result<Arc<dyn SessionBindingStore>> {
    match cfg.kind {
        BindingStoreKind::Memory => {
            let s: Arc<dyn SessionBindingStore> = Arc::new(MemoryBindingStore::new());
            Ok(s)
        }
        BindingStoreKind::Sqlite => {
            let path = cfg
                .path
                .clone()
                .ok_or_else(|| anyhow!("sqlite binding_store requires path"))?;
            let mut store =
                SqliteBindingStore::open(path.to_str().ok_or_else(|| anyhow!("bad path"))?).await?;
            if let Some(t) = cfg.idle_ttl {
                store = store.with_idle_ttl(t);
            }
            if let Some(a) = cfg.max_age {
                store = store.with_max_age(a);
            }
            let s: Arc<dyn SessionBindingStore> = Arc::new(store);
            Ok(s)
        }
    }
}

async fn build_orchestrator(cfg: &DriverConfig, no_events: bool) -> Result<DriverOrchestrator> {
    let binding_store = open_binding_store(&cfg.binding_store).await?;
    let workspace_manager = Arc::new(WorkspaceManager::new(&cfg.workspace.root));
    let decider: Arc<dyn PermissionDecider> = match &cfg.permission.decider {
        DeciderConfig::AllowAll => Arc::new(AllowAllDecider),
        DeciderConfig::DenyAll { reason } => Arc::new(DenyAllDecider {
            reason: reason.clone(),
        }),
        DeciderConfig::Llm { .. } => {
            return Err(anyhow!(
                "DeciderConfig::Llm not yet wired here (needs llm config from broker layer)"
            ));
        }
    };
    let event_sink: Arc<dyn nexo_driver_loop::DriverEventSink> =
        if no_events || !cfg.driver.emit_nats_events {
            Arc::new(NoopEventSink)
        } else {
            // 67.4 ships emission via NoopEventSink by default in CLI;
            // an external runner that wants NATS will construct the
            // orchestrator programmatically with NatsEventSink.
            Arc::new(NoopEventSink)
        };
    // 67.5 — wire the real acceptance evaluator with operator-supplied
    // shell timeout / evidence cap, plus the two built-in custom
    // verifiers (no_paths_touched, git_clean).
    let mut acceptance = DefaultAcceptanceEvaluator::new();
    if let Some(t) = cfg.acceptance.default_shell_timeout {
        acceptance = acceptance.with_default_shell_timeout(t);
    }
    if let Some(n) = cfg.acceptance.evidence_byte_limit {
        acceptance = acceptance.with_evidence_byte_limit(n);
    }
    let acceptance: Arc<dyn AcceptanceEvaluator> = Arc::new(acceptance);

    let orch = DriverOrchestrator::builder()
        .claude_config(cfg.claude.clone())
        .binding_store(binding_store)
        .acceptance(acceptance)
        .decider(decider)
        .workspace_manager(workspace_manager)
        .event_sink(event_sink)
        .bin_path(cfg.driver.bin_path.clone())
        .socket_path(cfg.permission.socket.clone())
        .build()
        .await?;
    Ok(orch)
}
