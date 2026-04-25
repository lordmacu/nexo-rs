//! Compose the prompt fed to Claude per turn.

use nexo_driver_types::{AcceptanceFailure, BudgetGuards, Goal};

/// Build the user prompt for turn `turn_index` (0-based).
///
/// Layout:
///
/// ```text
/// <goal description>
///
/// Previous attempt failed:
///   1. <criterion_label>: <message>
///   2. ...
///
/// [Turn N/M]
/// ```
///
/// `prior_failures` is the verdict of the previous turn's acceptance
/// run (empty on turn 0). `budget` is read for the trailing hint.
pub fn compose_turn_prompt(
    goal: &Goal,
    turn_index: u32,
    prior_failures: &[AcceptanceFailure],
    budget: &BudgetGuards,
) -> String {
    let mut out = String::new();
    out.push_str(goal.description.trim());

    if !prior_failures.is_empty() {
        out.push_str("\n\nPrevious attempt failed:");
        for (i, f) in prior_failures.iter().enumerate() {
            out.push_str(&format!(
                "\n  {}. {}: {}",
                i + 1,
                f.criterion_label,
                f.message
            ));
        }
    }

    out.push_str(&format!(
        "\n\n[Turn {}/{}]",
        turn_index + 1,
        budget.max_turns
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_driver_types::{AcceptanceCriterion, BudgetGuards, Goal, GoalId};
    use std::time::Duration;
    use uuid::Uuid;

    fn goal_for(text: &str) -> Goal {
        Goal {
            id: GoalId(Uuid::nil()),
            description: text.into(),
            acceptance: vec![AcceptanceCriterion::shell("true")],
            budget: BudgetGuards {
                max_turns: 5,
                max_wall_time: Duration::from_secs(60),
                max_tokens: 1_000,
                max_consecutive_denies: 3,
                max_consecutive_errors: 5,
            },
            workspace: None,
            metadata: serde_json::Map::new(),
        }
    }

    #[test]
    fn turn_zero_no_failures_has_budget_suffix() {
        let g = goal_for("Implement Phase 26.z");
        let p = compose_turn_prompt(&g, 0, &[], &g.budget);
        assert!(p.starts_with("Implement Phase 26.z"));
        assert!(p.ends_with("[Turn 1/5]"));
        assert!(!p.contains("Previous attempt failed"));
    }

    #[test]
    fn retry_turn_includes_failures_numbered() {
        let g = goal_for("Goal X");
        let failures = vec![
            AcceptanceFailure {
                criterion_index: 0,
                criterion_label: "cargo test".into(),
                message: "test x failed".into(),
                evidence: None,
            },
            AcceptanceFailure {
                criterion_index: 1,
                criterion_label: "cargo clippy".into(),
                message: "warning Z".into(),
                evidence: None,
            },
        ];
        let p = compose_turn_prompt(&g, 1, &failures, &g.budget);
        assert!(p.contains("Previous attempt failed:"));
        assert!(p.contains("1. cargo test: test x failed"));
        assert!(p.contains("2. cargo clippy: warning Z"));
        assert!(p.ends_with("[Turn 2/5]"));
    }

    #[test]
    fn budget_suffix_uses_one_based_index() {
        let g = goal_for("X");
        let p = compose_turn_prompt(&g, 4, &[], &g.budget);
        assert!(p.ends_with("[Turn 5/5]"));
    }
}
