use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    pub short_term: ShortTermConfig,
    pub long_term: LongTermConfig,
    #[serde(default)]
    pub vector: VectorConfig,
    /// C5 — Secret-scanner policy (provider-agnostic). When the YAML
    /// key is omitted, the secure default applies (`enabled: true,
    /// on_secret: block, rules: all, exclude_rules: []`).
    ///
    /// **Wire-shape duplication note**: the canonical types
    /// (`nexo_memory::SecretGuardConfig`, `OnSecret`, `RuleSelection`)
    /// live in `nexo-memory`. Because `nexo-memory` depends on
    /// `nexo-llm` which depends on `nexo-config`, a direct
    /// `nexo-config -> nexo-memory` dep would form a cycle. The
    /// wire-shape struct below mirrors the schema 1:1 and is
    /// converted to the domain type via a `From` impl that lives
    /// in `src/main.rs` (which holds both deps). When updating the
    /// schema, change BOTH this struct AND `secret_config.rs`.
    #[serde(default)]
    pub secret_guard: SecretGuardYamlConfig,
    /// Phase 36.2 — memory snapshot subsystem. When omitted, defaults
    /// match `nexo_memory_snapshot::MemorySnapshotConfig::default()`:
    /// enabled, retention 30/90d/1h, encryption off, no auto-pre-dream,
    /// auto-pre-restore on. Wire-shape duplicated to keep
    /// `nexo-memory-snapshot` out of the `nexo-config` dep graph.
    #[serde(default)]
    pub snapshot: SnapshotYamlConfig,
}

/// Wire-shape clone of `nexo_memory::SecretGuardConfig`. See doc on
/// [`MemoryConfig::secret_guard`] for the cycle-break rationale.
///
/// Provider-agnostic — the scanner detects API keys for every
/// supported LLM provider (Anthropic, MiniMax, OpenAI, Gemini,
/// DeepSeek, xAI, Mistral) using the same regex set; `exclude_rules`
/// operates on rule IDs (kebab-case like `github-pat`,
/// `aws-access-token`, `openai-api-key`), not on providers.
///
/// Prior art (validated, not copied):
///   * `claude-code-leak/src/services/teamMemorySync/secretScanner.ts:48,596-615,312-324`
///     — hardcoded scanner with no YAML knob; activation via build
///     flag (`feature('TEAMMEM')`) only. We adopt a richer
///     operator-facing config rather than the hardcoded model.
///   * `research/src/config/zod-schema.ts` — OpenClaw uses 2-value
///     enums (`redactSensitive: off|tools`, `mode: enforce|warn`).
///     We extend to 3 (`block|redact|warn`) for richer behaviour.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecretGuardYamlConfig {
    /// Master switch. When `false`, every check is a no-op.
    pub enabled: bool,
    /// Policy for handling detected secrets: `block` | `redact` | `warn`.
    /// Wire as a string here; main.rs converts to
    /// `nexo_memory::secret_scanner::OnSecret` and validates.
    pub on_secret: String,
    /// Rule selection. Either the string `"all"` or a YAML list of
    /// rule IDs (kebab-case strings). `serde_yaml::Value` lets us
    /// accept both shapes; main.rs branches on the variant.
    pub rules: serde_yaml::Value,
    /// Rule IDs to skip (false positives). kebab-case, e.g.
    /// `["github-pat", "openai-api-key"]`.
    pub exclude_rules: Vec<String>,
}

impl Default for SecretGuardYamlConfig {
    fn default() -> Self {
        // Mirrors the secure default of
        // `nexo_memory::SecretGuardConfig::default()`.
        Self {
            enabled: true,
            on_secret: "block".into(),
            rules: serde_yaml::Value::String("all".into()),
            exclude_rules: Vec::new(),
        }
    }
}

/// Wire-shape mirror of `nexo_memory_snapshot::MemorySnapshotConfig`.
///
/// **Schema-duplication note**: when adding/renaming/removing a field
/// here, mirror the change in `crates/memory-snapshot/src/config.rs`
/// AND in the converter `src/main.rs::build_snapshot_config_from_yaml`
/// — both are exercised by `crates/config/tests/load_test.rs`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SnapshotYamlConfig {
    pub enabled: bool,
    /// On-disk root for the per-tenant snapshot tree. When the path
    /// is empty (default), `src/main.rs` falls back to
    /// `nexo_state_dir()`.
    pub root: String,
    pub auto_pre_dream: bool,
    pub auto_pre_restore: bool,
    pub auto_pre_mutating_tool: bool,
    pub lock_timeout_secs: u64,
    pub redact_secrets_default: bool,
    pub encryption: SnapshotEncryptionYamlConfig,
    pub retention: SnapshotRetentionYamlConfig,
    pub events: SnapshotEventsYamlConfig,
    /// Optional override of the per-agent memdir root. When unset,
    /// the snapshotter falls back to `root` (single-tenant ops) or
    /// the per-agent `workspace_git` root (multi-tenant SaaS, future).
    #[serde(default)]
    pub memdir_root: String,
    /// Optional override of the per-agent SQLite root. Same fallback
    /// as `memdir_root`.
    #[serde(default)]
    pub sqlite_root: String,
}

impl Default for SnapshotYamlConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            root: String::new(),
            auto_pre_dream: false,
            auto_pre_restore: true,
            auto_pre_mutating_tool: false,
            lock_timeout_secs: 60,
            redact_secrets_default: true,
            encryption: SnapshotEncryptionYamlConfig::default(),
            retention: SnapshotRetentionYamlConfig::default(),
            events: SnapshotEventsYamlConfig::default(),
            memdir_root: String::new(),
            sqlite_root: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SnapshotEncryptionYamlConfig {
    pub enabled: bool,
    pub recipients: Vec<String>,
    pub identity_path: String,
}

impl Default for SnapshotEncryptionYamlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            recipients: Vec::new(),
            identity_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SnapshotRetentionYamlConfig {
    pub keep_count: u32,
    pub max_age_days: u32,
    pub gc_interval_secs: u64,
}

impl Default for SnapshotRetentionYamlConfig {
    fn default() -> Self {
        Self {
            keep_count: 30,
            max_age_days: 90,
            gc_interval_secs: 3600,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SnapshotEventsYamlConfig {
    pub mutation_subject_prefix: String,
    pub lifecycle_subject_prefix: String,
    pub mutation_publish_enabled: bool,
}

impl Default for SnapshotEventsYamlConfig {
    fn default() -> Self {
        Self {
            mutation_subject_prefix: "nexo.memory.mutated".into(),
            lifecycle_subject_prefix: "nexo.memory.snapshot".into(),
            mutation_publish_enabled: true,
        }
    }
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
