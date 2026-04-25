//! End-to-end happy path: mock-claude returns `result.success`,
//! AllowAllDecider passes every tool call, NoopAcceptanceEvaluator
//! passes the goal, orchestrator returns `Done` after one turn.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexo_driver_claude::{ClaudeConfig, ClaudeDefaultArgs, MemoryBindingStore, OutputFormat};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_driver_types::{AcceptanceCriterion, AttemptOutcome, BudgetGuards, Goal, GoalId};
use uuid::Uuid;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../driver-claude/tests/fixtures")
        .join(name)
}

fn mock_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../driver-claude/tests/fixtures/mock-claude.sh")
}

#[tokio::test]
async fn happy_goal_done_in_one_turn() {
    let fix = fixture("init_assistant_result.jsonl");
    let dir = tempfile::tempdir().unwrap();
    let workspace_root = dir.path().to_path_buf();
    let socket_path = dir.path().join("driver.sock");

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

    // Inject the mock script as the prompt arg + env-var fixture
    // selector. The bin we'll run is `bash <mock-claude.sh>`, which
    // means our ClaudeCommand must end up invoking bash with the
    // script as the prompt (mock ignores its argv and reads
    // MOCK_FIXTURE).
    //
    // Trick: ClaudeCommand::new(binary="bash", prompt=mock-claude path).
    // The orchestrator passes `goal.description` as the prompt. So we
    // craft the goal description = mock script path. Side effect:
    // `compose_turn_prompt` adds "[Turn N/M]" suffix; bash treats
    // that as extra args and discards.
    let g = Goal {
        id: GoalId(Uuid::new_v4()),
        description: mock_path().display().to_string(),
        acceptance: vec![AcceptanceCriterion::shell("true")],
        budget: BudgetGuards {
            max_turns: 3,
            max_wall_time: Duration::from_secs(30),
            max_tokens: 100_000,
            max_consecutive_denies: 3,
            max_consecutive_errors: 5,
        },
        workspace: None,
        metadata: serde_json::Map::new(),
    };

    let binding: Arc<dyn nexo_driver_claude::SessionBindingStore> =
        Arc::new(MemoryBindingStore::new());
    let workspace_manager = Arc::new(WorkspaceManager::new(&workspace_root));
    let decider: Arc<dyn PermissionDecider> = Arc::new(AllowAllDecider);
    let event_sink: Arc<dyn nexo_driver_loop::DriverEventSink> = Arc::new(NoopEventSink);

    // Set MOCK_FIXTURE env so the mock script reads our fixture.
    std::env::set_var("MOCK_FIXTURE", &fix);

    let orch = DriverOrchestrator::builder()
        .claude_config(claude_cfg)
        .binding_store(binding)
        .decider(decider)
        .workspace_manager(workspace_manager)
        .event_sink(event_sink)
        .bin_path(PathBuf::from("/usr/local/bin/nexo-driver-permission-mcp"))
        .socket_path(socket_path)
        .build()
        .await
        .unwrap();

    let outcome = orch.run_goal(g).await.unwrap();
    assert!(
        matches!(outcome.outcome, AttemptOutcome::Done),
        "expected Done, got {:?}",
        outcome.outcome,
    );
    assert_eq!(outcome.total_turns, 1);
    let _ = orch.shutdown().await;

    std::env::remove_var("MOCK_FIXTURE");
}
