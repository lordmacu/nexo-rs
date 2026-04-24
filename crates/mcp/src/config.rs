//! Launch config for a stdio MCP server.
//!
//! The YAML loader (12.3/12.4) is responsible for resolving `${ENV_VAR}`
//! placeholders before constructing this struct. 12.1 treats every field
//! as fully resolved.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct McpServerConfig {
    /// Logical name — also the circuit-breaker key (`mcp:{name}`).
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub connect_timeout: Duration,
    pub initialize_timeout: Duration,
    pub call_timeout: Duration,
    pub shutdown_grace: Duration,
    /// Phase 12.8 — when Some and the server advertises the `logging`
    /// capability, the client sends `logging/setLevel` once after
    /// initialize. Unknown levels or missing capability are logged at
    /// `warn` and skipped — connect does not fail.
    pub log_level: Option<String>,
    /// Phase 12.8 — per-server override for `mcp.context.passthrough`.
    /// `None` defers to the global flag; `Some(bool)` forces the value
    /// regardless of the global setting.
    pub context_passthrough: Option<bool>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            connect_timeout: Duration::from_secs(30),
            initialize_timeout: Duration::from_secs(10),
            call_timeout: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(3),
            log_level: None,
            context_passthrough: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timeouts_are_sane() {
        let c = McpServerConfig::default();
        assert_eq!(c.connect_timeout, Duration::from_secs(30));
        assert_eq!(c.initialize_timeout, Duration::from_secs(10));
        assert_eq!(c.call_timeout, Duration::from_secs(30));
        assert_eq!(c.shutdown_grace, Duration::from_secs(3));
        assert!(c.env.is_empty());
    }
}
