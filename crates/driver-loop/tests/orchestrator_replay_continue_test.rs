//! Phase 78.2 — synthetic test that an `AttemptOutcome::Continue`
//! routed through `DefaultReplayPolicy::NextTurn` actually fires
//! turn N+1 instead of stalling on turn N.
//!
//! Setup:
//!   * Turn 0 — mock-claude emits init+assistant only, no `Result`
//!     event. `attempt::run_attempt` returns
//!     `Continue { reason: "stream ended without result event" }`.
//!   * Replay policy classifies that message → `NextTurn` (no
//!     "session"/"timeout"/"rate limit" keywords).
//!   * Turn 1 — mock-claude emits a full success fixture.
//!     orchestrator returns `Done` with `total_turns == 2`.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexo_driver_claude::{ClaudeConfig, ClaudeDefaultArgs, MemoryBindingStore, OutputFormat};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_driver_types::{AcceptanceCriterion, AttemptOutcome, BudgetGuards, Goal, GoalId};
use std::os::unix::fs::PermissionsExt;
use uuid::Uuid;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../driver-claude/tests/fixtures")
        .join(name)
}

/// Writes a per-test bash script that picks `${MOCK_FIXTURE_T<N>}`
/// based on a counter file, increments the counter, then `cat`s the
/// chosen fixture. Stateless across processes — counter lives on
/// disk so each spawn of the script advances.
fn write_multi_mock(dir: &std::path::Path) -> PathBuf {
    let script = dir.join("multi-mock.sh");
    let body = r#"#!/usr/bin/env bash
# Phase 78.2 multi-turn mock: picks $MOCK_FIXTURE_T<N> per call.
set -u
counter_file="${MOCK_COUNTER_FILE:?MOCK_COUNTER_FILE must be set}"
n=0
if [[ -f "$counter_file" ]]; then
    n=$(cat "$counter_file")
fi
next=$((n + 1))
echo "$next" > "$counter_file"
var="MOCK_FIXTURE_T${n}"
fixture="${!var:-}"
if [[ -z "$fixture" || ! -f "$fixture" ]]; then
    echo "multi-mock.sh: $var unset or missing ($fixture)" >&2
    exit 2
fi
cat "$fixture"
"#;
    std::fs::write(&script, body).unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    script
}

#[tokio::test]
async fn continue_outcome_advances_to_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_root = dir.path().to_path_buf();
    let socket_path = dir.path().join("driver.sock");
    let counter_file = dir.path().join("mock-counter");
    let mock_script = write_multi_mock(dir.path());

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

    // Goal description = path to multi-mock.sh. ClaudeCommand spawns
    // `bash <description>`; the script ignores its args and reads
    // its env to pick a fixture per invocation.
    let g = Goal {
        id: GoalId(Uuid::new_v4()),
        description: mock_script.display().to_string(),
        acceptance: vec![AcceptanceCriterion::shell("true")],
        budget: BudgetGuards {
            max_turns: 5,
            max_wall_time: Duration::from_secs(30),
            max_tokens: 100_000,
            max_consecutive_denies: 3,
            max_consecutive_errors: 5,
            max_consecutive_413: 2,
        },
        workspace: None,
        metadata: serde_json::Map::new(),
    };

    let binding: Arc<dyn nexo_driver_claude::SessionBindingStore> =
        Arc::new(MemoryBindingStore::new());
    let workspace_manager = Arc::new(WorkspaceManager::new(&workspace_root));
    let decider: Arc<dyn PermissionDecider> = Arc::new(AllowAllDecider);
    let event_sink: Arc<dyn nexo_driver_loop::DriverEventSink> = Arc::new(NoopEventSink);

    std::env::set_var("MOCK_COUNTER_FILE", &counter_file);
    std::env::set_var("MOCK_FIXTURE_T0", fixture("continue_no_result.jsonl"));
    std::env::set_var("MOCK_FIXTURE_T1", fixture("init_assistant_result.jsonl"));

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
        "expected Done after Continue → NextTurn, got {:?}",
        outcome.outcome,
    );
    assert_eq!(
        outcome.total_turns, 2,
        "Continue on turn 0 must advance to turn 1, then Done — total_turns should be 2",
    );
    let _ = orch.shutdown().await;

    std::env::remove_var("MOCK_COUNTER_FILE");
    std::env::remove_var("MOCK_FIXTURE_T0");
    std::env::remove_var("MOCK_FIXTURE_T1");
}
