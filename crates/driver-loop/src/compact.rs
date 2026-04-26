//! Phase 67.9 — opportunistic `/compact`.
//!
//! `CompactPolicy::classify` returns `Some(focus_hint)` when the
//! orchestrator should inject a compact turn before continuing with
//! the next regular turn. The default rules-based impl fires when
//! `usage.tokens / context_window` crosses a threshold and at least
//! `min_turns_between_compacts` turns have elapsed since the prior
//! compact (anti-storm).

use async_trait::async_trait;
use nexo_driver_types::{BudgetUsage, GoalId};

const FOCUS_TRUNCATE_CHARS: usize = 140;

#[derive(Clone, Debug)]
pub struct CompactContext<'a> {
    pub goal_id: GoalId,
    pub turn_index: u32,
    pub usage: &'a BudgetUsage,
    pub context_window: u64,
    pub last_compact_turn: Option<u32>,
    pub goal_description: &'a str,
}

#[async_trait]
pub trait CompactPolicy: Send + Sync + 'static {
    /// `Some(focus_hint)` when a compact turn should be injected
    /// before the next regular turn. `None` to keep going.
    async fn classify(&self, ctx: &CompactContext<'_>) -> Option<String>;
}

pub struct DefaultCompactPolicy {
    pub enabled: bool,
    pub threshold: f64,
    pub min_turns_between_compacts: u32,
}

impl Default for DefaultCompactPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 0.7,
            min_turns_between_compacts: 5,
        }
    }
}

#[async_trait]
impl CompactPolicy for DefaultCompactPolicy {
    async fn classify(&self, ctx: &CompactContext<'_>) -> Option<String> {
        if !self.enabled || ctx.context_window == 0 {
            return None;
        }
        let last = ctx.last_compact_turn.unwrap_or(0);
        if ctx.turn_index.saturating_sub(last) < self.min_turns_between_compacts {
            return None;
        }
        let pressure = (ctx.usage.tokens as f64) / (ctx.context_window as f64);
        if pressure < self.threshold {
            return None;
        }
        let focus: String = ctx
            .goal_description
            .chars()
            .take(FOCUS_TRUNCATE_CHARS)
            .collect();
        Some(format!("continue goal: {focus}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        usage: &'a BudgetUsage,
        ctx_window: u64,
        turn: u32,
        last: Option<u32>,
        desc: &'a str,
    ) -> CompactContext<'a> {
        CompactContext {
            goal_id: GoalId::new(),
            turn_index: turn,
            usage,
            context_window: ctx_window,
            last_compact_turn: last,
            goal_description: desc,
        }
    }

    #[tokio::test]
    async fn pressure_above_threshold_returns_focus() {
        let p = DefaultCompactPolicy::default();
        let usage = BudgetUsage {
            tokens: 140_000,
            ..Default::default()
        };
        let r = p
            .classify(&ctx(&usage, 200_000, 6, None, "Implementa Phase 26.z"))
            .await;
        match r {
            Some(focus) => {
                assert!(focus.starts_with("continue goal: "));
                assert!(focus.contains("Phase 26.z"));
            }
            None => panic!("expected Some(focus)"),
        }
    }

    #[tokio::test]
    async fn pressure_below_threshold_returns_none() {
        let p = DefaultCompactPolicy::default();
        let usage = BudgetUsage {
            tokens: 50_000,
            ..Default::default()
        };
        assert!(p
            .classify(&ctx(&usage, 200_000, 6, None, "x"))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn disabled_returns_none() {
        let p = DefaultCompactPolicy {
            enabled: false,
            ..Default::default()
        };
        let usage = BudgetUsage {
            tokens: 999_999,
            ..Default::default()
        };
        assert!(p
            .classify(&ctx(&usage, 200_000, 9, None, "x"))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn context_window_zero_returns_none() {
        let p = DefaultCompactPolicy::default();
        let usage = BudgetUsage {
            tokens: 999_999,
            ..Default::default()
        };
        assert!(p.classify(&ctx(&usage, 0, 9, None, "x")).await.is_none());
    }

    #[tokio::test]
    async fn min_gap_respected() {
        let p = DefaultCompactPolicy {
            min_turns_between_compacts: 5,
            ..Default::default()
        };
        let usage = BudgetUsage {
            tokens: 180_000,
            ..Default::default()
        };
        // Last compact at turn 6, current 8 → gap 2 < 5, no compact.
        assert!(p
            .classify(&ctx(&usage, 200_000, 8, Some(6), "x"))
            .await
            .is_none());
        // Same setup but at turn 11 → gap 5 ≥ 5, compact fires.
        assert!(p
            .classify(&ctx(&usage, 200_000, 11, Some(6), "x"))
            .await
            .is_some());
    }
}
