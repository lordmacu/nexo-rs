//! Round-trip every public type through serde_json so the wire format
//! stays stable. If any of these break, NATS consumers break.

use std::time::Duration;

use chrono::{TimeZone, Utc};
use nexo_driver_types::*;
use uuid::Uuid;

fn rt<T>(value: &T) -> T
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de>,
{
    let s = serde_json::to_string(value).expect("serialize");
    serde_json::from_str::<T>(&s).expect("deserialize")
}

fn budget() -> BudgetGuards {
    BudgetGuards {
        max_turns: 40,
        max_wall_time: Duration::from_secs(5400),
        max_tokens: 250_000,
        max_consecutive_denies: 5,
        max_consecutive_errors: 5,
            max_consecutive_413: 2,
    }
}

fn goal() -> Goal {
    Goal {
        id: GoalId(Uuid::nil()),
        description: "implementa Phase 26.z".into(),
        acceptance: vec![
            AcceptanceCriterion::shell("cargo build --workspace --locked"),
            AcceptanceCriterion::file("PHASES.md", r"26\.z.*✅"),
            AcceptanceCriterion::Custom {
                name: "no_secrets_touched".into(),
                args: serde_json::json!({"prefixes":["secrets/"]}),
            },
        ],
        budget: budget(),
        workspace: Some("/tmp/claude-runs/26-z".into()),
        metadata: serde_json::Map::new(),
    }
}

#[test]
fn roundtrip_goal() {
    let g = goal();
    assert_eq!(rt(&g), g);
}

#[test]
fn roundtrip_budget_usage() {
    let u = BudgetUsage {
        turns: 3,
        wall_time: Duration::from_secs(120),
        tokens: 12_345,
        consecutive_denies: 1,
        consecutive_errors: 0,
        consecutive_413: 0,
    };
    assert_eq!(rt(&u), u);
}

#[test]
fn roundtrip_budget_axis_each_variant() {
    for axis in [
        BudgetAxis::Turns,
        BudgetAxis::WallTime,
        BudgetAxis::Tokens,
        BudgetAxis::ConsecutiveDenies,
    ] {
        assert_eq!(rt(&axis), axis);
    }
}

#[test]
fn roundtrip_acceptance_verdict() {
    let v = AcceptanceVerdict {
        met: false,
        failures: vec![AcceptanceFailure {
            criterion_index: 1,
            criterion_label: "cargo test".into(),
            message: "test result: FAILED".into(),
            evidence: Some("---- t::a stdout ----".into()),
        }],
        evaluated_at: Utc.with_ymd_and_hms(2026, 4, 25, 18, 0, 0).unwrap(),
        elapsed_ms: 1234,
    };
    assert_eq!(rt(&v), v);
}

#[test]
fn roundtrip_decision_each_choice() {
    let base = |choice: DecisionChoice| Decision {
        id: DecisionId(Uuid::nil()),
        goal_id: GoalId(Uuid::nil()),
        turn_index: 2,
        tool: "Edit".into(),
        input: serde_json::json!({"file":"src/lib.rs"}),
        choice,
        rationale: "looks correct vs. spec".into(),
        decided_at: Utc.with_ymd_and_hms(2026, 4, 25, 18, 0, 0).unwrap(),
    };
    for choice in [
        DecisionChoice::Allow,
        DecisionChoice::Deny {
            message: "destructive".into(),
        },
        DecisionChoice::Observe {
            note: "shadow".into(),
        },
    ] {
        let d = base(choice);
        assert_eq!(rt(&d), d);
    }
}

#[test]
fn roundtrip_attempt_outcome_each_variant() {
    let variants = vec![
        AttemptOutcome::Done,
        AttemptOutcome::NeedsRetry { failures: vec![] },
        AttemptOutcome::Continue {
            reason: "tool result pending".into(),
        },
        AttemptOutcome::BudgetExhausted {
            axis: BudgetAxis::Turns,
        },
        AttemptOutcome::Cancelled,
        AttemptOutcome::Escalate {
            reason: "stuck".into(),
        },
    ];
    for v in variants {
        assert_eq!(rt(&v), v);
    }
}

#[test]
fn roundtrip_attempt_result() {
    let r = AttemptResult {
        goal_id: GoalId(Uuid::nil()),
        turn_index: 7,
        outcome: AttemptOutcome::Done,
        decisions_recorded: vec![],
        usage_after: BudgetUsage::default(),
        acceptance: None,
        final_text: Some("ready".into()),
        harness_extras: serde_json::Map::new(),
    };
    assert_eq!(rt(&r), r);
}

#[test]
fn roundtrip_compact_result_both() {
    let a = CompactResult::Compacted {
        tokens_saved: 1234,
        summary: "dropped pre-test logs".into(),
    };
    let b = CompactResult::skipped("not needed");
    assert_eq!(rt(&a), a);
    assert_eq!(rt(&b), b);
}

#[test]
fn roundtrip_reset_params_each_reason() {
    for reason in [
        ResetReason::New,
        ResetReason::Reset,
        ResetReason::Idle,
        ResetReason::Daily,
        ResetReason::Compaction,
        ResetReason::Deleted,
        ResetReason::Unknown,
    ] {
        let p = ResetParams {
            goal_id: Some(GoalId(Uuid::nil())),
            reason,
        };
        assert_eq!(rt(&p), p);
    }
}

#[test]
fn roundtrip_support_both() {
    let a = Support::Supported {
        priority: 100,
        reason: Some("default for anthropic".into()),
    };
    let b = Support::Unsupported {
        reason: "wrong runtime".into(),
    };
    assert_eq!(rt(&a), a);
    assert_eq!(rt(&b), b);
}

#[test]
fn roundtrip_support_context_each_runtime() {
    for runtime in [
        HarnessRuntime::Local,
        HarnessRuntime::Subprocess,
        HarnessRuntime::Http,
        HarnessRuntime::Ws,
    ] {
        let c = SupportContext {
            provider: "anthropic".into(),
            model_id: Some("claude-sonnet-4-6".into()),
            runtime,
        };
        assert_eq!(rt(&c), c);
    }
}
