use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    pub short_term: ShortTermConfig,
    pub long_term: LongTermConfig,
    pub vector: VectorConfig,
}

#[derive(Debug, Deserialize)]
pub struct ShortTermConfig {
    #[serde(default = "default_max_turns")]
    pub max_history_turns: usize,
    #[serde(default = "default_session_ttl")]
    pub session_ttl: String,
}

fn default_max_turns() -> usize { 50 }
fn default_session_ttl() -> String { "24h".to_string() }

#[derive(Debug, Deserialize)]
pub struct LongTermConfig {
    #[serde(default = "default_backend")]
    pub backend: String,
    pub sqlite: Option<SqliteConfig>,
    pub redis: Option<RedisConfig>,
}

fn default_backend() -> String { "sqlite".to_string() }

#[derive(Debug, Deserialize)]
pub struct SqliteConfig {
    #[serde(default = "default_sqlite_path")]
    pub path: String,
}

fn default_sqlite_path() -> String { "./data/memory.db".to_string() }

#[derive(Debug, Deserialize)]
pub struct RedisConfig {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct VectorConfig {
    #[serde(default = "default_vector_backend")]
    pub backend: String,
    pub embedding: EmbeddingConfig,
}

fn default_vector_backend() -> String { "sqlite-vec".to_string() }

#[derive(Debug, Deserialize)]
pub struct EmbeddingConfig {
    pub provider: String,
    pub model: String,
    #[serde(default = "default_dimensions")]
    pub dimensions: usize,
}

fn default_dimensions() -> usize { 1536 }
