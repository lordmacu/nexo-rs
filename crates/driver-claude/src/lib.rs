//! Phase 67.1 — `nexo-driver-claude`.
//!
//! Spawn the `claude` CLI, consume its stream-json events, and keep
//! session bindings so the next turn can `--resume <id>`. See
//! `README.md` for the layering rationale and a quick example.

pub mod binding;
#[cfg(feature = "sqlite")]
pub mod binding_sqlite;
pub mod child;
pub mod command;
pub mod config;
pub mod error;
pub mod event;
pub mod stream;
pub mod turn;

pub use binding::{MemoryBindingStore, SessionBinding, SessionBindingStore};
#[cfg(feature = "sqlite")]
pub use binding_sqlite::SqliteBindingStore;
pub use child::ChildHandle;
pub use command::ClaudeCommand;
pub use config::{ClaudeConfig, ClaudeDefaultArgs, OutputFormat};
pub use error::ClaudeError;
pub use event::{
    AssistantEvent, AssistantMessage, ClaudeEvent, ContentBlock, McpServerInfo, ResultEvent,
    SystemEvent, TokenUsage, UserEvent, UserMessage,
};
pub use stream::EventStream;
pub use turn::{spawn_turn, TurnHandle};
