use serde::Deserialize;

fn default_max_sessions() -> u32 {
    3
}

fn default_timeout_secs() -> u32 {
    30
}

fn default_max_output_bytes() -> u64 {
    65536
}

fn default_allowed_runtimes() -> Vec<String> {
    vec!["python".into(), "node".into(), "bash".into()]
}

/// Phase 79.12 — per-agent / per-binding REPL tool configuration.
///
/// When `enabled: true` the agent registers a `Repl` tool that can spawn
/// persistent Python, Node.js, or bash subprocesses, execute code, and
/// read output across turns. Feature-gated behind `repl-tool`.
///
/// YAML example (agent-level default):
/// ```yaml
/// agents:
///   - id: ana
///     repl:
///       enabled: true
///       allowed_runtimes: ["python", "node"]
///       max_sessions: 3
///       timeout_secs: 30
///       max_output_bytes: 65536
/// ```
///
/// Per-binding override (replaces the whole struct when present):
/// ```yaml
/// inbound_bindings:
///   - plugin: whatsapp
///     repl:
///       enabled: true
///       allowed_runtimes: ["python"]
/// ```
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReplConfig {
    /// Enable the Repl tool. Off by default — arbitrary code execution.
    #[serde(default)]
    pub enabled: bool,

    /// Runtimes the agent is allowed to spawn.
    #[serde(default = "default_allowed_runtimes")]
    pub allowed_runtimes: Vec<String>,

    /// Maximum concurrent REPL sessions per agent.
    #[serde(default = "default_max_sessions")]
    pub max_sessions: u32,

    /// Seconds before an exec action returns `timed_out: true`.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u32,

    /// Maximum bytes stored in the output buffer per session.
    /// Oldest bytes are dropped when the cap is reached.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: u64,
}

impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_runtimes: default_allowed_runtimes(),
            max_sessions: default_max_sessions(),
            timeout_secs: default_timeout_secs(),
            max_output_bytes: default_max_output_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_phase_79_12() {
        let c = ReplConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.allowed_runtimes, vec!["python", "node", "bash"]);
        assert_eq!(c.max_sessions, 3);
        assert_eq!(c.timeout_secs, 30);
        assert_eq!(c.max_output_bytes, 65536);
    }
}
