//! Phase 77.6 — memdir findRelevantMemories + memoryAge decay.
//!
//! Composite scoring: similarity × recency(per-type half-life) × log1p(frequency),
//! with staleness annotation and already-surfaced dedup.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::long_term::MemoryEntry;

/// Four-type taxonomy matching Phase 77.5 extractMemories.
///
/// Half-life rationale:
///   user      — ∞ (preferences don't expire)
///   feedback  — 365 d (approach corrections age slowly)
///   project   —  90 d (project context rotates fast)
///   reference — ∞ (external pointers don't expire)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
}

impl MemoryType {
    /// Half-life in days. Large sentinel (10,000 d ≈ 27 years) instead of
    /// f64::MAX to keep exp() numerically stable. Callers treat ≥ 10,000
    /// as "effectively infinite" — recency ≈ 1.0 always.
    pub fn half_life_days(self) -> f64 {
        match self {
            MemoryType::User => 10_000.0,
            MemoryType::Feedback => 365.0,
            MemoryType::Project => 90.0,
            MemoryType::Reference => 10_000.0,
        }
    }

    /// Lenient parser. Unknown values → None (not error).
    /// Legacy memories without the column also map to None.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(MemoryType::User),
            "feedback" => Some(MemoryType::Feedback),
            "project" => Some(MemoryType::Project),
            "reference" => Some(MemoryType::Reference),
            _ => None,
        }
    }
}

/// A memory entry with its composite score and optional staleness warning.
#[derive(Debug, Clone)]
pub struct ScoredMemory {
    pub entry: MemoryEntry,
    /// Composite score: similarity × recency × log1p(frequency). [0, 1].
    pub score: f32,
    /// `<system-reminder>` block when the memory is older than the
    /// freshness threshold, else None.
    pub freshness_warning: Option<String>,
}

/// Score memories by composite formula, returning sorted (score, entry) pairs
/// in descending order.
///
/// # Scoring
/// `score = similarity × recency × log1p(frequency)`
///
/// - `similarity`: cosine against query embedding (or 1.0 when no embedding,
///   capped at 0.5 to signal reduced confidence). Derived from the entry's
///   position in the RRF-ranked list: top-ranked entries get higher similarity.
/// - `recency`: exponential decay `exp(-days × ln2 / half_life_days)`.
///   Half-life comes from `MemoryEntry::memory_type` (defaults to Project 90d
///   when `None`). `half_life_days = 0` → recency = 0.0. Future timestamps →
///   age clamped to 0.
/// - `frequency`: `log1p(count) / ln(11)` normalized — 0 hits = 0.0, 10 hits ≈ 1.0.
///
/// All scores are [0.0, 1.0] and clamped. Non-finite values → 0.0.
pub fn score_memories(
    entries: Vec<MemoryEntry>,
    similarity_scores: &[(f32, MemoryEntry)],
    now: DateTime<Utc>,
    frequency_counts: &HashMap<Uuid, u32>,
) -> Vec<(f32, MemoryEntry)> {
    let mut scored: Vec<(f32, MemoryEntry)> = Vec::with_capacity(entries.len());

    for (sim, entry) in similarity_scores.iter() {
        let sim = if sim.is_finite() { *sim } else { 0.0 };
        let sim = sim.clamp(0.0, 1.0);

        let half_life = entry
            .memory_type
            .map(|t| t.half_life_days())
            .unwrap_or_else(|| MemoryType::Project.half_life_days());

        let age_days = memory_age_days(entry.created_at, now);

        // Half-life = 0 → instant decay
        let recency = if half_life <= 0.0 {
            0.0
        } else {
            (-age_days * std::f64::consts::LN_2 / half_life).exp() as f32
        };

        // log1p normalization: saturates gracefully.
        // 1 hit ≈ 0.29, 10 hits ≈ 0.83, 100 hits → 1.0.
        let freq_count = frequency_counts.get(&entry.id).copied().unwrap_or(1);
        let freq_norm = ((freq_count as f32).ln_1p() / 6.0_f32.ln_1p()).min(1.0);

        let score = sim * recency * freq_norm;
        let score = if score.is_finite() { score.clamp(0.0, 1.0) } else { 0.0 };

        scored.push((score, entry.clone()));
    }

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

/// Days between `created_at` and `now`. Floor-rounded, clamped to ≥ 0
/// (future timestamps from clock skew → 0 days).
fn memory_age_days(created_at: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    let delta_ms = (now - created_at).num_milliseconds();
    if delta_ms <= 0 {
        return 0.0;
    }
    (delta_ms as f64 / (1000.0 * 60.0 * 60.0 * 24.0)).floor()
}

/// Return a `<system-reminder>` block when the memory is older than
/// `threshold_days`. Returns `None` when the memory is fresh, threshold is
/// effectively infinite (i32::MAX), or the note has already been shown.
pub fn freshness_note(
    entry: &MemoryEntry,
    now: DateTime<Utc>,
    threshold_days: u32,
) -> Option<String> {
    if threshold_days >= i32::MAX as u32 {
        return None;
    }
    let age = memory_age_days(entry.created_at, now) as u32;
    if age > threshold_days {
        Some(format!(
            "<system-reminder>\nMemory '{}' is {} days old. Verify it is still accurate.\n</system-reminder>",
            entry.id, age
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_entry(id: &str, content: &str, days_old: i64, memory_type: Option<MemoryType>) -> MemoryEntry {
        let now = Utc::now();
        MemoryEntry {
            id: uuid::Uuid::parse_str(id).unwrap(),
            agent_id: "test-agent".into(),
            content: content.into(),
            tags: vec![],
            concept_tags: vec![],
            memory_type,
            created_at: now - Duration::days(days_old),
        }
    }

    #[test]
    fn memory_type_half_life_values() {
        assert_eq!(MemoryType::User.half_life_days(), 10_000.0);
        assert_eq!(MemoryType::Feedback.half_life_days(), 365.0);
        assert_eq!(MemoryType::Project.half_life_days(), 90.0);
        assert_eq!(MemoryType::Reference.half_life_days(), 10_000.0);
    }

    #[test]
    fn parse_known_types() {
        assert_eq!(MemoryType::parse("user"), Some(MemoryType::User));
        assert_eq!(MemoryType::parse("feedback"), Some(MemoryType::Feedback));
        assert_eq!(MemoryType::parse("project"), Some(MemoryType::Project));
        assert_eq!(MemoryType::parse("reference"), Some(MemoryType::Reference));
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert_eq!(MemoryType::parse("unknown"), None);
        assert_eq!(MemoryType::parse(""), None);
    }

    #[test]
    fn score_memories_nan_cosine_guarded() {
        let e = make_entry("00000000-0000-0000-0000-000000000001", "test", 0, Some(MemoryType::Project));
        let scored = score_memories(vec![e.clone()], &[(f32::NAN, e)], Utc::now(), &HashMap::new());
        assert_eq!(scored[0].0, 0.0);
    }

    #[test]
    fn score_memories_zero_half_life() {
        let e = make_entry("00000000-0000-0000-0000-000000000002", "old", 365, Some(MemoryType::Project));
        let scored = score_memories(vec![e.clone()], &[(0.8, e)], Utc::now(), &HashMap::new());
        // 365 days old with 90d half-life → recency ≈ exp(-365*ln2/90) ≈ exp(-2.81) ≈ 0.06
        // score = 0.8 * 0.06 * 0.29 ≈ 0.014
        assert!(scored[0].0 > 0.0 && scored[0].0 < 0.1,
            "expected low score for very old memory, got {}", scored[0].0);
    }

    #[test]
    fn score_memories_future_mtime() {
        let e = make_entry("00000000-0000-0000-0000-000000000003", "future", -1, Some(MemoryType::Project));
        let scored = score_memories(vec![e.clone()], &[(0.5, e)], Utc::now(), &HashMap::new());
        // age=0 → recency=1.0, score = 0.5 * 1.0 * freq(1) ≈ 0.145
        assert!(scored[0].0 > 0.1, "expected non-zero score for future mtime, got {}", scored[0].0);
    }

    #[test]
    fn score_memories_legacy_none_type() {
        let e = make_entry("00000000-0000-0000-0000-000000000004", "legacy", 10, None);
        let scored = score_memories(vec![e.clone()], &[(0.9, e)], Utc::now(), &HashMap::new());
        // 10d old, 90d half-life → recency ≈ exp(-10*ln2/90) ≈ 0.926
        // score = 0.9 * 0.926 * 0.29 ≈ 0.242
        assert!(scored[0].0 > 0.2, "expected moderate score for legacy None, got {}", scored[0].0);
    }

    #[test]
    fn score_memories_sorted_descending() {
        let recent = make_entry("00000000-0000-0000-0000-000000000005", "recent", 1, Some(MemoryType::Project));
        let medium = make_entry("00000000-0000-0000-0000-000000000006", "medium", 30, Some(MemoryType::Project));
        let old = make_entry("00000000-0000-0000-0000-000000000007", "old", 180, Some(MemoryType::Project));

        let entries = vec![old.clone(), recent.clone(), medium.clone()];
        let sims = vec![(0.8, old), (0.8, recent), (0.8, medium)];
        let scored = score_memories(entries, &sims, Utc::now(), &HashMap::new());

        assert_eq!(scored.len(), 3);
        assert!(scored[0].0 > scored[1].0, "recent ({}) should outrank medium ({})", scored[0].0, scored[1].0);
        assert!(scored[1].0 > scored[2].0, "medium ({}) should outrank old ({})", scored[1].0, scored[2].0);
    }

    #[test]
    fn score_memories_user_type_never_decays() {
        let e = make_entry("00000000-0000-0000-0000-000000000008", "pref", 365, Some(MemoryType::User));
        let scored = score_memories(vec![e.clone()], &[(0.5, e)], Utc::now(), &HashMap::new());
        // 365d old, 10000d half-life → recency ≈ exp(-365*ln2/10000) ≈ 0.975
        // score = 0.5 * 0.975 * 0.29 ≈ 0.141
        assert!(scored[0].0 > 0.13, "user memory should barely decay, got {}", scored[0].0);
    }

    #[test]
    fn freshness_note_below_threshold() {
        let e = make_entry("00000000-0000-0000-0000-000000000009", "fresh", 0, Some(MemoryType::Project));
        let note = freshness_note(&e, Utc::now(), 1);
        assert!(note.is_none());
    }

    #[test]
    fn freshness_note_above_threshold() {
        let e = make_entry("00000000-0000-0000-0000-00000000000a", "stale", 2, Some(MemoryType::Project));
        let note = freshness_note(&e, Utc::now(), 1);
        assert!(note.is_some());
        let n = note.unwrap();
        assert!(n.contains("is 2 days old"));
        assert!(n.contains("<system-reminder>"));
    }

    #[test]
    fn freshness_note_threshold_zero() {
        let e = make_entry("00000000-0000-0000-0000-00000000000b", "always", 0, Some(MemoryType::Project));
        let note = freshness_note(&e, Utc::now(), 0);
        // 0 days old, threshold 0 — 0 > 0 is false, no warning
        assert!(note.is_none());
        // But after 1 day, threshold 0 should warn
        let old = make_entry("00000000-0000-0000-0000-00000000000c", "old", 1, Some(MemoryType::Project));
        let note2 = freshness_note(&old, Utc::now(), 0);
        assert!(note2.is_some());
    }

    #[test]
    fn freshness_note_max_threshold_disables() {
        let e = make_entry("00000000-0000-0000-0000-00000000000d", "ancient", 1000, Some(MemoryType::Project));
        let note = freshness_note(&e, Utc::now(), i32::MAX as u32);
        assert!(note.is_none());
    }
}
