pub mod compactions;
pub mod concepts;
pub mod embedding;
pub mod long_term;
pub mod metrics;
pub mod relevance;
pub mod secret_config;
pub mod secret_scanner;
pub mod vector;

pub use compactions::{CompactionRow, CompactionStore};
pub use concepts::{classify_script, derive_concept_tags, ScriptFamily, MAX_CONCEPT_TAGS};
pub use embedding::{EmbeddingProvider, HttpEmbeddingProvider};
pub use long_term::{
    EmailFollowupEntry, EmailFollowupStatus, LongTermMemory, MemoryEntry, RecallSignals,
    ReminderEntry, StoredInteraction,
};
pub use relevance::{freshness_note, score_memories, MemoryType, ScoredMemory};
pub use secret_config::SecretGuardConfig;
pub use secret_scanner::{OnSecret, SecretBlockedError, SecretGuard, SecretMatch, SecretScanner};
