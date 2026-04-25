//! Strongly-typed shape of the JSONL event stream Claude Code emits
//! when invoked with `--output-format stream-json`.
//!
//! `#[serde(other)]` on every enum makes the parser forward-compatible
//! — Anthropic adding a new event type doesn't break us; the caller
//! just sees `Other` and decides whether to log or ignore.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClaudeEvent {
    System(SystemEvent),
    Assistant(AssistantEvent),
    User(UserEvent),
    Result(ResultEvent),
    /// Forward-compat: any unknown `type` value lands here.
    #[serde(other)]
    Other,
}

impl ClaudeEvent {
    /// Best-effort accessor — the session id, if the event carries one.
    /// Useful for binding upserts without matching every variant.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            ClaudeEvent::System(SystemEvent::Init { session_id, .. }) => Some(session_id),
            ClaudeEvent::Assistant(e) => Some(e.session_id.as_str()),
            ClaudeEvent::User(e) => Some(e.session_id.as_str()),
            ClaudeEvent::Result(ResultEvent::Success { session_id, .. }) => Some(session_id),
            ClaudeEvent::Result(ResultEvent::ErrorMaxTurns { session_id, .. }) => Some(session_id),
            ClaudeEvent::Result(ResultEvent::ErrorDuringExecution { session_id, .. }) => {
                session_id.as_deref()
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum SystemEvent {
    Init {
        session_id: String,
        cwd: PathBuf,
        model: String,
        #[serde(default)]
        tools: Vec<String>,
        #[serde(default)]
        mcp_servers: Vec<McpServerInfo>,
        #[serde(default)]
        permission_mode: String,
        #[serde(default)]
        api_key_source: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AssistantEvent {
    pub session_id: String,
    pub message: AssistantMessage,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UserEvent {
    pub session_id: String,
    pub message: UserMessage,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AssistantMessage {
    pub id: String,
    pub model: String,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub usage: TokenUsage,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UserMessage {
    pub content: Vec<ContentBlock>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
        #[serde(default)]
        is_error: bool,
    },
    Thinking {
        thinking: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ResultEvent {
    Success {
        session_id: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        duration_ms: u64,
        #[serde(default)]
        duration_api_ms: Option<u64>,
        #[serde(default)]
        num_turns: u64,
        #[serde(default)]
        result: Option<String>,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        usage: TokenUsage,
    },
    ErrorMaxTurns {
        session_id: String,
        #[serde(default)]
        num_turns: u64,
    },
    ErrorDuringExecution {
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_top_type_lands_in_other() {
        let json = r#"{"type":"newfeature","data":{"hello":"world"}}"#;
        let ev: ClaudeEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(ev, ClaudeEvent::Other));
    }

    #[test]
    fn unknown_content_block_lands_in_other() {
        let json = r#"{"type":"future_block","value":42}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(block, ContentBlock::Other));
    }
}
