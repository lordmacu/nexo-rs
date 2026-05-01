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

/// Phase 85.2 — micro-compact policy. Per-turn cheap O(1) check
/// the orchestrator runs BEFORE assembling the request body. When
/// it returns `Some(decision)`, the orchestrator replaces the
/// matched tool result with [`TIME_BASED_MC_CLEARED_MESSAGE`] and
/// records the truncation in the next [`CompactSummary`].
///
/// Distinct from [`CompactPolicy`] (which decides whether to inject
/// a full /compact turn). Micro-compact runs per-turn and trims
/// individual oversized tool results in-place without invalidating
/// the cache.
#[async_trait]
pub trait MicroCompactPolicy: Send + Sync + 'static {
    /// `Some(decision)` when the tool result identified by
    /// `(call_id, body_byte_size, turn_index)` should be replaced
    /// by the marker. `None` to leave the tool result intact.
    async fn classify(
        &self,
        ctx: &MicroCompactContext<'_>,
    ) -> Option<MicroCompactDecision>;
}

/// Inputs the micro-compact policy needs per call.
#[derive(Clone, Debug)]
pub struct MicroCompactContext<'a> {
    pub call_id: &'a str,
    /// Byte size of the candidate tool result body.
    pub body_byte_size: u64,
    pub turn_index: u32,
    /// Highest turn that already has a cache breakpoint pinned.
    /// Truncating a tool result *after* this point is safe (the
    /// cache hasn't matched there yet); truncating *before* would
    /// invalidate downstream prompt-cache breakpoints, defeating
    /// the whole purpose of the policy.
    pub cache_breakpoint: u32,
}

/// What the policy decided to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroCompactDecision {
    pub call_id: String,
    pub original_byte_size: u64,
    pub marker_inserted_at_turn: u32,
}

/// Default rules-based micro-compact policy: trim when the body
/// is over `min_body_bytes` AND the call lives strictly before
/// the active cache breakpoint.
pub struct DefaultMicroCompactPolicy {
    /// Master switch. `false` always returns `None`.
    pub enabled: bool,
    /// Minimum tool-result body size that warrants a truncate.
    /// Default 8 KiB — anything smaller is cheap to keep verbatim
    /// and the marker overhead would actually grow some payloads.
    pub min_body_bytes: u64,
}

impl Default for DefaultMicroCompactPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            min_body_bytes: 8 * 1024,
        }
    }
}

#[async_trait]
impl MicroCompactPolicy for DefaultMicroCompactPolicy {
    async fn classify(
        &self,
        ctx: &MicroCompactContext<'_>,
    ) -> Option<MicroCompactDecision> {
        if !self.enabled {
            return None;
        }
        if ctx.body_byte_size < self.min_body_bytes {
            return None;
        }
        // Truncating *at or after* the active cache breakpoint is
        // safe — those bytes haven't anchored a cache hit yet.
        // Truncating *before* would dirty an already-cached
        // prefix, defeating the purpose.
        if ctx.turn_index < ctx.cache_breakpoint {
            return None;
        }
        Some(MicroCompactDecision {
            call_id: ctx.call_id.to_string(),
            original_byte_size: ctx.body_byte_size,
            marker_inserted_at_turn: ctx.turn_index,
        })
    }
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

/// One tool result that the micro-compact policy truncated. Carried
/// inside [`CompactSummary`] so the orchestrator + provider clients
/// reconstruct the same shape across daemon restarts.
///
/// Phase 85.2.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TruncatedToolResult {
    /// The tool call's stable id (matches the `id` field in the
    /// provider's `tool_use` / `tool_result` messages).
    pub call_id: String,
    /// Byte size of the original tool result payload before
    /// truncation. Lets the operator-facing telemetry surface
    /// "saved 12 KiB on this turn" without re-reading the
    /// transcript.
    pub original_byte_size: u64,
    /// Turn index at which the marker was inserted. Used by the
    /// orchestrator's idempotency check (see done-criterion 6:
    /// "two consecutive compacts do not double-mark the same
    /// call_id").
    pub marker_inserted_at_turn: u32,
}

/// Marker text the orchestrator splices into a truncated tool
/// result. Constant string so the provider's prompt-cache prefix
/// matcher sees identical bytes across compact passes.
///
/// Phase 85.2.
pub const TIME_BASED_MC_CLEARED_MESSAGE: &str =
    "[tool_result truncated by micro-compact for cache stability — \
see CompactSummary.truncated_tool_results for original byte size]";

/// A persisted compact summary, stored in long-term memory for session resume.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CompactSummary {
    pub agent_id: String,
    pub summary: String,
    pub turn_index: u32,
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub stored_at: chrono::DateTime<chrono::Utc>,
    /// Phase 85.2 — provider-cache breakpoint anchors that survived
    /// the compact pass. Each entry is a stable string the provider
    /// client uses to render `cache_control: { type: "ephemeral" }`
    /// markers at the same logical positions across turns. Empty in
    /// pre-85.2 payloads thanks to `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_pin_keys: Vec<String>,
    /// Phase 85.2 — tool results the micro-compact policy truncated
    /// during this compact pass. The orchestrator deduplicates by
    /// `call_id` across consecutive compacts so the same call is
    /// never marked twice. Empty in pre-85.2 payloads thanks to
    /// `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncated_tool_results: Vec<TruncatedToolResult>,
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

#[cfg(test)]
mod micro_compact_tests {
    use super::*;

    fn mctx(call: &str, bytes: u64, turn: u32, breakpoint: u32) -> MicroCompactContext<'_> {
        MicroCompactContext {
            call_id: call,
            body_byte_size: bytes,
            turn_index: turn,
            cache_breakpoint: breakpoint,
        }
    }

    #[tokio::test]
    async fn small_body_returns_none() {
        let p = DefaultMicroCompactPolicy::default();
        // 4 KiB < default min_body_bytes (8 KiB)
        let d = p.classify(&mctx("c1", 4_000, 10, 5)).await;
        assert!(d.is_none());
    }

    #[tokio::test]
    async fn oversized_body_after_breakpoint_returns_decision() {
        let p = DefaultMicroCompactPolicy::default();
        let d = p
            .classify(&mctx("c1", 16_000, 10, 5))
            .await
            .expect("decision");
        assert_eq!(d.call_id, "c1");
        assert_eq!(d.original_byte_size, 16_000);
        assert_eq!(d.marker_inserted_at_turn, 10);
    }

    #[tokio::test]
    async fn oversized_body_before_breakpoint_returns_none() {
        // Truncating BEFORE the active cache breakpoint would
        // dirty an already-cached prefix — must skip.
        let p = DefaultMicroCompactPolicy::default();
        let d = p.classify(&mctx("c1", 32_000, 3, 10)).await;
        assert!(d.is_none(), "must not truncate inside cached prefix");
    }

    #[tokio::test]
    async fn body_exactly_at_breakpoint_truncates() {
        // Equal turn = at the breakpoint, NOT before it. Safe to
        // truncate (not yet anchored).
        let p = DefaultMicroCompactPolicy::default();
        let d = p.classify(&mctx("c1", 16_000, 5, 5)).await;
        assert!(d.is_some(), "turn == breakpoint must be eligible");
    }

    #[tokio::test]
    async fn disabled_policy_returns_none() {
        let p = DefaultMicroCompactPolicy {
            enabled: false,
            min_body_bytes: 0,
        };
        let d = p.classify(&mctx("c1", 999_999, 100, 0)).await;
        assert!(d.is_none());
    }

    #[tokio::test]
    async fn custom_min_body_threshold_respected() {
        let p = DefaultMicroCompactPolicy {
            enabled: true,
            min_body_bytes: 100_000,
        };
        // 16 KiB is no longer over a 100 KiB threshold.
        assert!(p.classify(&mctx("c1", 16_000, 10, 5)).await.is_none());
        // 200 KiB still triggers.
        assert!(p.classify(&mctx("c1", 200_000, 10, 5)).await.is_some());
    }

    #[test]
    fn truncated_tool_result_serde_round_trip() {
        let t = TruncatedToolResult {
            call_id: "call-9".into(),
            original_byte_size: 32_768,
            marker_inserted_at_turn: 7,
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: TruncatedToolResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn compact_summary_pre85_2_payload_loads_with_empty_extensions() {
        // Backwards-compat: pre-85.2 payloads (no cache_pin_keys
        // / truncated_tool_results) must deserialise cleanly with
        // empty Vecs via #[serde(default)]. Migration test
        // (done-criterion 5).
        let pre_85_2_json = r#"{
            "agent_id": "ana",
            "summary": "compacted history",
            "turn_index": 12,
            "before_tokens": 80000,
            "after_tokens": 20000,
            "stored_at": "2026-04-01T12:00:00Z"
        }"#;
        let s: CompactSummary = serde_json::from_str(pre_85_2_json)
            .expect("pre-85.2 payload must load");
        assert_eq!(s.agent_id, "ana");
        assert!(s.cache_pin_keys.is_empty());
        assert!(s.truncated_tool_results.is_empty());
    }

    #[test]
    fn compact_summary_85_2_payload_round_trips() {
        let s = CompactSummary {
            agent_id: "ana".into(),
            summary: "compacted".into(),
            turn_index: 12,
            before_tokens: 80_000,
            after_tokens: 20_000,
            stored_at: chrono::DateTime::parse_from_rfc3339(
                "2026-04-01T12:00:00Z",
            )
            .unwrap()
            .with_timezone(&chrono::Utc),
            cache_pin_keys: vec!["sys".into(), "tools".into()],
            truncated_tool_results: vec![TruncatedToolResult {
                call_id: "c-7".into(),
                original_byte_size: 16_000,
                marker_inserted_at_turn: 5,
            }],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: CompactSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cache_pin_keys, s.cache_pin_keys);
        assert_eq!(back.truncated_tool_results, s.truncated_tool_results);
    }

    #[test]
    fn time_based_marker_constant_is_stable() {
        // The marker text MUST be a constant — provider's
        // prompt-cache prefix matcher keys on byte-identical
        // bytes across turns. Re-formatting / interpolating the
        // marker would break the cache.
        assert!(TIME_BASED_MC_CLEARED_MESSAGE.contains("truncated"));
        assert!(TIME_BASED_MC_CLEARED_MESSAGE.contains("micro-compact"));
        assert_eq!(
            TIME_BASED_MC_CLEARED_MESSAGE.len(),
            TIME_BASED_MC_CLEARED_MESSAGE.len(),
            "constant must not change"
        );
    }
}
