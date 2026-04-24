//! Relevance-based tool filter.
//!
//! Sending 146 tool definitions on every LLM turn is expensive:
//! context tokens scale linearly, and the model gets easily confused
//! by near-duplicates. We pre-index each tool's `(name, description)`
//! as a term-frequency vector and, on each turn, rank tools by cosine
//! similarity against the user prompt + recent assistant text.
//!
//! Top-K survive, plus any tool whose name appears verbatim in the
//! prompt (so `"github list issues"` can't accidentally drop the
//! github tools), plus any `always_include` glob from config.
//!
//! Zero network cost — pure TF-on-words. Good enough for the tool
//! routing problem: we're not doing semantic search over a 10k-doc
//! corpus, we're ranking 150 short tool descriptions.
use agent_llm::ToolDef;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RelevanceConfig {
    #[serde(default)]
    pub enabled: bool,
    /// How many top-ranked tools to keep. Tools matched verbatim or
    /// listed in `always_include` are added on top of this cap.
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    /// Minimum cosine score. Tools below this threshold are dropped
    /// even if they would otherwise sit in the top-K.
    #[serde(default = "default_min_score")]
    pub min_score: f32,
    /// Glob patterns (trailing `*` or exact) that always pass the
    /// filter — e.g. `"delegate"` so agent-to-agent routing never
    /// drops out.
    #[serde(default)]
    pub always_include: Vec<String>,
}
fn default_top_k() -> usize {
    24
}
fn default_min_score() -> f32 {
    0.01
}
pub struct ToolFilter {
    cfg: RelevanceConfig,
    /// tool_name → token-frequency vector
    index: HashMap<String, HashMap<String, f32>>,
    /// Cached norms for each tool.
    norms: HashMap<String, f32>,
    always_exact: HashSet<String>,
    always_prefix: Vec<String>,
}
impl ToolFilter {
    /// Build the index from the full tool catalog. Call once at boot
    /// or whenever the tool set changes.
    pub fn build(cfg: RelevanceConfig, tools: &[ToolDef]) -> Self {
        let mut index: HashMap<String, HashMap<String, f32>> = HashMap::new();
        let mut norms: HashMap<String, f32> = HashMap::new();
        for t in tools {
            let doc = format!("{} {}", t.name, t.description);
            let vec = tokenize_tf(&doc);
            let norm = vec
                .values()
                .map(|v| v * v)
                .sum::<f32>()
                .sqrt()
                .max(f32::EPSILON);
            norms.insert(t.name.clone(), norm);
            index.insert(t.name.clone(), vec);
        }
        let (always_exact, always_prefix) = split_globs(&cfg.always_include);
        Self {
            cfg,
            index,
            norms,
            always_exact,
            always_prefix,
        }
    }
    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }
    /// Rank tools against `prompt`, return the cut-down catalog. When
    /// disabled returns `tools` unchanged (a clone, cheap — `ToolDef`
    /// clones are small).
    ///
    /// Empty-prompt path: if the prompt tokenizes to nothing (sticker,
    /// media-only message, cleared text), ranking collapses to zeros
    /// for every tool — pretending the operator didn't ask for
    /// anything discards useful context. Pass the full catalog back
    /// instead so the LLM can at least reach for generic tools.
    pub fn filter(&self, prompt: &str, tools: &[ToolDef]) -> Vec<ToolDef> {
        if !self.cfg.enabled || tools.len() <= self.cfg.top_k {
            return tools.to_vec();
        }
        let q = tokenize_tf(prompt);
        if q.is_empty() {
            return tools.to_vec();
        }
        let q_norm = q
            .values()
            .map(|v| v * v)
            .sum::<f32>()
            .sqrt()
            .max(f32::EPSILON);
        let mut scored: Vec<(f32, &ToolDef)> = Vec::with_capacity(tools.len());
        for t in tools {
            let doc_vec = match self.index.get(&t.name) {
                Some(v) => v,
                None => continue,
            };
            let doc_norm = self.norms.get(&t.name).copied().unwrap_or(1.0);
            // Dot product — iterate the smaller vector for speed.
            let (small, large) = if q.len() < doc_vec.len() {
                (&q, doc_vec)
            } else {
                (doc_vec, &q)
            };
            let mut dot = 0.0f32;
            for (tok, w) in small {
                if let Some(w2) = large.get(tok) {
                    dot += w * w2;
                }
            }
            let score = dot / (q_norm * doc_norm);
            scored.push((score, t));
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut keep: HashMap<String, ToolDef> = HashMap::new();
        // Top-K above threshold.
        for (score, t) in scored.iter().take(self.cfg.top_k) {
            if *score >= self.cfg.min_score {
                keep.insert(t.name.clone(), (*t).clone());
            }
        }
        // Verbatim name mentions — lowercase both sides.
        let prompt_lower = prompt.to_lowercase();
        for t in tools {
            if prompt_lower.contains(&t.name.to_lowercase()) {
                keep.insert(t.name.clone(), t.clone());
            }
        }
        // Always-include globs.
        for t in tools {
            if self.always_exact.contains(&t.name)
                || self.always_prefix.iter().any(|p| t.name.starts_with(p))
            {
                keep.insert(t.name.clone(), t.clone());
            }
        }
        // Preserve catalog order in the output for stable tool lists
        // (helps prompt-cache hit rates on providers that tokenise the
        // tools array verbatim).
        let mut out = Vec::with_capacity(keep.len());
        for t in tools {
            if let Some(def) = keep.remove(&t.name) {
                out.push(def);
            }
        }
        out
    }
}
fn split_globs(patterns: &[String]) -> (HashSet<String>, Vec<String>) {
    let mut exact = HashSet::new();
    let mut prefix = Vec::new();
    for p in patterns {
        if let Some(stem) = p.strip_suffix('*') {
            prefix.push(stem.to_string());
        } else {
            exact.insert(p.clone());
        }
    }
    (exact, prefix)
}
/// Cheap tokenise: lowercase, split on non-alphanumerics, drop
/// stopwords, keep stems ≥3 chars. Returns a term-frequency map.
fn tokenize_tf(text: &str) -> HashMap<String, f32> {
    let mut out: HashMap<String, f32> = HashMap::new();
    for raw in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if raw.is_empty() {
            continue;
        }
        let lower = raw.to_lowercase();
        if lower.len() < 3 || STOPWORDS.contains(&lower.as_str()) {
            continue;
        }
        *out.entry(lower).or_insert(0.0) += 1.0;
    }
    out
}
/// Short stoplist for Spanish + English. Not exhaustive — covers the
/// common filler that dominates a user prompt without adding signal.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "that", "you", "your", "with", "this", "from", "what", "when", "where",
    "which", "how", "can", "could", "would", "should", "will", "has", "have", "are", "was", "were",
    "been", "being", "into", "onto", "about", "over", "under", "also", "just", "some", "any",
    "all", "out", "off", "per", "via", "not", "but", "una", "uno", "unas", "unos", "los", "las",
    "del", "las", "por", "para", "con", "sin", "sobre", "entre", "cuando", "donde", "como", "que",
    "qué", "cual", "cuál", "quien", "quién", "cuanto", "cuánto", "ese", "esa", "eso", "este",
    "esta", "esto", "aquel", "aquella", "aquello", "hay", "has", "haz", "son", "era", "fue", "ser",
    "soy", "eres", "esta", "está", "están", "estaba", "estaban", "estoy",
];
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn td(name: &str, desc: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: desc.into(),
            parameters: json!({}),
        }
    }
    fn cfg(k: usize) -> RelevanceConfig {
        RelevanceConfig {
            enabled: true,
            top_k: k,
            min_score: 0.01,
            always_include: vec!["delegate".into(), "memory_*".into()],
        }
    }
    #[test]
    fn tokenize_drops_stopwords_and_short() {
        let tf = tokenize_tf("the weather in Bogotá is sunny today");
        assert!(!tf.contains_key("the"));
        assert!(!tf.contains_key("in"));
        assert!(tf.contains_key("weather"));
        assert!(tf.contains_key("bogotá"));
        assert!(tf.contains_key("sunny"));
    }
    #[test]
    fn ranks_weather_higher_for_weather_prompt() {
        let tools = vec![
            td(
                "ext_weather_forecast",
                "Weather forecast (clima, pronóstico) for any city (ciudad)",
            ),
            td("ext_github_comment", "Post a comment to a github issue"),
            td("ext_spotify_play", "Play a song on Spotify"),
        ];
        let f = ToolFilter::build(cfg(2), &tools);
        let kept = f.filter("qué clima hace en Bogotá ciudad", &tools);
        assert!(kept.iter().any(|t| t.name == "ext_weather_forecast"));
    }
    #[test]
    fn verbatim_name_mention_is_preserved() {
        let tools = vec![
            td("ext_weather_forecast", "Weather for a city"),
            td("ext_github_comment", "Comment on github issues"),
            td("ext_spotify_play", "Play songs"),
        ];
        let f = ToolFilter::build(cfg(1), &tools);
        let kept = f.filter("use the ext_github_comment tool please", &tools);
        assert!(kept.iter().any(|t| t.name == "ext_github_comment"));
    }
    #[test]
    fn always_include_glob_never_drops() {
        let tools = vec![
            td("memory_append", "Append to long-term memory"),
            td("memory_recall", "Recall memories"),
            td("ext_weather_forecast", "Weather"),
            td("delegate", "Delegate a task to another agent"),
        ];
        let f = ToolFilter::build(cfg(1), &tools);
        let kept = f.filter("what's the weather", &tools);
        assert!(kept.iter().any(|t| t.name == "delegate"));
        assert!(kept.iter().any(|t| t.name == "memory_append"));
        assert!(kept.iter().any(|t| t.name == "memory_recall"));
        assert!(kept.iter().any(|t| t.name == "ext_weather_forecast"));
    }
    #[test]
    fn passthrough_when_under_cap() {
        let tools = vec![td("a", "x"), td("b", "y")];
        let f = ToolFilter::build(cfg(10), &tools);
        let kept = f.filter("anything", &tools);
        assert_eq!(kept.len(), 2);
    }
    #[test]
    fn disabled_returns_full_list() {
        let mut c = cfg(1);
        c.enabled = false;
        let tools = vec![td("a", "x"), td("b", "y"), td("c", "z")];
        let f = ToolFilter::build(c, &tools);
        assert_eq!(f.filter("anything", &tools).len(), 3);
    }
    #[test]
    fn strict_allow_list_via_top_k_zero() {
        // `top_k: 0` + `always_include` acts as a strict allow-list —
        // scoring is bypassed entirely. Used by sensitive agents that
        // must only see a curated tool set (no fuzzy bleed-through
        // from TF-cosine). Verbatim mentions in the prompt still pass.
        let tools = vec![
            td("ext_finance_wire", "Send a bank wire. Destructive."),
            td("ext_weather_forecast", "Weather forecast by city"),
            td("delegate", "Delegate to agent"),
            td("ext_github_comment", "Comment on github"),
        ];
        let c = RelevanceConfig {
            enabled: true,
            top_k: 0,
            min_score: 0.01,
            always_include: vec!["delegate".into()],
        };
        let f = ToolFilter::build(c, &tools);
        // With empty prompt → fallback to full list per current
        // semantics. That's fine — strict mode is about scoring,
        // not about overriding the empty-prompt escape hatch.
        // Non-empty prompt without verbatim mention: only always_include passes.
        let kept = f.filter("what is the weather like in Bogotá", &tools);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "delegate");
        // Verbatim mention still forces inclusion.
        let kept = f.filter("use ext_github_comment please", &tools);
        let names: Vec<_> = kept.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"delegate"));
        assert!(names.contains(&"ext_github_comment"));
        assert_eq!(kept.len(), 2);
    }
}
