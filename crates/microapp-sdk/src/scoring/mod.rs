//! Lead scoring primitives (M15.23.f).
//!
//! A small, dep-free framework for **explainable** lead /
//! contact scoring that any microapp can compose.
//!
//! Three layers:
//!
//! - [`Score`] — `0..=100` saturating newtype carrying a list
//!   of [`ScoreReason`] traces. Every contribution is logged
//!   ("+15 because corporate domain", "+20 because intent =
//!   ready_to_buy") so the operator UI can render an
//!   explanation popover instead of an opaque number.
//! - [`Scorer`] trait — `score(input) -> Score`. Pure;
//!   stateless from the trait's perspective. Implementations
//!   own their own reference data (configured weights, table
//!   lookups, …).
//! - [`HeuristicScorer`] — default weighted-rule impl that
//!   accepts a list of [`HeuristicRule`]s, applies each, sums
//!   contributions, and threads the trace through.
//!
//! ## Why not a full ML pipeline
//!
//! Heuristic v1 — nine boolean / banded features cover ~80%
//! of operator intuition (corporate vs personal sender, reply
//! latency, content length, explicit intent labels). Replace
//! later with an `MlScorer` impl behind the same trait when
//! a labelled dataset exists.
//!
//! ## Composability
//!
//! Two scorers compose by summing their `Score`s:
//! `combined = a.score(input) + b.score(input)` (the
//! `Add` impl saturates at 100 + concatenates traces). One
//! source of truth, one explanation list.

use serde::{Deserialize, Serialize};

/// One reason a scorer contributed to (or against) the
/// outcome. Operator UI lists these in the lead drawer's
/// "Por qué este score" tooltip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreReason {
    /// Stable machine label — useful for analytics joins
    /// (`"corporate_domain"`, `"replied_within_1h"`).
    pub label: String,
    /// Operator-facing prose. Optional — caller may
    /// translate the label at render time instead.
    pub detail: Option<String>,
    /// Signed contribution, applied to the running total.
    /// Saturating at the 0..=100 boundary at sum time.
    pub delta: i32,
}

impl ScoreReason {
    /// Build with a label + delta only — caller renders the
    /// label translation themselves.
    pub fn new(label: impl Into<String>, delta: i32) -> Self {
        Self {
            label: label.into(),
            detail: None,
            delta,
        }
    }

    /// Same, with an inline operator-facing detail string.
    pub fn with_detail(label: impl Into<String>, delta: i32, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: Some(detail.into()),
            delta,
        }
    }
}

/// `0..=100` saturating score with an attached reason trace.
/// Construction always goes through [`Score::from_reasons`]
/// or [`Score::saturating_new`] so the invariant holds at the
/// type boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Score {
    value: u8,
    reasons: Vec<ScoreReason>,
}

impl Score {
    /// Build a zero-value score with no reasons attached.
    /// Useful as an `acc` for fold operations.
    pub fn zero() -> Self {
        Self {
            value: 0,
            reasons: Vec::new(),
        }
    }

    /// Build by saturating any signed sum into the 0..=100
    /// band. Negative ⇒ 0; > 100 ⇒ 100.
    pub fn saturating_new(value: i32, reasons: Vec<ScoreReason>) -> Self {
        let clamped = value.clamp(0, 100) as u8;
        Self {
            value: clamped,
            reasons,
        }
    }

    /// Sum `reasons` into a final value; saturate.
    pub fn from_reasons(reasons: Vec<ScoreReason>) -> Self {
        let raw: i32 = reasons.iter().map(|r| r.delta).sum();
        Self::saturating_new(raw, reasons)
    }

    /// Final 0..=100 value.
    pub fn value(&self) -> u8 {
        self.value
    }

    /// Borrow the reason trace.
    pub fn reasons(&self) -> &[ScoreReason] {
        &self.reasons
    }

    /// Consume + return the reason trace (avoid clone for the
    /// audit-log persist path).
    pub fn into_reasons(self) -> Vec<ScoreReason> {
        self.reasons
    }
}

impl std::ops::Add for Score {
    type Output = Score;

    fn add(self, other: Score) -> Score {
        let mut reasons = self.reasons;
        reasons.extend(other.reasons);
        let raw = self.value as i32 + other.value as i32;
        Score::saturating_new(raw, reasons)
    }
}

/// Generic scorer trait. `Input` is the caller's lead /
/// contact / message context — keeps the trait dep-free.
pub trait Scorer<Input: ?Sized> {
    /// Compute a [`Score`] for one input. Pure; same input ⇒
    /// same output. Implementations MUST always return a
    /// well-formed `Score` (no panics, no invariant
    /// violations).
    fn score(&self, input: &Input) -> Score;
}

/// One rule the heuristic scorer evaluates. Caller supplies
/// the predicate (`should_apply`) + the contribution
/// (`contribution`).
///
/// Generic over the input type so consumers compose typed
/// rules without `Box<dyn Any>` gymnastics. The `Send + Sync`
/// bounds let the resulting [`HeuristicScorer`] be wrapped in
/// `Arc<dyn Scorer>`.
pub struct HeuristicRule<Input: ?Sized> {
    predicate: Box<dyn Fn(&Input) -> bool + Send + Sync>,
    contribution: ScoreReason,
}

impl<Input: ?Sized> HeuristicRule<Input> {
    /// Build a rule that contributes `delta` (signed) +
    /// records the `label` when the predicate fires.
    pub fn new(
        label: impl Into<String>,
        delta: i32,
        predicate: impl Fn(&Input) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            predicate: Box::new(predicate),
            contribution: ScoreReason::new(label, delta),
        }
    }

    /// Same with a prose detail attached.
    pub fn with_detail(
        label: impl Into<String>,
        delta: i32,
        detail: impl Into<String>,
        predicate: impl Fn(&Input) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            predicate: Box::new(predicate),
            contribution: ScoreReason::with_detail(label, delta, detail),
        }
    }
}

impl<Input: ?Sized> std::fmt::Debug for HeuristicRule<Input> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeuristicRule")
            .field("contribution", &self.contribution)
            .finish_non_exhaustive()
    }
}

/// Default scorer impl: walks a vector of [`HeuristicRule`]s,
/// applies each in order, sums contributions, returns the
/// saturated score with the reason trace.
pub struct HeuristicScorer<Input: ?Sized + 'static> {
    rules: Vec<HeuristicRule<Input>>,
}

impl<Input: ?Sized + 'static> Default for HeuristicScorer<Input> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Input: ?Sized + 'static> HeuristicScorer<Input> {
    /// Empty scorer — `score()` always returns `Score::zero()`.
    /// Caller adds rules with [`HeuristicScorer::push`].
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Build from a fixed rule list.
    pub fn with_rules(rules: Vec<HeuristicRule<Input>>) -> Self {
        Self { rules }
    }

    /// Append one rule.
    pub fn push(&mut self, rule: HeuristicRule<Input>) -> &mut Self {
        self.rules.push(rule);
        self
    }

    /// Number of rules — handy for tests + boot logging.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

impl<Input: ?Sized + 'static> Scorer<Input> for HeuristicScorer<Input> {
    fn score(&self, input: &Input) -> Score {
        let mut hits = Vec::with_capacity(self.rules.len());
        for rule in &self.rules {
            if (rule.predicate)(input) {
                hits.push(rule.contribution.clone());
            }
        }
        Score::from_reasons(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal lead-shaped input the heuristic scorer cares
    // about — kept inline so the SDK doesn't depend on the
    // marketing wire shape.
    struct LeadCtx {
        is_corporate: bool,
        replied_minutes_ago: Option<u32>,
        body_words: u32,
        intent_label: Option<&'static str>,
        signature_role_hint: Option<&'static str>,
    }

    fn corporate_lead() -> LeadCtx {
        LeadCtx {
            is_corporate: true,
            replied_minutes_ago: Some(45),
            body_words: 60,
            intent_label: Some("ready_to_buy"),
            signature_role_hint: Some("CTO"),
        }
    }

    fn cold_lead() -> LeadCtx {
        LeadCtx {
            is_corporate: false,
            replied_minutes_ago: None,
            body_words: 5,
            intent_label: None,
            signature_role_hint: None,
        }
    }

    fn marketing_scorer() -> HeuristicScorer<LeadCtx> {
        let mut s = HeuristicScorer::new();
        s.push(HeuristicRule::new("corporate_domain", 15, |l: &LeadCtx| {
            l.is_corporate
        }));
        s.push(HeuristicRule::new(
            "replied_within_1h",
            10,
            |l: &LeadCtx| l.replied_minutes_ago.map(|m| m <= 60).unwrap_or(false),
        ));
        s.push(HeuristicRule::new(
            "substantive_reply_30plus_words",
            10,
            |l: &LeadCtx| l.body_words >= 30,
        ));
        s.push(HeuristicRule::new(
            "intent_ready_to_buy",
            20,
            |l: &LeadCtx| matches!(l.intent_label, Some("ready_to_buy")),
        ));
        s.push(HeuristicRule::new("senior_signature", 10, |l: &LeadCtx| {
            matches!(
                l.signature_role_hint,
                Some("CTO" | "CEO" | "VP" | "Director" | "Head"),
            )
        }));
        s
    }

    // ─── Score newtype ────────────────────────────────────────

    #[test]
    fn score_zero_starts_blank() {
        let s = Score::zero();
        assert_eq!(s.value(), 0);
        assert!(s.reasons().is_empty());
    }

    #[test]
    fn score_saturates_below_zero_to_zero() {
        let s = Score::saturating_new(-50, vec![]);
        assert_eq!(s.value(), 0);
    }

    #[test]
    fn score_saturates_above_100_to_100() {
        let s = Score::saturating_new(250, vec![]);
        assert_eq!(s.value(), 100);
    }

    #[test]
    fn score_from_reasons_sums_deltas() {
        let s = Score::from_reasons(vec![
            ScoreReason::new("a", 30),
            ScoreReason::new("b", 25),
            ScoreReason::new("c", -5),
        ]);
        assert_eq!(s.value(), 50);
        assert_eq!(s.reasons().len(), 3);
    }

    #[test]
    fn score_add_concatenates_reasons_and_saturates() {
        let a = Score::from_reasons(vec![ScoreReason::new("x", 60)]);
        let b = Score::from_reasons(vec![ScoreReason::new("y", 70)]);
        let combined = a + b;
        assert_eq!(combined.value(), 100);
        assert_eq!(combined.reasons().len(), 2);
    }

    #[test]
    fn score_serde_roundtrips() {
        let s = Score::from_reasons(vec![ScoreReason::with_detail("x", 10, "rationale")]);
        let json = serde_json::to_string(&s).unwrap();
        let back: Score = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    // ─── HeuristicScorer ──────────────────────────────────────

    #[test]
    fn corporate_lead_lands_high_score() {
        let s = marketing_scorer().score(&corporate_lead());
        // 15 + 10 + 10 + 20 + 10 = 65.
        assert_eq!(s.value(), 65);
        assert_eq!(s.reasons().len(), 5);
    }

    #[test]
    fn cold_lead_lands_zero_score() {
        let s = marketing_scorer().score(&cold_lead());
        assert_eq!(s.value(), 0);
        assert!(s.reasons().is_empty());
    }

    #[test]
    fn empty_scorer_returns_zero() {
        let s: HeuristicScorer<LeadCtx> = HeuristicScorer::new();
        assert_eq!(s.score(&corporate_lead()).value(), 0);
        assert_eq!(s.rule_count(), 0);
    }

    #[test]
    fn reason_traces_carry_labels() {
        let s = marketing_scorer().score(&corporate_lead());
        let labels: Vec<&str> = s.reasons().iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"corporate_domain"));
        assert!(labels.contains(&"replied_within_1h"));
        assert!(labels.contains(&"intent_ready_to_buy"));
        assert!(labels.contains(&"senior_signature"));
    }

    #[test]
    fn score_is_deterministic_for_same_input() {
        let scorer = marketing_scorer();
        let lead = corporate_lead();
        let a = scorer.score(&lead);
        let b = scorer.score(&lead);
        assert_eq!(a, b);
    }

    #[test]
    fn rules_with_negative_deltas_subtract() {
        let mut scorer = HeuristicScorer::new();
        scorer.push(HeuristicRule::new("good", 50, |_: &LeadCtx| true));
        scorer.push(HeuristicRule::new("bad", -30, |_: &LeadCtx| true));
        let s = scorer.score(&cold_lead());
        assert_eq!(s.value(), 20);
    }

    #[test]
    fn rules_can_carry_inline_detail() {
        let mut scorer = HeuristicScorer::new();
        scorer.push(HeuristicRule::with_detail(
            "intent_ready_to_buy",
            20,
            "el cliente respondió `quiero comprar`",
            |l: &LeadCtx| matches!(l.intent_label, Some("ready_to_buy")),
        ));
        let s = scorer.score(&corporate_lead());
        assert_eq!(s.reasons().len(), 1);
        assert_eq!(
            s.reasons()[0].detail.as_deref(),
            Some("el cliente respondió `quiero comprar`"),
        );
    }

    #[test]
    fn scorer_composition_via_add() {
        // Two separate scorers — domain alone + intent alone —
        // composed via `+` produce the same total + traces as
        // a single combined scorer.
        let mut domain = HeuristicScorer::new();
        domain.push(HeuristicRule::new("corporate_domain", 15, |l: &LeadCtx| {
            l.is_corporate
        }));
        let mut intent = HeuristicScorer::new();
        intent.push(HeuristicRule::new("ready_to_buy", 20, |l: &LeadCtx| {
            matches!(l.intent_label, Some("ready_to_buy"))
        }));
        let lead = corporate_lead();
        let combined = domain.score(&lead) + intent.score(&lead);
        assert_eq!(combined.value(), 35);
        assert_eq!(combined.reasons().len(), 2);
    }

    #[test]
    fn scorer_works_through_dyn_dispatch() {
        // Confirms the trait is object-safe + the impl can be
        // wrapped in `Box<dyn Scorer>`.
        let scorer: Box<dyn Scorer<LeadCtx>> = Box::new(marketing_scorer());
        let s = scorer.score(&corporate_lead());
        assert_eq!(s.value(), 65);
    }
}
