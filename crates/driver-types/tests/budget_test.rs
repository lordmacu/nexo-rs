use std::time::Duration;

use nexo_driver_types::{BudgetAxis, BudgetGuards, BudgetUsage};

fn guards() -> BudgetGuards {
    BudgetGuards {
        max_turns: 10,
        max_wall_time: Duration::from_secs(60),
        max_tokens: 1000,
        max_consecutive_denies: 3,
        max_consecutive_errors: 5,
    }
}

#[test]
fn fresh_usage_does_not_exhaust() {
    assert_eq!(guards().is_exhausted(&BudgetUsage::default()), None);
}

#[test]
fn each_axis_exhausts_independently() {
    let g = guards();

    let u = BudgetUsage {
        turns: 10,
        ..Default::default()
    };
    assert_eq!(g.is_exhausted(&u), Some(BudgetAxis::Turns));

    let u = BudgetUsage {
        wall_time: Duration::from_secs(60),
        ..Default::default()
    };
    assert_eq!(g.is_exhausted(&u), Some(BudgetAxis::WallTime));

    let u = BudgetUsage {
        tokens: 1000,
        ..Default::default()
    };
    assert_eq!(g.is_exhausted(&u), Some(BudgetAxis::Tokens));

    let u = BudgetUsage {
        consecutive_denies: 3,
        consecutive_errors: 0,
        ..Default::default()
    };
    assert_eq!(g.is_exhausted(&u), Some(BudgetAxis::ConsecutiveDenies));
}

#[test]
fn turns_axis_takes_precedence_when_multiple_exhausted() {
    // Document the ordering — turns wins over wall_time if both fire
    // at the same evaluation. Caller relies on this for "which axis
    // killed me" telemetry.
    let g = guards();
    let u = BudgetUsage {
        turns: 99,
        wall_time: Duration::from_secs(99),
        tokens: 9999,
        consecutive_denies: 99,
        consecutive_errors: 0,
    };
    assert_eq!(g.is_exhausted(&u), Some(BudgetAxis::Turns));
}

#[test]
fn under_limit_returns_none() {
    let g = guards();
    let u = BudgetUsage {
        turns: 9,
        wall_time: Duration::from_secs(59),
        tokens: 999,
        consecutive_denies: 2,
        consecutive_errors: 0,
    };
    assert_eq!(g.is_exhausted(&u), None);
}

#[test]
fn consecutive_errors_axis_fires_when_cap_nonzero() {
    let g = guards();
    let u = BudgetUsage {
        consecutive_errors: 5,
        ..Default::default()
    };
    assert_eq!(g.is_exhausted(&u), Some(BudgetAxis::ConsecutiveErrors));
}

#[test]
fn consecutive_errors_axis_disabled_when_cap_is_zero() {
    let mut g = guards();
    g.max_consecutive_errors = 0;
    let u = BudgetUsage {
        consecutive_errors: 999,
        ..Default::default()
    };
    assert_eq!(g.is_exhausted(&u), None);
}

#[test]
fn equal_to_limit_exhausts() {
    // `>=` semantics — at the limit counts as exhausted.
    let g = guards();
    let u = BudgetUsage {
        turns: 10,
        ..Default::default()
    };
    assert!(g.is_exhausted(&u).is_some());
}
