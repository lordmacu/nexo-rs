use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    pub short_term: ShortTermConfig,
    pub long_term: LongTermConfig,
    #[serde(default)]
    pub vector: VectorConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShortTermConfig {
    #[serde(default = "default_max_turns")]
    pub max_history_turns: usize,
    #[serde(default = "default_session_ttl")]
    pub session_ttl: String,
    /// Soft cap on concurrent live sessions. When the cap is reached
    /// the oldest-idle session is evicted on insert. Set to `0` to
    /// disable the cap (unbounded). Protects against spam-driven DoS
    /// where an attacker rotates `chat_id`s to grow the session map.
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
}

fn default_max_turns() -> usize {
    50
}
fn default_session_ttl() -> String {
    "24h".to_string()
}
fn default_max_sessions() -> usize {
    10_000
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LongTermConfig {
    #[serde(default = "default_backend")]
    pub backend: String,
    pub sqlite: Option<SqliteConfig>,
    pub redis: Option<RedisConfig>,
}

fn default_backend() -> String {
    "sqlite".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqliteConfig {
    #[serde(default = "default_sqlite_path")]
    pub path: String,
}

fn default_sqlite_path() -> String {
    "./data/memory.db".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisConfig {
    pub url: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct VectorConfig {
    /// Phase 5.4 — opt-in. Absent or false means no vector index.
    #[serde(default = "default_vector_enabled")]
    pub enabled: bool,
    #[serde(default = "default_vector_backend")]
    pub backend: String,
    /// Default recall mode used by the `memory` tool when callers omit
    /// `mode`. Supported: `keyword` (default), `vector`, `hybrid`.
    #[serde(default = "default_recall_mode")]
    pub default_recall_mode: String,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
}

fn default_vector_enabled() -> bool {
    false
}
fn default_vector_backend() -> String {
    "sqlite-vec".to_string()
}
fn default_recall_mode() -> String {
    "keyword".to_string()
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    /// "http" is the only provider shipped in 5.4. Local backends are
    /// follow-ups.
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_dimensions")]
    pub dimensions: usize,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_provider() -> String {
    "http".to_string()
}
fn default_dimensions() -> usize {
    1536
}
fn default_timeout_secs() -> u64 {
    30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_default_recall_mode_defaults_to_keyword() {
        let yaml = r#"
short_term:
  max_history_turns: 50
  session_ttl: "24h"
long_term:
  backend: "sqlite"
  sqlite:
    path: "./data/memory.db"
vector:
  enabled: false
  backend: "sqlite-vec"
"#;
        let cfg: MemoryConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.vector.default_recall_mode, "keyword");
    }

    #[test]
    fn vector_default_recall_mode_parses_when_set() {
        let yaml = r#"
short_term:
  max_history_turns: 50
  session_ttl: "24h"
long_term:
  backend: "sqlite"
  sqlite:
    path: "./data/memory.db"
vector:
  enabled: true
  backend: "sqlite-vec"
  default_recall_mode: "hybrid"
  embedding:
    provider: "http"
    base_url: "http://localhost:11434/v1"
    model: "nomic-embed-text"
    dimensions: 768
    timeout_secs: 30
"#;
        let cfg: MemoryConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.vector.default_recall_mode, "hybrid");
    }
}
