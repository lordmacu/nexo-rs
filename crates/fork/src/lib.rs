//! Fork subagent infrastructure.
//!
//! # Cache-key invariant (CRITICAL)
//!
//! Do NOT filter incomplete tool calls here — that drops the whole
//! assistant message on partial tool batches, orphaning the paired
//! results (API 400). Dangling tool_uses are repaired downstream when
//! pairing tool results in transport, same as the main thread — an
//! identical post-repair prefix keeps the cache hit.
//!
//! Tests verify bit-for-bit message-prefix pass-through.

pub mod agent_dispatcher;
pub mod auto_mem_filter;
pub mod cache_safe;
pub mod delegate_mode;
pub mod error;
pub mod fork_handle;
pub mod fork_subagent;
pub mod on_message;
pub mod overrides;
pub mod tool_filter;
pub mod turn_loop;

pub use agent_dispatcher::AgentToolDispatcher;
pub use auto_mem_filter::{tool_names, AutoMemFilter, AutoMemFilterError};
pub use cache_safe::{CacheSafeParams, CacheSafeSlot};
pub use delegate_mode::DelegateMode;
pub use error::ForkError;
pub use fork_handle::{fork_error_to_task_notification, ForkHandle, ForkResult};
pub use fork_subagent::{DefaultForkSubagent, ForkParams, ForkSubagent, QuerySource};
pub use on_message::{ChainCollector, LoggingCollector, NoopCollector, OnMessage};
pub use overrides::{create_fork_context, ForkOverrides};
pub use tool_filter::{AllowAllFilter, ToolFilter};
pub use turn_loop::{run_turn_loop, ToolDispatcher, TurnLoopParams, TurnLoopResult};
