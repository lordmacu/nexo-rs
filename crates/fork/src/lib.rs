//! Phase 80.19 — Fork subagent infrastructure.
//!
//! Verbatim semantics from `claude-code-leak/src/utils/forkedAgent.ts`.
//! Decisions in `proyecto/design-kairos-port.md` D-8.
//!
//! # Cache-key invariant (CRITICAL)
//!
//! Per leak `forkedAgent.ts:522-525`:
//! > Do NOT `filterIncompleteToolCalls` here — drops the whole assistant
//! > on partial tool batches, orphaning the paired results (API 400).
//! > Dangling tool_uses are repaired downstream by
//! > `ensureToolResultPairing` in claude.ts, same as the main thread —
//! > identical post-repair prefix keeps the cache hit.
//!
//! Tests verify bit-for-bit message-prefix pass-through.

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

pub use auto_mem_filter::{tool_names, AutoMemFilter, AutoMemFilterError};
pub use cache_safe::{CacheSafeParams, CacheSafeSlot};
pub use delegate_mode::DelegateMode;
pub use error::ForkError;
pub use fork_handle::{ForkHandle, ForkResult};
pub use fork_subagent::{DefaultForkSubagent, ForkParams, ForkSubagent, QuerySource};
pub use on_message::{ChainCollector, LoggingCollector, NoopCollector, OnMessage};
pub use overrides::{create_fork_context, ForkOverrides};
pub use tool_filter::{AllowAllFilter, ToolFilter};
pub use turn_loop::{run_turn_loop, ToolDispatcher, TurnLoopParams, TurnLoopResult};
