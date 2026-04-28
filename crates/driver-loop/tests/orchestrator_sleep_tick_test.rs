//! Phase 77.20.2 — when a turn returns the Sleep sentinel, the
//! orchestrator must park the goal, wake it, and prepend a synthetic
//! `<tick>` prompt to the next turn.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexo_driver_claude::{ClaudeConfig, ClaudeDefaultArgs, MemoryBindingStore, OutputFormat};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_driver_types::{AcceptanceCriterion, AttemptOutcome, BudgetGuards, Goal, GoalId};
use uuid::Uuid;

fn write_multi_mock(dir: &std::path::Path) -> PathBuf {
    let script = dir.join("sleep-multi-mock.sh");
    let body = r#"#!/usr/bin/env bash
set -u

counter_file="${MOCK_COUNTER_FILE:?MOCK_COUNTER_FILE must be set}"
prompt_log="${MOCK_PROMPT_LOG:?MOCK_PROMPT_LOG must be set}"

prompt=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -p)
            prompt="${2:-}"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

printf '%s\n===TURN===\n' "$prompt" >> "$prompt_log"

n=0
if [[ -f "$counter_file" ]]; then
    n=$(cat "$counter_file")
fi
next=$((n + 1))
printf '%s' "$next" > "$counter_file"

var="MOCK_FIXTURE_T${n}"
fixture="${!var:-}"
if [[ -z "$fixture" || ! -f "$fixture" ]]; then
    echo "sleep-multi-mock.sh: $var unset or missing ($fixture)" >&2
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
async fn sleep_outcome_wakes_with_tick_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_root = dir.path().to_path_buf();
    let socket_path = dir.path().join("driver.sock");
    let counter_file = dir.path().join("mock-counter");
    let prompt_log = dir.path().join("prompt-log");
    let mock_script = write_multi_mock(dir.path());

    let sleep_fixture = dir.path().join("sleep-result.jsonl");
    std::fs::write(
        &sleep_fixture,
        concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sess-sleep\",\"cwd\":\"/tmp/work\",\"model\":\"claude-sonnet-4-6\",\"tools\":[\"Sleep\"],\"mcp_servers\":[{\"name\":\"nexo-driver\",\"status\":\"connected\"}],\"permission_mode\":\"default\",\"api_key_source\":\"env\"}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"duration_ms\":1,\"duration_api_ms\":1,\"is_error\":false,\"num_turns\":1,\"result\":\"{\\\"__nexo_sleep__\\\":true,\\\"duration_ms\\\":1,\\\"reason\\\":\\\"waiting for new work\\\"}\",\"session_id\":\"sess-sleep\",\"total_cost_usd\":0,\"usage\":{\"input_tokens\":1,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":1,\"service_tier\":\"standard\"}}\n"
        ),
    )
    .unwrap();

    let done_fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../driver-claude/tests/fixtures/init_assistant_result.jsonl");

    let claude_cfg = ClaudeConfig {
        binary: Some(mock_script),
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

    let goal = Goal {
        id: GoalId(Uuid::new_v4()),
        description: "check for proactive work".into(),
        acceptance: vec![AcceptanceCriterion::shell("true")],
        budget: BudgetGuards {
            max_turns: 5,
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

    std::env::set_var("MOCK_COUNTER_FILE", &counter_file);
    std::env::set_var("MOCK_PROMPT_LOG", &prompt_log);
    std::env::set_var("MOCK_FIXTURE_T0", &sleep_fixture);
    std::env::set_var("MOCK_FIXTURE_T1", &done_fixture);

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

    let outcome = orch.run_goal(goal).await.unwrap();
    assert!(matches!(outcome.outcome, AttemptOutcome::Done));
    assert_eq!(outcome.total_turns, 2);

    let prompts = std::fs::read_to_string(&prompt_log).unwrap();
    assert!(prompts.contains("<tick>"), "prompts were: {prompts}");
    assert!(
        prompts.contains("kind: sleep_wake"),
        "prompts were: {prompts}"
    );
    assert!(
        prompts.contains("reason: waiting for new work"),
        "prompts were: {prompts}"
    );

    let _ = orch.shutdown().await;

    std::env::remove_var("MOCK_COUNTER_FILE");
    std::env::remove_var("MOCK_PROMPT_LOG");
    std::env::remove_var("MOCK_FIXTURE_T0");
    std::env::remove_var("MOCK_FIXTURE_T1");
}
