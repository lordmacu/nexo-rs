//! Cheap support-check types so the harness selector can decide
//! without spawning a subprocess.

use serde::{Deserialize, Serialize};

/// Context passed to `AgentHarness::supports` — enough info for a
/// harness to say "yes, I drive this provider/runtime" without
/// touching state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportContext {
    /// Provider id of the underlying model — `"anthropic"`,
    /// `"minimax"`, `"claude-code"` (when the CLI is the provider).
    pub provider: String,
    pub model_id: Option<String>,
    pub runtime: HarnessRuntime,
}

/// Where the harness drives the agent. `Local` means the harness runs
/// the model in-process; `Subprocess` is the Claude Code CLI case.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessRuntime {
    Local,
    Subprocess,
    Http,
    Ws,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Support {
    Supported {
        /// Higher wins when several harnesses claim the same provider.
        priority: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Unsupported {
        reason: String,
    },
}
