//! Phase 77.2 — autoCompact policy shared by driver-loop and core agent.
//!
//! `CompactPolicy::classify` returns `Some((focus_hint, trigger))` when
//! the caller should inject a compact turn before the next regular turn.
//! Two independent triggers:
//! 1. Token pressure — `estimated_tokens >= context_window * token_pct`.
//! 2. Age — `session_age_minutes >= max_age_minutes`.
//! Both respect `min_turns_between` (anti-storm) and
//! `max_consecutive_failures` (circuit breaker).

use async_trait::async_trait;
use nexo_config::types::llm::AutoCompactionConfig;

use crate::{BudgetUsage, GoalId};

const FOCUS_TRUNCATE_CHARS: usize = 140;

/// Why autoCompact fired. Carried in `DriverEvent::CompactRequested`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompactTrigger {
    TokenPressure {
        pct: f64,
        tokens_used: u64,
        context_window: u64,
    },
    Age {
        age_minutes: u64,
        max_age_minutes: u64,
    },
}

impl CompactTrigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            CompactTrigger::TokenPressure { .. } => "token_pressure",
            CompactTrigger::Age { .. } => "age",
        }
    }
}

/// Snapshot the policy needs for a single classification decision.
#[derive(Clone, Debug)]
pub struct CompactContext<'a> {
    pub goal_id: GoalId,
    pub turn_index: u32,
    pub usage: &'a BudgetUsage,
    pub context_window: u64,
    pub last_compact_turn: Option<u32>,
    pub goal_description: &'a str,
    /// Minutes since the session was created (`Utc::now() - created_at`).
    /// Best-effort: the driver-loop uses wall-clock from spawn; the core
    /// agent uses the persisted `Session.created_at`.
    pub session_age_minutes: u64,
    /// `None` means age trigger is disabled and token trigger falls back
    /// to the legacy `CompactPolicyConfig::threshold`.
    pub auto_config: Option<&'a AutoCompactionConfig>,
}

/// Policy that decides whether to inject a `/compact` turn.
#[async_trait]
pub trait CompactPolicy: Send + Sync + 'static {
    /// `Some((focus_hint, trigger))` when a compact turn should be injected
    /// before the next regular turn. `None` to keep going.
    async fn classify(&self, ctx: &CompactContext<'_>) -> Option<(String, CompactTrigger)>;
}

/// Default rules-based implementation: token pressure + age triggers.
pub struct DefaultCompactPolicy {
    /// Master switch. When `false`, `classify` always returns `None`.
    pub enabled: bool,
    /// Legacy threshold (0.0–1.0). Used only when `auto_config` is `None`
    /// or its `token_pct` is 0.0.
    pub threshold: f64,
    /// Minimum turns between consecutive compact injections.
    pub min_turns_between: u32,
    /// Consecutive failures that trip the circuit breaker.
    pub max_consecutive_failures: u32,
}

impl Default for DefaultCompactPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 0.7,
            min_turns_between: 5,
            max_consecutive_failures: 3,
        }
    }
}

#[async_trait]
impl CompactPolicy for DefaultCompactPolicy {
    async fn classify(&self, ctx: &CompactContext<'_>) -> Option<(String, CompactTrigger)> {
        if !self.enabled || ctx.context_window == 0 {
            return None;
        }

        // ── anti-storm ──────────────────────────────────────────────
        let last = ctx.last_compact_turn.unwrap_or(0);
        let min_gap = ctx
            .auto_config
            .map(|a| a.min_turns_between)
            .unwrap_or(self.min_turns_between);
        if ctx.turn_index.saturating_sub(last) < min_gap {
            return None;
        }

        // Resolve effective token-pct threshold.
        let token_pct: f64 = ctx
            .auto_config
            .and_then(|a| {
                if a.token_pct > 0.0 {
                    Some(a.token_pct as f64)
                } else {
                    None
                }
            })
            .unwrap_or(self.threshold);

        // ── token-pressure trigger ──────────────────────────────────
        let pressure = (ctx.usage.tokens as f64) / (ctx.context_window as f64);
        if pressure >= token_pct {
            let focus: String = ctx
                .goal_description
                .chars()
                .take(FOCUS_TRUNCATE_CHARS)
                .collect();
            return Some((
                format!("continue goal: {focus}"),
                CompactTrigger::TokenPressure {
                    pct: pressure,
                    tokens_used: ctx.usage.tokens,
                    context_window: ctx.context_window,
                },
            ));
        }

        // ── age trigger ─────────────────────────────────────────────
        if let Some(auto) = ctx.auto_config {
            if auto.max_age_minutes > 0
                && ctx.session_age_minutes >= auto.max_age_minutes
            {
                let focus: String = ctx
                    .goal_description
                    .chars()
                    .take(FOCUS_TRUNCATE_CHARS)
                    .collect();
                return Some((
                    format!("continue goal: {focus}"),
                    CompactTrigger::Age {
                        age_minutes: ctx.session_age_minutes,
                        max_age_minutes: auto.max_age_minutes,
                    },
                ));
            }
        }

        None
    }
}

/// Circuit-breaker helper. Callers increment on failure, reset on success.
#[derive(Debug, Clone, Default)]
pub struct AutoCompactBreaker {
    pub consecutive_failures: u32,
    pub last_compact_turn: Option<u32>,
}

impl AutoCompactBreaker {
    pub fn is_tripped(&self, max: u32) -> bool {
        max > 0 && self.consecutive_failures >= max
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }

    pub fn record_success(&mut self, turn: u32) {
        self.consecutive_failures = 0;
        self.last_compact_turn = Some(turn);
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
        age_minutes: u64,
        auto: Option<&'a AutoCompactionConfig>,
    ) -> CompactContext<'a> {
        CompactContext {
            goal_id: GoalId::new(),
            turn_index: turn,
            usage,
            context_window: ctx_window,
            last_compact_turn: last,
            goal_description: desc,
            session_age_minutes: age_minutes,
            auto_config: auto,
        }
    }

    fn auto_cfg(token_pct: f32, max_age_minutes: u64) -> AutoCompactionConfig {
        AutoCompactionConfig {
            token_pct,
            max_age_minutes,
            buffer_tokens: 13_000,
            min_turns_between: 5,
            max_consecutive_failures: 3,
        }
    }

    // ── token-pressure trigger ─────────────────────────────────────

    #[tokio::test]
    async fn token_pressure_with_legacy_threshold() {
        let p = DefaultCompactPolicy::default();
        let usage = BudgetUsage {
            tokens: 140_000,
            ..Default::default()
        };
        let r = p
            .classify(&ctx(&usage, 200_000, 6, None, "do stuff", 10, None))
            .await;
        let (focus, trigger) = r.expect("should fire");
        assert!(focus.starts_with("continue goal: "));
        assert!(matches!(trigger, CompactTrigger::TokenPressure { .. }));
    }

    #[tokio::test]
    async fn token_pressure_below_threshold_returns_none() {
        let p = DefaultCompactPolicy::default();
        let usage = BudgetUsage {
            tokens: 50_000,
            ..Default::default()
        };
        assert!(p
            .classify(&ctx(&usage, 200_000, 6, None, "x", 10, None))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn token_pressure_with_auto_config() {
        let p = DefaultCompactPolicy::default();
        let auto = auto_cfg(0.80, 120);
        let usage = BudgetUsage {
            tokens: 170_000,
            ..Default::default()
        };
        let r = p
            .classify(&ctx(&usage, 200_000, 6, None, "goal X", 10, Some(&auto)))
            .await;
        let (_focus, trigger) = r.expect("auto token_pct should fire at 85%");
        assert!(matches!(trigger, CompactTrigger::TokenPressure { .. }));
    }

    #[tokio::test]
    async fn token_pct_zero_disables_token_trigger_in_auto() {
        let p = DefaultCompactPolicy::default();
        let auto = auto_cfg(0.0, 120);
        let usage = BudgetUsage {
            tokens: 190_000,
            ..Default::default()
        };
        // token_pct=0 means no token trigger from auto; falls back to
        // legacy threshold=0.7. 190K/200K = 0.95 ≥ 0.7, so it fires.
        let r = p
            .classify(&ctx(
                &usage, 200_000, 6, None, "goal X", 10, Some(&auto),
            ))
            .await;
        assert!(r.is_some(), "legacy threshold 0.7 should fire at 95%");
    }

    // ── age trigger ────────────────────────────────────────────────

    #[tokio::test]
    async fn age_trigger_fires_when_expired() {
        let p = DefaultCompactPolicy::default();
        let auto = auto_cfg(0.80, 120);
        let usage = BudgetUsage::default(); // low tokens
        let r = p
            .classify(&ctx(&usage, 200_000, 6, None, "old goal", 185, Some(&auto)))
            .await;
        let (_focus, trigger) = r.expect("age trigger should fire at 185 min");
        assert!(matches!(
            trigger,
            CompactTrigger::Age {
                age_minutes: 185,
                max_age_minutes: 120
            }
        ));
    }

    #[tokio::test]
    async fn age_trigger_skips_when_not_expired() {
        let p = DefaultCompactPolicy::default();
        let auto = auto_cfg(0.80, 120);
        let usage = BudgetUsage::default();
        assert!(p
            .classify(&ctx(&usage, 200_000, 6, None, "young goal", 30, Some(&auto)))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn age_trigger_disabled_when_max_age_zero() {
        let p = DefaultCompactPolicy::default();
        let auto = auto_cfg(0.80, 0); // age disabled
        let usage = BudgetUsage::default();
        assert!(p
            .classify(&ctx(&usage, 200_000, 6, None, "goal", 999, Some(&auto)))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn age_trigger_disabled_when_auto_none() {
        let p = DefaultCompactPolicy::default();
        let usage = BudgetUsage::default();
        assert!(p
            .classify(&ctx(&usage, 200_000, 6, None, "goal", 999, None))
            .await
            .is_none());
    }

    // ── both triggers: token wins (checked first) ──────────────────

    #[tokio::test]
    async fn both_triggers_token_wins() {
        let p = DefaultCompactPolicy::default();
        let auto = auto_cfg(0.80, 120);
        let usage = BudgetUsage {
            tokens: 180_000,
            ..Default::default()
        };
        let r = p
            .classify(&ctx(&usage, 200_000, 6, None, "hot old goal", 200, Some(&auto)))
            .await;
        let (_focus, trigger) = r.expect("should fire");
        assert!(
            matches!(trigger, CompactTrigger::TokenPressure { .. }),
            "token pressure should win over age"
        );
    }

    // ── guards ─────────────────────────────────────────────────────

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
            .classify(&ctx(&usage, 200_000, 9, None, "x", 999, None))
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
        assert!(p.classify(&ctx(&usage, 0, 9, None, "x", 999, None)).await.is_none());
    }

    #[tokio::test]
    async fn min_turns_between_respected() {
        let p = DefaultCompactPolicy {
            min_turns_between: 5,
            ..Default::default()
        };
        let usage = BudgetUsage {
            tokens: 180_000,
            ..Default::default()
        };
        // Last compact at turn 6, current 8 → gap 2 < 5, no compact.
        assert!(p
            .classify(&ctx(&usage, 200_000, 8, Some(6), "x", 10, None))
            .await
            .is_none());
        // Same setup but at turn 11 → gap 5 ≥ 5, compact fires.
        assert!(p
            .classify(&ctx(&usage, 200_000, 11, Some(6), "x", 10, None))
            .await
            .is_some());
    }

    #[tokio::test]
    async fn min_turns_between_from_auto_config() {
        let p = DefaultCompactPolicy::default();
        let auto = AutoCompactionConfig {
            min_turns_between: 3,
            ..auto_cfg(0.80, 120)
        };
        let usage = BudgetUsage {
            tokens: 180_000,
            ..Default::default()
        };
        // Gap 2 < 3 from auto → no compact.
        assert!(p
            .classify(&ctx(&usage, 200_000, 8, Some(6), "x", 10, Some(&auto)))
            .await
            .is_none());
        // Gap 3 = 3 → fires.
        assert!(p
            .classify(&ctx(&usage, 200_000, 9, Some(6), "x", 10, Some(&auto)))
            .await
            .is_some());
    }

    // ── circuit breaker ────────────────────────────────────────────

    #[test]
    fn breaker_stops_after_n_failures() {
        let mut b = AutoCompactBreaker::default();
        assert!(!b.is_tripped(3));
        b.record_failure();
        b.record_failure();
        assert!(!b.is_tripped(3));
        b.record_failure();
        assert!(b.is_tripped(3));
    }

    #[test]
    fn breaker_resets_on_success() {
        let mut b = AutoCompactBreaker {
            consecutive_failures: 3,
            last_compact_turn: None,
        };
        assert!(b.is_tripped(3));
        b.record_success(5);
        assert!(!b.is_tripped(3));
        assert_eq!(b.last_compact_turn, Some(5));
    }

    #[test]
    fn breaker_max_zero_never_trips() {
        let b = AutoCompactBreaker {
            consecutive_failures: 99,
            last_compact_turn: None,
        };
        assert!(!b.is_tripped(0));
    }
}

// ── Phase 77.3 — session memory compact ───────────────────────────

/// Config for session-memory-backed compaction.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SmCompactConfig {
    #[serde(default = "default_sm_min_tokens")]
    pub min_tokens: u64,
    #[serde(default = "default_sm_max_tokens")]
    pub max_tokens: u64,
    #[serde(default = "default_sm_store_in_ltm")]
    pub store_in_long_term_memory: bool,
}

impl Default for SmCompactConfig {
    fn default() -> Self {
        Self {
            min_tokens: default_sm_min_tokens(),
            max_tokens: default_sm_max_tokens(),
            store_in_long_term_memory: default_sm_store_in_ltm(),
        }
    }
}

fn default_sm_min_tokens() -> u64 {
    10_000
}
fn default_sm_max_tokens() -> u64 {
    40_000
}
fn default_sm_store_in_ltm() -> bool {
    true
}

/// A persisted compact summary, stored in long-term memory for session resume.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CompactSummary {
    pub agent_id: String,
    pub summary: String,
    pub turn_index: u32,
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub stored_at: chrono::DateTime<chrono::Utc>,
}

/// Persistence for compact summaries. Separate trait so tests can use
/// a noop implementation and the real backend can swap independently.
#[async_trait]
pub trait CompactSummaryStore: Send + Sync + 'static {
    /// Persist a compact summary. Idempotent per `(goal_id, turn_index)`.
    async fn store(&self, summary: CompactSummary) -> Result<(), String>;

    /// Load the most recent compact summary for a goal, if any.
    async fn load(
        &self,
        agent_id: &str,
        goal_id: &crate::GoalId,
    ) -> Result<Option<CompactSummary>, String>;

    /// Remove all summaries for a goal (cleanup on goal done).
    async fn forget(&self, goal_id: &crate::GoalId) -> Result<(), String>;
}

// ── Phase 77.5 — extractMemories config ────────────────────────────

/// Config for post-turn LLM memory extraction.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExtractMemoriesConfig {
    /// Master switch. Default false — opt-in.
    #[serde(default)]
    pub enabled: bool,
    /// Run extraction every N eligible turns (1 = every turn).
    #[serde(default = "default_extract_throttle")]
    pub turns_throttle: u32,
    /// Hard cap on LLM turns per extraction.
    #[serde(default = "default_extract_max_turns")]
    pub max_turns: u32,
    /// Consecutive failures that trip the circuit breaker (0 = disabled).
    #[serde(default = "default_extract_max_failures")]
    pub max_consecutive_failures: u32,
}

impl Default for ExtractMemoriesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            turns_throttle: default_extract_throttle(),
            max_turns: default_extract_max_turns(),
            max_consecutive_failures: default_extract_max_failures(),
        }
    }
}

fn default_extract_throttle() -> u32 { 1 }
fn default_extract_max_turns() -> u32 { 5 }
fn default_extract_max_failures() -> u32 { 3 }
