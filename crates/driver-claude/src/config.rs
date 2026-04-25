//! Operator-visible config struct. The real loader lives in 67.4 —
//! here we only define the deserialisable shape so a YAML round-trip
//! can be tested in isolation.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct ClaudeConfig {
    /// `which("claude")` is consulted at runtime when this is `None`.
    #[serde(default)]
    pub binary: Option<PathBuf>,
    #[serde(default)]
    pub default_args: ClaudeDefaultArgs,
    /// Path to MCP config JSON; passed to `claude --mcp-config`.
    #[serde(default)]
    pub mcp_config: Option<PathBuf>,
    /// Grace before SIGKILL after SIGTERM.
    #[serde(default = "default_forced_kill", with = "humantime_serde")]
    pub forced_kill_after: Duration,
    /// Per-turn wall-clock cap.
    #[serde(default = "default_turn_timeout", with = "humantime_serde")]
    pub turn_timeout: Duration,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ClaudeDefaultArgs {
    #[serde(default)]
    pub output_format: OutputFormat,
    #[serde(default)]
    pub permission_prompt_tool: Option<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    StreamJson,
    Json,
    Text,
}

impl OutputFormat {
    pub fn as_cli(self) -> &'static str {
        match self {
            Self::StreamJson => "stream-json",
            Self::Json => "json",
            Self::Text => "text",
        }
    }
}

fn default_forced_kill() -> Duration {
    Duration::from_secs(1)
}
fn default_turn_timeout() -> Duration {
    Duration::from_secs(600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_yaml() {
        let yaml = r#"
binary: claude
default_args:
  output_format: stream_json
  permission_prompt_tool: mcp__nexo-driver__permission_prompt
  allowed_tools: ["Read", "Grep"]
forced_kill_after: 1s
turn_timeout: 10m
"#;
        let cfg: ClaudeConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.binary, Some(PathBuf::from("claude")));
        assert_eq!(cfg.default_args.output_format, OutputFormat::StreamJson);
        assert_eq!(
            cfg.default_args.permission_prompt_tool.as_deref(),
            Some("mcp__nexo-driver__permission_prompt")
        );
        assert_eq!(cfg.default_args.allowed_tools, vec!["Read", "Grep"]);
        assert_eq!(cfg.forced_kill_after, Duration::from_secs(1));
        assert_eq!(cfg.turn_timeout, Duration::from_secs(600));
    }
}
