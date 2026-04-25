//! Confirms the YAML shape published in the spec deserialises into
//! `Goal` without manual conversions. The driver loop (Phase 67.4) is
//! the runtime consumer; here we lock the contract.

use std::time::Duration;

use nexo_driver_types::{AcceptanceCriterion, Goal};

const GOAL_YAML: &str = r#"
id: "00000000-0000-0000-0000-000000000000"
description: "Implementa Phase 26.z (tunnel.url integration)"
workspace: "/tmp/claude-runs/26-z"
budget:
  max_turns: 40
  max_wall_time: 90m
  max_tokens: 250000
  max_consecutive_denies: 5
acceptance:
  - kind: shell_command
    command: "cargo build --workspace --locked"
  - kind: shell_command
    command: "cargo test --workspace --locked"
    timeout_secs: 600
  - kind: shell_command
    command: "cargo clippy --workspace --locked --all-targets -- -D warnings"
  - kind: file_matches
    path: "proyecto/PHASES.md"
    regex: '#### 26\.z .* ✅'
"#;

#[test]
fn spec_yaml_loads_into_goal() {
    let goal: Goal = serde_yaml::from_str(GOAL_YAML).expect("parse");
    assert_eq!(goal.acceptance.len(), 4);
    assert_eq!(goal.budget.max_wall_time, Duration::from_secs(90 * 60));
    assert_eq!(goal.budget.max_turns, 40);
    assert!(matches!(
        &goal.acceptance[3],
        AcceptanceCriterion::FileMatches { path, .. } if path == "proyecto/PHASES.md"
    ));
}
