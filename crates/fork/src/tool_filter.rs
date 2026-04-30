//! `ToolFilter` trait — per-fork tool whitelist policy.
//! Step 80.19 / 5.
//!
//! Implementations consumed by [`crate::run_turn_loop`]:
//! - [`AllowAllFilter`] — default, accepts every tool. Right for sync
//!   delegation tools that already enforce the parent's binding-level
//!   `allowed_tools` upstream.
//! - `AutoMemFilter` — Phase 80.20, restricts the autoDream forked
//!   subagent to read-only filesystem + Edit/Write inside the memory
//!   directory. Mirror of `createAutoMemCanUseTool` (leak
//!   `services/extractMemories/extractMemories.ts:171-222`).
//! - Other filters (away_summary, eval) land in their own sub-phases.

use serde_json::Value;
use std::fmt::Debug;

/// A per-fork policy that gates tool invocations BEFORE dispatch.
///
/// The fork's turn loop calls [`allows`](Self::allows) for each
/// `ToolCall` the model emits; rejected calls produce a synthetic
/// `tool_result` whose body is [`denial_message`](Self::denial_message)
/// so the model can recover within the same turn.
pub trait ToolFilter: Send + Sync + Debug {
    /// Return `true` if the fork is allowed to dispatch this tool with
    /// these arguments. Implementations may inspect `args` to make
    /// argument-aware decisions (e.g. "only allow `FileEdit` when
    /// `file_path` is inside the memory directory").
    fn allows(&self, tool_name: &str, args: &Value) -> bool;

    /// Human-readable denial message inserted as the synthetic
    /// `tool_result` body when [`allows`](Self::allows) returned `false`.
    /// Should explain the constraint so the model can recover.
    fn denial_message(&self, tool_name: &str) -> String;
}

/// Default filter — allows everything. Right when the parent's
/// binding-level capability gate already filters the tool catalog
/// upstream.
#[derive(Debug, Default, Clone, Copy)]
pub struct AllowAllFilter;

impl ToolFilter for AllowAllFilter {
    fn allows(&self, _name: &str, _args: &Value) -> bool {
        true
    }

    fn denial_message(&self, _name: &str) -> String {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn allow_all_accepts_every_tool() {
        let f = AllowAllFilter;
        assert!(f.allows("Bash", &json!({"command": "rm -rf /"})));
        assert!(f.allows("FileEdit", &json!({"file_path": "/etc/passwd"})));
        assert_eq!(f.denial_message("Bash"), "");
    }
}
