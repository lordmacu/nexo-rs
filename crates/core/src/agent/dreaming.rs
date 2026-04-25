//! Dreaming — Phase 10.6.
//!
//! Background memory consolidation. Adapted from OpenClaw's three-phase model
//! (`research/docs/concepts/dreaming.md`):
//!
//! 1. **Light** — collect every memory that has at least one recall event and
//!    dedupe by memory id. No durable writes.
//! 2. **REM** — summarize themes for the diary. No durable writes.
//! 3. **Deep** — rank candidates with a weighted score, apply gate thresholds,
//!    append survivors to `MEMORY.md`, record the promotion in SQLite, and
//!    log a summary line to `DREAMS.md`.
//!
//! Gates (`min_score`, `min_recall_count`, `min_unique_queries`) borrow
//! OpenClaw's defaults. Weights likewise. Promoted memories are recorded in
//! `memory_promotions` so subsequent sweeps skip them — the sweep is
//! idempotent even if the cron fires twice.
use chrono::{DateTime, Utc};
use nexo_config::types::agents::{DreamingWeightsYaml, DreamingYamlConfig};
use nexo_memory::{LongTermMemory, RecallSignals};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
impl From<DreamingYamlConfig> for DreamingConfig {
    fn from(y: DreamingYamlConfig) -> Self {
        Self {
            enabled: y.enabled,
            interval_secs: y.interval_secs,
            min_score: y.min_score,
            min_recall_count: y.min_recall_count,
            min_unique_queries: y.min_unique_queries,
            weights: DreamWeights::from(y.weights),
            max_promotions_per_sweep: y.max_promotions_per_sweep,
        }
    }
}
impl From<DreamingWeightsYaml> for DreamWeights {
    fn from(w: DreamingWeightsYaml) -> Self {
        Self {
            frequency: w.frequency,
            relevance: w.relevance,
            recency: w.recency,
            diversity: w.diversity,
            consolidation: w.consolidation,
        }
    }
}
/// Config for a single dreaming sweep. Weights + gate thresholds; loaded from
/// YAML in `main.rs` (Phase 10.6 wiring).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DreamingConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Interval between sweeps, in seconds. Default: 24h.
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    #[serde(default = "default_min_score")]
    pub min_score: f32,
    #[serde(default = "default_min_recall_count")]
    pub min_recall_count: u32,
    #[serde(default = "default_min_unique_queries")]
    pub min_unique_queries: u32,
    #[serde(default)]
    pub weights: DreamWeights,
    #[serde(default = "default_max_promotions_per_sweep")]
    pub max_promotions_per_sweep: usize,
}
fn default_interval_secs() -> u64 {
    86_400
}
fn default_min_score() -> f32 {
    0.35
}
fn default_min_recall_count() -> u32 {
    3
}
fn default_min_unique_queries() -> u32 {
    2
}
fn default_max_promotions_per_sweep() -> usize {
    20
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DreamWeights {
    pub frequency: f32,
    pub relevance: f32,
    pub recency: f32,
    pub diversity: f32,
    pub consolidation: f32,
}
impl Default for DreamWeights {
    // OpenClaw defaults (docs/concepts/dreaming.md), minus conceptual_richness
    // (0.06) which is deferred to Phase 10.7.
    fn default() -> Self {
        Self {
            frequency: 0.24,
            relevance: 0.30,
            recency: 0.15,
            diversity: 0.15,
            consolidation: 0.10,
        }
    }
}
impl Default for DreamingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: default_interval_secs(),
            min_score: default_min_score(),
            min_recall_count: default_min_recall_count(),
            min_unique_queries: default_min_unique_queries(),
            weights: DreamWeights::default(),
            max_promotions_per_sweep: default_max_promotions_per_sweep(),
        }
    }
}
/// One candidate considered by the deep phase.
#[derive(Debug, Clone)]
pub struct DreamCandidate {
    pub memory_id: Uuid,
    pub content: String,
    pub signals: RecallSignals,
    pub score: f32,
    /// `true` when every gate threshold is met.
    pub passed_gates: bool,
}
/// Summary of a single sweep — returned to callers and persisted to DREAMS.md.
#[derive(Debug, Clone)]
pub struct DreamReport {
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub agent_id: String,
    pub candidates_considered: usize,
    pub promoted: Vec<DreamCandidate>,
    pub skipped_already_promoted: usize,
}
pub struct DreamEngine {
    memory: std::sync::Arc<LongTermMemory>,
    workspace: PathBuf,
    config: DreamingConfig,
}
impl DreamEngine {
    pub fn new(
        memory: std::sync::Arc<LongTermMemory>,
        workspace: impl Into<PathBuf>,
        config: DreamingConfig,
    ) -> Self {
        Self {
            memory,
            workspace: workspace.into(),
            config,
        }
    }
    pub fn config(&self) -> &DreamingConfig {
        &self.config
    }
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
    /// Run one full sweep (light → REM → deep). Safe to call repeatedly:
    /// promoted memories are persisted in `memory_promotions` so later sweeps
    /// skip them.
    pub async fn run_sweep(&self, agent_id: &str) -> anyhow::Result<DreamReport> {
        let started_at = Utc::now();
        tracing::info!(
            agent_id = %agent_id,
            workspace = %self.workspace.display(),
            "dream sweep started"
        );
        // ── Light: gather every memory with at least one recall event ──────
        let recalled = self.memory.recalled_memories(agent_id).await?;
        let candidates_considered = recalled.len();
        // ── Deep: score + gate + promote ──────────────────────────────────
        let mut scored: Vec<DreamCandidate> = Vec::with_capacity(recalled.len());
        let mut skipped_already_promoted = 0usize;
        for (memory_id, content) in recalled {
            if self.memory.is_promoted(memory_id).await.unwrap_or(false) {
                skipped_already_promoted += 1;
                continue;
            }
            let signals = self
                .memory
                .recall_signals(agent_id, memory_id, None)
                .await?;
            let score = self.score(&signals);
            let passed_gates = signals.recall_count >= self.config.min_recall_count
                && signals.unique_days.max(1) >= 1
                && distinct_queries_for(&signals) >= self.config.min_unique_queries
                && score >= self.config.min_score;
            scored.push(DreamCandidate {
                memory_id,
                content,
                signals,
                score,
                passed_gates,
            });
        }
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut promoted: Vec<DreamCandidate> = Vec::new();
        for cand in scored.iter() {
            if !cand.passed_gates {
                continue;
            }
            if promoted.len() >= self.config.max_promotions_per_sweep {
                break;
            }
            promoted.push(cand.clone());
        }
        // ── Durable writes: MEMORY.md append + SQLite promotion ledger ────
        if !promoted.is_empty() {
            self.append_to_memory_md(&promoted, started_at).await?;
            for cand in &promoted {
                // Phase 10.7: backfill concept_tags on promoted rows so recall
                // query expansion can find them later. Rows inserted before
                // 10.7 (or via paths that bypassed `remember`) have '[]'.
                let tags = nexo_memory::derive_concept_tags(
                    "",
                    &cand.content,
                    nexo_memory::MAX_CONCEPT_TAGS,
                );
                if !tags.is_empty() {
                    if let Err(e) = self.memory.set_concept_tags(cand.memory_id, &tags).await {
                        tracing::warn!(
                            memory_id = %cand.memory_id,
                            error = %e,
                            "failed to backfill concept_tags on promoted memory"
                        );
                    }
                }
                self.memory
                    .mark_promoted(agent_id, cand.memory_id, cand.score, "deep")
                    .await?;
            }
        }
        let finished_at = Utc::now();
        let report = DreamReport {
            started_at,
            finished_at,
            agent_id: agent_id.to_string(),
            candidates_considered,
            promoted,
            skipped_already_promoted,
        };
        // ── REM: diary entry (human-readable) ─────────────────────────────
        if let Err(e) = self.append_to_dreams_md(&report).await {
            tracing::warn!(
                agent_id = %agent_id,
                error = %e,
                "DREAMS.md diary append failed — sweep result still valid"
            );
        }
        tracing::info!(
            agent_id = %agent_id,
            candidates = report.candidates_considered,
            promoted = report.promoted.len(),
            skipped = report.skipped_already_promoted,
            "dream sweep finished"
        );
        Ok(report)
    }
    /// Deep-phase weighted score. Uses `consolidation = unique_days / 5` as a
    /// proxy until multi-day recurrence gets its own tracker.
    pub fn score(&self, s: &RecallSignals) -> f32 {
        let consolidation = (s.unique_days as f32 / 5.0).min(1.0);
        let w = &self.config.weights;
        w.frequency * s.frequency
            + w.relevance * s.relevance
            + w.recency * s.recency
            + w.diversity * s.diversity
            + w.consolidation * consolidation
    }
    async fn append_to_memory_md(
        &self,
        promoted: &[DreamCandidate],
        at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(&self.workspace).await?;
        let path = self.workspace.join("MEMORY.md");
        let existed = tokio::fs::try_exists(&path).await.unwrap_or(false);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        // First write seeds a top-level heading so the file reads cleanly.
        if !existed {
            file.write_all(b"# MEMORY.md\n\n").await?;
        }
        let mut block = String::new();
        block.push_str(&format!(
            "\n## Dreamed {}\n\n",
            at.format("%Y-%m-%d %H:%M UTC")
        ));
        for cand in promoted {
            block.push_str(&format!(
                "- {} _(score={:.2}, hits={}, days={})_\n",
                cand.content.trim(),
                cand.score,
                cand.signals.recall_count,
                cand.signals.unique_days
            ));
        }
        file.write_all(block.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }
    async fn append_to_dreams_md(&self, report: &DreamReport) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(&self.workspace).await?;
        let path = self.workspace.join("DREAMS.md");
        let existed = tokio::fs::try_exists(&path).await.unwrap_or(false);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        if !existed {
            file.write_all("# DREAMS.md — Dream Diary\n\n".as_bytes())
                .await?;
        }
        let mut block = String::new();
        block.push_str(&format!(
            "\n## Deep Sleep {}\n\n",
            report.started_at.format("%Y-%m-%d %H:%M UTC")
        ));
        block.push_str(&format!(
            "- candidates: {}\n- promoted: {}\n- skipped (already promoted): {}\n",
            report.candidates_considered,
            report.promoted.len(),
            report.skipped_already_promoted,
        ));
        if !report.promoted.is_empty() {
            block.push_str("\n### Promoted\n\n");
            for cand in &report.promoted {
                block.push_str(&format!(
                    "- {} — score {:.2}, hits {}\n",
                    cand.content.trim(),
                    cand.score,
                    cand.signals.recall_count,
                ));
            }
        }
        file.write_all(block.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }
}
/// Distinct-query count reconstruction — the signals struct exposes diversity
/// as a normalized float, but we also need the raw distinct-query count for
/// the `min_unique_queries` gate. We re-derive from the normalization rule
/// (`diversity = min(raw_count, 5) / 5`).
fn distinct_queries_for(s: &RecallSignals) -> u32 {
    (s.diversity * 5.0).round() as u32
}
#[cfg(test)]
mod tests {
    use super::*;
    fn mk_engine(
        ws: &Path,
        cfg: DreamingConfig,
        memory: std::sync::Arc<LongTermMemory>,
    ) -> DreamEngine {
        DreamEngine::new(memory, ws, cfg)
    }
    async fn seed_db() -> std::sync::Arc<LongTermMemory> {
        std::sync::Arc::new(LongTermMemory::open(":memory:").await.unwrap())
    }
    fn tmp_ws(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dream-{label}-{}", Uuid::new_v4()))
    }
    #[tokio::test]
    async fn empty_memory_yields_empty_report() -> anyhow::Result<()> {
        let ws = tmp_ws("empty");
        let mem = seed_db().await;
        let engine = mk_engine(&ws, DreamingConfig::default(), mem);
        let report = engine.run_sweep("kate").await?;
        assert_eq!(report.candidates_considered, 0);
        assert_eq!(report.promoted.len(), 0);
        assert!(
            !ws.join("MEMORY.md").exists(),
            "no MEMORY.md without promotions"
        );
        // DREAMS.md is always written — even for empty sweeps — so the diary is auditable.
        assert!(ws.join("DREAMS.md").exists());
        tokio::fs::remove_dir_all(&ws).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn gate_filters_candidates_below_thresholds() -> anyhow::Result<()> {
        let ws = tmp_ws("gates");
        let mem = seed_db().await;
        // m_strong: 3 hits, 2 distinct queries, high score → promote
        let strong = mem.remember("kate", "user likes dark mode", &[]).await?;
        mem.record_recall_event("kate", strong, "dark", 1.0).await?;
        mem.record_recall_event("kate", strong, "dark mode", 1.0)
            .await?;
        mem.record_recall_event("kate", strong, "preferences", 1.0)
            .await?;
        // m_weak: 1 hit, 1 query → fails min_recall_count
        let weak = mem.remember("kate", "random detail", &[]).await?;
        mem.record_recall_event("kate", weak, "q", 0.5).await?;
        let engine = mk_engine(&ws, DreamingConfig::default(), mem);
        let report = engine.run_sweep("kate").await?;
        assert_eq!(report.candidates_considered, 2);
        assert_eq!(report.promoted.len(), 1, "only strong candidate promotes");
        assert_eq!(report.promoted[0].memory_id, strong);
        let md = tokio::fs::read_to_string(ws.join("MEMORY.md")).await?;
        assert!(md.contains("user likes dark mode"));
        assert!(!md.contains("random detail"));
        tokio::fs::remove_dir_all(&ws).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn idempotent_sweep_does_not_promote_twice() -> anyhow::Result<()> {
        let ws = tmp_ws("idempotent");
        let mem = seed_db().await;
        let id = mem.remember("kate", "important fact", &[]).await?;
        for q in ["q1", "q2", "q3"] {
            mem.record_recall_event("kate", id, q, 1.0).await?;
        }
        let engine = mk_engine(&ws, DreamingConfig::default(), mem.clone());
        let first = engine.run_sweep("kate").await?;
        assert_eq!(first.promoted.len(), 1);
        let second = engine.run_sweep("kate").await?;
        assert_eq!(second.promoted.len(), 0, "already promoted must be skipped");
        assert_eq!(second.skipped_already_promoted, 1);
        // MEMORY.md must contain exactly one "important fact" line.
        let md = tokio::fs::read_to_string(ws.join("MEMORY.md")).await?;
        let count = md.matches("important fact").count();
        assert_eq!(
            count, 1,
            "fact appended exactly once; got {count} in:\n{md}"
        );
        tokio::fs::remove_dir_all(&ws).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn score_respects_configured_weights() {
        // Zero-out everything except relevance — score should equal relevance.
        let cfg = DreamingConfig {
            weights: DreamWeights {
                frequency: 0.0,
                relevance: 1.0,
                recency: 0.0,
                diversity: 0.0,
                consolidation: 0.0,
            },
            ..DreamingConfig::default()
        };
        let mem = std::sync::Arc::new(LongTermMemory::open(":memory:").await.unwrap());
        let engine = DreamEngine::new(mem, "/tmp/unused", cfg);
        let s = RecallSignals {
            frequency: 1.0,
            relevance: 0.42,
            recency: 1.0,
            diversity: 1.0,
            recall_count: 9,
            unique_days: 3,
        };
        assert!((engine.score(&s) - 0.42).abs() < 1e-5);
    }
    #[tokio::test]
    async fn max_promotions_caps_output() -> anyhow::Result<()> {
        let ws = tmp_ws("cap");
        let mem = seed_db().await;
        for i in 0..5 {
            let id = mem.remember("kate", &format!("fact {i}"), &[]).await?;
            for q in ["q1", "q2", "q3"] {
                mem.record_recall_event("kate", id, q, 1.0).await?;
            }
        }
        let cfg = DreamingConfig {
            max_promotions_per_sweep: 2,
            ..DreamingConfig::default()
        };
        let engine = mk_engine(&ws, cfg, mem);
        let report = engine.run_sweep("kate").await?;
        assert_eq!(report.candidates_considered, 5);
        assert_eq!(report.promoted.len(), 2, "cap honored");
        tokio::fs::remove_dir_all(&ws).await.ok();
        Ok(())
    }
}
