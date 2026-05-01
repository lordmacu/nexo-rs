//! Budget exhaustion: max_turns=1, mock-claude that emits init only
//! (no Result::Success). After one turn the next loop iteration sees
//! `usage.turns >= max_turns` and returns BudgetExhausted{Turns}.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexo_driver_claude::{ClaudeConfig, ClaudeDefaultArgs, MemoryBindingStore, OutputFormat};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_driver_types::{
    AcceptanceCriterion, AttemptOutcome, BudgetAxis, BudgetGuards, Goal, GoalId,
};
use uuid::Uuid;

fn mock_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../driver-claude/tests/fixtures/mock-claude.sh")
}

#[tokio::test]
async fn budget_turns_axis_fires() {
    // Use a fixture that DOES emit Result::Success but with
    // max_turns=1; after the first turn (Done if acceptance passed),
    // the loop never re-enters. To force budget axis, make the goal
    // require a retry: scripted ScriptedAcceptanceEvaluator that
    // always fails. With max_turns=1, after attempt 1's NeedsRetry,
    // total_turns=1, loop checks usage.turns(1) >= max_turns(1),
    // emits BudgetExhausted{Turns}.
    use async_trait::async_trait;
    use chrono::Utc;
    use nexo_driver_loop::AcceptanceEvaluator;
    use nexo_driver_types::{AcceptanceFailure, AcceptanceVerdict};

    struct AlwaysFail;
    #[async_trait]
    impl AcceptanceEvaluator for AlwaysFail {
        async fn evaluate(
            &self,
            _criteria: &[AcceptanceCriterion],
            _workspace: &std::path::Path,
        ) -> Result<AcceptanceVerdict, nexo_driver_loop::DriverError> {
            Ok(AcceptanceVerdict {
                met: false,
                failures: vec![AcceptanceFailure {
                    criterion_index: 0,
                    criterion_label: "test".into(),
                    message: "scripted fail".into(),
                    evidence: None,
                }],
                evaluated_at: Utc::now(),
                elapsed_ms: 0,
            })
        }
    }

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

    let g = Goal {
        id: GoalId(Uuid::new_v4()),
        description: mock_path().display().to_string(),
        acceptance: vec![AcceptanceCriterion::shell("true")],
        budget: BudgetGuards {
            max_turns: 1,
            max_wall_time: Duration::from_secs(30),
            max_tokens: 100_000,
            max_consecutive_denies: 3,
            max_consecutive_errors: 5,
            max_consecutive_413: 2,
        },
        workspace: None,
        metadata: serde_json::Map::new(),
    };

    std::env::set_var(
        "MOCK_FIXTURE",
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../driver-claude/tests/fixtures/init_assistant_result.jsonl"),
    );

    let binding: Arc<dyn nexo_driver_claude::SessionBindingStore> =
        Arc::new(MemoryBindingStore::new());
    let workspace_manager = Arc::new(WorkspaceManager::new(&workspace_root));
    let decider: Arc<dyn PermissionDecider> = Arc::new(AllowAllDecider);
    let event_sink: Arc<dyn nexo_driver_loop::DriverEventSink> = Arc::new(NoopEventSink);
    let acceptance: Arc<dyn AcceptanceEvaluator> = Arc::new(AlwaysFail);

    let orch = DriverOrchestrator::builder()
        .claude_config(claude_cfg)
        .binding_store(binding)
        .acceptance(acceptance)
        .decider(decider)
        .workspace_manager(workspace_manager)
        .event_sink(event_sink)
        .bin_path(PathBuf::from("/usr/local/bin/nexo-driver-permission-mcp"))
        .socket_path(socket_path)
        .build()
        .await
        .unwrap();

    let outcome = orch.run_goal(g).await.unwrap();
    match outcome.outcome {
        AttemptOutcome::BudgetExhausted {
            axis: BudgetAxis::Turns,
        } => {}
        other => panic!("expected BudgetExhausted{{Turns}}, got {other:?}"),
    }
    let _ = orch.shutdown().await;
    std::env::remove_var("MOCK_FIXTURE");
}
