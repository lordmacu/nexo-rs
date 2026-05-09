//! Topic guardrails (M15.23.d).
//!
//! Operator-configured regex tagger that scans inbound text
//! against a list of `(id, patterns, action)` rules and
//! returns every match. Used by any microapp / extension
//! that wants to gate autonomous behaviour on topic-
//! sensitive content (pricing quotes, legal questions,
//! complaints, contract terms, …).
//!
//! Pure logic; no async. Patterns are compiled once at
//! ruleset-build time and cached in the [`GuardrailSet`]
//! instance — repeat calls against the same set don't pay
//! re-compilation.
//!
//! ## Actions
//!
//! - [`GuardrailAction::ForceApproval`] — operator must
//!   approve the AI-generated draft before it sends.
//!   Autonomous mode demoted to draft mode for this thread.
//! - [`GuardrailAction::Block`] — refuse to draft a reply
//!   at all. Lead lands in the operator queue with the
//!   guardrail tag attached.
//!
//! ## Pattern syntax
//!
//! Each `pattern` is a `regex` crate pattern, ASCII case-
//! insensitive by default (the loader prepends `(?i)` when
//! the pattern doesn't already declare flags). Multiple
//! patterns inside one guardrail rule OR together — match
//! ANY pattern ⇒ rule fires.

use std::collections::HashSet;

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// What to do when a guardrail rule fires. Caller's draft
/// pipeline branches on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardrailAction {
    /// Demote autonomous-reply mode to draft-mode for this
    /// thread. Operator approves before send.
    ForceApproval,
    /// Refuse to draft anything; lead waits in the operator
    /// queue.
    Block,
}

/// Wire shape — what the operator authors in YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardrailRule {
    /// Stable machine id, used for analytics joins +
    /// audit-row tags. Convention: `snake_case`.
    pub id: String,
    /// Operator-facing label rendered in the UI banner.
    pub name: String,
    /// At least one regex pattern. Empty list ⇒ rule never
    /// fires (loader rejects this case).
    pub patterns: Vec<String>,
    /// Action when any pattern matches.
    pub action: GuardrailAction,
}

/// Errors building a [`GuardrailSet`] from raw rules.
/// Surfaced at boot + on `PUT /config/topic_guardrails` so
/// the operator sees a precise error instead of silent skip.
#[derive(Debug, Error)]
pub enum GuardrailLoadError {
    /// One or more patterns failed to compile.
    #[error("guardrail {rule_id:?} pattern {index} invalid: {error}")]
    InvalidPattern {
        /// Rule that owns the bad pattern.
        rule_id: String,
        /// Zero-based index of the pattern within the
        /// rule's `patterns` vec.
        index: usize,
        /// Underlying `regex` crate error message.
        error: String,
    },
    /// Two rules share the same `id` — would be ambiguous in
    /// the audit + analytics layer.
    #[error("guardrail rule id {0:?} duplicated")]
    DuplicateId(String),
    /// A rule has zero patterns — would never fire; reject
    /// at load time so the operator catches the typo.
    #[error("guardrail {0:?} has no patterns")]
    EmptyRule(String),
}

/// Compiled guardrail set, ready for [`scan`]. Cheap to
/// clone (every internal `Regex` is `Send + Sync` and
/// pattern caches are owned).
#[derive(Debug, Clone)]
pub struct GuardrailSet {
    rules: Vec<CompiledRule>,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    id: String,
    name: String,
    action: GuardrailAction,
    patterns: Vec<Regex>,
}

impl GuardrailSet {
    /// Build from operator-supplied rules. Rejects empty
    /// pattern lists, duplicate ids, and invalid regex.
    pub fn build(rules: Vec<GuardrailRule>) -> Result<Self, GuardrailLoadError> {
        let mut seen = HashSet::with_capacity(rules.len());
        let mut compiled = Vec::with_capacity(rules.len());
        for rule in rules {
            if rule.patterns.is_empty() {
                return Err(GuardrailLoadError::EmptyRule(rule.id));
            }
            if !seen.insert(rule.id.clone()) {
                return Err(GuardrailLoadError::DuplicateId(rule.id));
            }
            let mut pats = Vec::with_capacity(rule.patterns.len());
            for (i, raw) in rule.patterns.iter().enumerate() {
                let prepared = if raw.starts_with("(?") {
                    raw.clone()
                } else {
                    format!("(?i){raw}")
                };
                let re = Regex::new(&prepared).map_err(|e| GuardrailLoadError::InvalidPattern {
                    rule_id: rule.id.clone(),
                    index: i,
                    error: e.to_string(),
                })?;
                pats.push(re);
            }
            compiled.push(CompiledRule {
                id: rule.id,
                name: rule.name,
                action: rule.action,
                patterns: pats,
            });
        }
        Ok(Self { rules: compiled })
    }

    /// Empty set — every scan returns no matches.
    pub fn empty() -> Self {
        Self { rules: Vec::new() }
    }

    /// Number of compiled rules — exposed for boot logging
    /// + tests.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Scan `text` against every rule. Returns one
    /// [`GuardrailMatch`] per rule that fired, in
    /// configuration order.
    ///
    /// Stops scanning further patterns within a rule on the
    /// first hit (one rule can fire at most once even if
    /// multiple patterns match).
    pub fn scan(&self, text: &str) -> Vec<GuardrailMatch> {
        let mut hits = Vec::new();
        for rule in &self.rules {
            for (i, pat) in rule.patterns.iter().enumerate() {
                if let Some(m) = pat.find(text) {
                    hits.push(GuardrailMatch {
                        rule_id: rule.id.clone(),
                        rule_name: rule.name.clone(),
                        action: rule.action,
                        matched_pattern_index: i,
                        excerpt: extract_excerpt(text, m.start(), m.end()),
                    });
                    break;
                }
            }
        }
        hits
    }

    /// Convenience: `true` when any rule fires `Block`.
    pub fn has_block_match(matches: &[GuardrailMatch]) -> bool {
        matches.iter().any(|m| m.action == GuardrailAction::Block)
    }

    /// Convenience: `true` when any rule fires `ForceApproval`.
    pub fn has_force_approval_match(matches: &[GuardrailMatch]) -> bool {
        matches
            .iter()
            .any(|m| m.action == GuardrailAction::ForceApproval)
    }
}

/// One rule that fired during a scan. Caller threads this
/// into the audit log + the operator UI banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardrailMatch {
    /// Rule id (machine label).
    pub rule_id: String,
    /// Operator-facing rule name.
    pub rule_name: String,
    /// What the caller's draft pipeline does next.
    pub action: GuardrailAction,
    /// Zero-based index of the rule's pattern that matched.
    pub matched_pattern_index: usize,
    /// Up to 80 chars around the match — surfaces in the
    /// operator UI ("…la propuesta de **precio** y descuento…").
    pub excerpt: String,
}

/// Pull a short excerpt centred on the match. Bounded so
/// audit rows don't bloat — operator UI has the full body
/// available separately.
fn extract_excerpt(text: &str, start: usize, end: usize) -> String {
    const RADIUS: usize = 30;
    // Use char indices to avoid splitting on a UTF-8 boundary.
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut start_idx = 0;
    let mut end_idx = chars.len();
    for (i, (b, _)) in chars.iter().enumerate() {
        if *b >= start.saturating_sub(RADIUS) && start_idx == 0 {
            start_idx = i;
        }
        if *b >= end + RADIUS {
            end_idx = i;
            break;
        }
    }
    let prefix = if start_idx > 0 { "…" } else { "" };
    let suffix = if end_idx < chars.len() { "…" } else { "" };
    let body: String = chars[start_idx..end_idx].iter().map(|(_, c)| *c).collect();
    format!("{prefix}{body}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, action: GuardrailAction, patterns: &[&str]) -> GuardrailRule {
        GuardrailRule {
            id: id.into(),
            name: id.into(),
            patterns: patterns.iter().map(|s| s.to_string()).collect(),
            action,
        }
    }

    fn pricing_set() -> GuardrailSet {
        GuardrailSet::build(vec![
            rule(
                "pricing_quotes",
                GuardrailAction::ForceApproval,
                &[r"\bprecio\b", r"\bcotizaci[oó]n\b", r"\bpricing\b"],
            ),
            rule(
                "legal",
                GuardrailAction::Block,
                &[r"\bcontrato\b", r"\bnda\b", r"\bclausula\b"],
            ),
        ])
        .unwrap()
    }

    // ─── Build ────────────────────────────────────────────────

    #[test]
    fn build_accepts_canonical_set() {
        let s = pricing_set();
        assert_eq!(s.rule_count(), 2);
    }

    #[test]
    fn build_rejects_empty_pattern_list() {
        let r = GuardrailSet::build(vec![rule("x", GuardrailAction::Block, &[])]);
        assert!(matches!(r, Err(GuardrailLoadError::EmptyRule(_))));
    }

    #[test]
    fn build_rejects_duplicate_ids() {
        let r = GuardrailSet::build(vec![
            rule("dup", GuardrailAction::Block, &["a"]),
            rule("dup", GuardrailAction::ForceApproval, &["b"]),
        ]);
        assert!(matches!(r, Err(GuardrailLoadError::DuplicateId(_))));
    }

    #[test]
    fn build_rejects_invalid_regex() {
        let r = GuardrailSet::build(vec![rule("x", GuardrailAction::Block, &["[unclosed"])]);
        assert!(matches!(r, Err(GuardrailLoadError::InvalidPattern { .. })));
    }

    // ─── Scan ─────────────────────────────────────────────────

    #[test]
    fn scan_pricing_match_force_approval() {
        let s = pricing_set();
        let m = s.scan("Necesito el precio del plan enterprise");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].rule_id, "pricing_quotes");
        assert_eq!(m[0].action, GuardrailAction::ForceApproval);
        assert_eq!(m[0].matched_pattern_index, 0);
        assert!(m[0].excerpt.contains("precio"));
    }

    #[test]
    fn scan_legal_match_block() {
        let s = pricing_set();
        let m = s.scan("Mándame el contrato firmado");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].rule_id, "legal");
        assert_eq!(m[0].action, GuardrailAction::Block);
    }

    #[test]
    fn scan_multiple_rules_fire_in_order() {
        let s = pricing_set();
        let m = s.scan("Necesito el precio + envíame el contrato");
        // Both rules match — order matches configuration.
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].rule_id, "pricing_quotes");
        assert_eq!(m[1].rule_id, "legal");
    }

    #[test]
    fn scan_one_rule_fires_at_most_once() {
        // "precio" + "cotización" both match the pricing
        // rule — only one match row is yielded.
        let s = pricing_set();
        let m = s.scan("El precio y la cotización ya las tengo");
        assert_eq!(m.len(), 1);
        // First pattern hit wins.
        assert_eq!(m[0].matched_pattern_index, 0);
    }

    #[test]
    fn scan_case_insensitive_by_default() {
        let s = pricing_set();
        let m = s.scan("PRECIO total del proyecto");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].rule_id, "pricing_quotes");
    }

    #[test]
    fn scan_no_match_returns_empty() {
        let s = pricing_set();
        let m = s.scan("Hola, gracias por tu mensaje.");
        assert!(m.is_empty());
    }

    #[test]
    fn scan_empty_set_never_fires() {
        let s = GuardrailSet::empty();
        let m = s.scan("Necesito el precio del plan");
        assert!(m.is_empty());
    }

    #[test]
    fn scan_excerpt_carries_match_context() {
        let s = pricing_set();
        let m = s.scan(
            "Hola equipo, después de revisar el plan el precio que ofrecen es competitivo, ¿podemos avanzar?",
        );
        assert_eq!(m.len(), 1);
        // Excerpt windows around the match.
        assert!(m[0].excerpt.contains("precio"));
        // Bounded length — won't carry the whole body.
        assert!(m[0].excerpt.chars().count() < 200);
    }

    // ─── Helpers ──────────────────────────────────────────────

    #[test]
    fn has_block_match_distinguishes_action_kinds() {
        let s = pricing_set();
        let force_only = s.scan("Necesito el precio");
        assert!(GuardrailSet::has_force_approval_match(&force_only));
        assert!(!GuardrailSet::has_block_match(&force_only));

        let block_too = s.scan("Necesito el precio + el contrato");
        assert!(GuardrailSet::has_force_approval_match(&block_too));
        assert!(GuardrailSet::has_block_match(&block_too));
    }

    #[test]
    fn pattern_with_explicit_flags_is_left_alone() {
        // (?-i) disables case-insensitivity — caller can opt
        // out of the default by stamping their own flags.
        let s = GuardrailSet::build(vec![rule(
            "case_sensitive",
            GuardrailAction::Block,
            &[r"(?-i)PII"],
        )])
        .unwrap();
        assert!(s.scan("This carries PII").len() == 1);
        // Lower-case should NOT match the case-sensitive rule.
        assert!(s.scan("this carries pii").is_empty());
    }

    #[test]
    fn rule_count_reports_compiled_rules() {
        assert_eq!(GuardrailSet::empty().rule_count(), 0);
        assert_eq!(pricing_set().rule_count(), 2);
    }
}
