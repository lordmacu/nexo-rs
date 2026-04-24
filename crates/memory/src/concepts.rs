// Concept vocabulary — derives stable, normalized concept tags from a (path,
// snippet) pair. Pure function; no IO, no async.
//
// Inspired by OpenClaw's `extensions/memory-core/src/concept-vocabulary.ts`.
// Phase 10.7 ships a Rust port of the normalization + stop-word + glossary
// pipeline. Dreaming promotes tags to memory rows; recall uses them for query
// expansion against FTS5.

use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;
use unicode_script::{Script, UnicodeScript};
use unicode_segmentation::UnicodeSegmentation;

pub const MAX_CONCEPT_TAGS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptFamily {
    Latin,
    Cjk,
    Mixed,
    Other,
}

// Shared tech noise — words that are ubiquitous across any project log/notes.
const SHARED_STOP: &[&str] = &[
    "about", "after", "agent", "again", "also", "because", "before", "being",
    "between", "build", "called", "could", "daily", "default", "deploy",
    "during", "every", "file", "files", "from", "have", "into", "just", "line",
    "lines", "long", "main", "make", "memory", "month", "more", "most", "move",
    "much", "next", "note", "notes", "over", "part", "past", "port", "same",
    "score", "search", "session", "sessions", "short", "should", "since",
    "some", "than", "that", "their", "there", "these", "they", "this",
    "through", "today", "using", "with", "work", "workspace", "year",
];

const ENGLISH_STOP: &[&str] = &["and", "are", "for", "into", "its", "our", "then", "were"];

const SPANISH_STOP: &[&str] = &[
    "al", "con", "como", "de", "del", "el", "en", "es", "la", "las", "los",
    "para", "por", "que", "se", "sin", "su", "sus", "una", "uno", "unos",
    "unas", "y",
];

const PATH_NOISE: &[&str] = &[
    "cjs", "cpp", "cts", "jsx", "json", "md", "mjs", "mts", "text", "toml",
    "ts", "tsx", "txt", "yaml", "yml",
];

// Protected glossary — tech terms we always keep even if short.
const GLOSSARY_RAW: &[&str] = &[
    "backup", "backups", "embedding", "embeddings", "failover", "gateway",
    "glacier", "gpt", "openai", "router", "network", "vlan", "s3", "kv", "qmd",
    // Spanish
    "configuración", "respaldo", "enrutador",
    // French
    "sauvegarde", "routeur", "passerelle",
    // German
    "konfiguration", "sicherung",
    // CJK
    "备份", "故障转移", "网络", "网关", "路由器",
    "バックアップ", "フェイルオーバー", "ルーター", "ネットワーク", "ゲートウェイ",
    "라우터", "백업", "페일오버", "네트워크", "게이트웨이",
];

static STOP_WORDS: LazyLock<HashSet<String>> = LazyLock::new(|| {
    let mut set = HashSet::new();
    for w in SHARED_STOP.iter().chain(ENGLISH_STOP).chain(SPANISH_STOP).chain(PATH_NOISE) {
        set.insert(nfkc_lower(w));
    }
    set
});

static PROTECTED_GLOSSARY: LazyLock<Vec<String>> =
    LazyLock::new(|| GLOSSARY_RAW.iter().map(|g| nfkc_lower(g)).collect());

static COMPOUND_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+(?:[._/\-][\p{L}\p{N}]+)+").unwrap());

static ISO_DATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}(?:\.[\p{L}\p{N}]+)?$").unwrap());

static PURE_DIGITS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+$").unwrap());

static EDGE_NON_WORD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^\p{L}\p{N}]+|[^\p{L}\p{N}]+$").unwrap());

fn nfkc_lower(s: &str) -> String {
    s.nfkc().collect::<String>().to_lowercase()
}

fn contains_letter_or_number(s: &str) -> bool {
    s.chars().any(|c| c.is_alphabetic() || c.is_numeric())
}

pub fn classify_script(tag: &str) -> ScriptFamily {
    let normalized: String = tag.nfkc().collect();
    let mut has_latin = false;
    let mut has_cjk = false;
    for ch in normalized.chars() {
        match ch.script() {
            Script::Latin => has_latin = true,
            Script::Han | Script::Hiragana | Script::Katakana | Script::Hangul => has_cjk = true,
            _ => {}
        }
    }
    match (has_latin, has_cjk) {
        (true, true) => ScriptFamily::Mixed,
        (false, true) => ScriptFamily::Cjk,
        (true, false) => ScriptFamily::Latin,
        (false, false) => ScriptFamily::Other,
    }
}

fn min_len_for(script: ScriptFamily) -> usize {
    match script {
        ScriptFamily::Cjk => 2,
        _ => 3,
    }
}

fn is_kana_only(s: &str) -> bool {
    let mut has_kana = false;
    for ch in s.chars() {
        match ch.script() {
            Script::Han | Script::Hangul => return false,
            Script::Hiragana | Script::Katakana => has_kana = true,
            _ => {}
        }
    }
    has_kana
}

pub(crate) fn normalize_token(raw: &str) -> Option<String> {
    let nfkc: String = raw.nfkc().collect();
    let trimmed = EDGE_NON_WORD_RE.replace_all(&nfkc, "");
    let swapped = trimmed.replace('_', "-");
    let normalized = swapped.to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if !contains_letter_or_number(&normalized) {
        return None;
    }
    if normalized.len() > 32 {
        return None;
    }
    if PURE_DIGITS_RE.is_match(&normalized) {
        return None;
    }
    if ISO_DATE_RE.is_match(&normalized) {
        return None;
    }
    let script = classify_script(&normalized);
    // Char count, not byte len — CJK chars are multi-byte.
    let char_count = normalized.chars().count();
    if char_count < min_len_for(script) {
        return None;
    }
    if is_kana_only(&normalized) && char_count < 3 {
        return None;
    }
    if STOP_WORDS.contains(&normalized) {
        return None;
    }
    Some(normalized)
}

fn collect_glossary_matches(source: &str) -> Vec<String> {
    let normalized_source = nfkc_lower(source);
    let mut out = Vec::new();
    for entry in PROTECTED_GLOSSARY.iter() {
        if normalized_source.contains(entry) {
            out.push(entry.clone());
        }
    }
    out
}

fn collect_compound_tokens(source: &str) -> Vec<String> {
    COMPOUND_TOKEN_RE
        .find_iter(source)
        .map(|m| m.as_str().to_string())
        .collect()
}

fn collect_segment_tokens(source: &str) -> Vec<String> {
    source.unicode_words().map(|w| w.to_string()).collect()
}

fn push_normalized(tags: &mut Vec<String>, raw: &str, limit: usize) -> bool {
    match normalize_token(raw) {
        Some(norm) if !tags.contains(&norm) => {
            tags.push(norm);
            tags.len() < limit
        }
        _ => true,
    }
}

/// Derive up to `limit` normalized concept tags from `path` (basename) and
/// `snippet`. Deterministic; returns `[]` when `limit == 0`.
pub fn derive_concept_tags(path: &str, snippet: &str, limit: usize) -> Vec<String> {
    let limit = limit.min(MAX_CONCEPT_TAGS);
    if limit == 0 {
        return Vec::new();
    }

    let basename = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let source = format!("{} {}", basename, snippet);

    let mut tags: Vec<String> = Vec::with_capacity(limit);
    let candidates = collect_glossary_matches(&source)
        .into_iter()
        .chain(collect_compound_tokens(&source))
        .chain(collect_segment_tokens(&source));
    for raw in candidates {
        if !push_normalized(&mut tags, &raw, limit) {
            break;
        }
    }
    tags.truncate(limit);
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rejects_pure_digits() {
        assert!(normalize_token("12345").is_none());
    }

    #[test]
    fn normalize_rejects_iso_date() {
        assert!(normalize_token("2026-04-22").is_none());
        assert!(normalize_token("2026-04-22.md").is_none());
    }

    #[test]
    fn normalize_rejects_stop_word() {
        assert!(normalize_token("memory").is_none());
        assert!(normalize_token("session").is_none());
        assert!(normalize_token("para").is_none());
    }

    #[test]
    fn normalize_rejects_too_short_latin() {
        assert!(normalize_token("ab").is_none());
        assert!(normalize_token("Ai").is_none());
    }

    #[test]
    fn normalize_accepts_cjk_two_chars() {
        let result = normalize_token("备份");
        assert_eq!(result.as_deref(), Some("备份"));
    }

    #[test]
    fn normalize_rejects_too_long() {
        let s = "a".repeat(33);
        assert!(normalize_token(&s).is_none());
    }

    #[test]
    fn normalize_underscore_becomes_dash() {
        assert_eq!(normalize_token("foo_bar").as_deref(), Some("foo-bar"));
    }

    #[test]
    fn script_latin() {
        assert_eq!(classify_script("hello"), ScriptFamily::Latin);
    }

    #[test]
    fn script_cjk() {
        assert_eq!(classify_script("配置"), ScriptFamily::Cjk);
    }

    #[test]
    fn script_mixed() {
        assert_eq!(classify_script("openai配置"), ScriptFamily::Mixed);
    }

    #[test]
    fn script_other() {
        assert_eq!(classify_script("---"), ScriptFamily::Other);
    }

    #[test]
    fn compound_tokens_keep_whole_identifier() {
        let toks = collect_compound_tokens("edit src/main.rs and config.yaml files");
        assert!(toks.iter().any(|t| t == "src/main.rs"));
        assert!(toks.iter().any(|t| t == "config.yaml"));
    }

    #[test]
    fn glossary_matches_found() {
        let hits = collect_glossary_matches("We call OpenAI via GPT for backups");
        assert!(hits.iter().any(|h| h == "openai"));
        assert!(hits.iter().any(|h| h == "gpt"));
        assert!(hits.iter().any(|h| h == "backup") || hits.iter().any(|h| h == "backups"));
    }

    #[test]
    fn derive_limit_zero() {
        assert!(derive_concept_tags("", "anything goes here", 0).is_empty());
    }

    #[test]
    fn derive_respects_max() {
        let snippet = "kubernetes deployment cluster rollout canary metrics dashboard pipeline alerts latency throughput errors";
        let tags = derive_concept_tags("notes.md", snippet, MAX_CONCEPT_TAGS);
        assert!(tags.len() <= MAX_CONCEPT_TAGS);
    }

    #[test]
    fn derive_picks_glossary_term() {
        let tags = derive_concept_tags("ops/notes.md", "OpenAI quota monitoring endpoint", 8);
        assert!(tags.iter().any(|t| t == "openai"), "expected 'openai' in tags: {:?}", tags);
    }

    #[test]
    fn derive_dedups() {
        let tags = derive_concept_tags("", "router router router network network", 8);
        let router_count = tags.iter().filter(|t| t.as_str() == "router").count();
        assert!(router_count <= 1);
    }

    #[test]
    fn derive_with_empty_path() {
        let tags = derive_concept_tags("", "embedding dimension tuning", 8);
        assert!(!tags.is_empty());
    }
}
