use serde::{Deserialize, Serialize};

use crate::prompt_block::PromptBlock;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    /// Present when role = Tool — matches the ToolCall.id that triggered this result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Present when role = Tool — the tool name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Present when role = Assistant and the turn was a tool-call
    /// request. Preserves `{id, name, input}` across history so the
    /// Anthropic-Messages wire can re-emit `tool_use` blocks the
    /// follow-up `tool_result` messages correlate against.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Optional multi-modal attachments (images, eventually audio/video).
    /// Providers that understand vision render these alongside `content`;
    /// text-only providers ignore them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

/// One multi-modal attachment carried by a `ChatMessage`. For images the
/// caller provides one of three sources; providers emit the right wire
/// format (base64 inlining for Anthropic / Gemini / most models).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// `image`, `audio`, `video`, `document`. Providers decide what they can render.
    pub kind: String,
    /// MIME type (`image/jpeg`, `image/png`, `audio/ogg`, …). Required.
    pub mime_type: String,
    #[serde(flatten)]
    pub data: AttachmentData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentData {
    /// Already-encoded base64 payload, ready for inline wire embedding.
    Base64 { base64: String },
    /// Public URL. Anthropic and Gemini can fetch directly.
    Url { url: String },
    /// Local path. Caller must read + base64-encode before calling the
    /// client, or rely on the client helper `resolve_attachments()`.
    Path { path: String },
}

impl Attachment {
    pub fn image_path(mime_type: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            kind: "image".into(),
            mime_type: mime_type.into(),
            data: AttachmentData::Path { path: path.into() },
        }
    }
    pub fn image_url(mime_type: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            kind: "image".into(),
            mime_type: mime_type.into(),
            data: AttachmentData::Url { url: url.into() },
        }
    }
    pub fn image_base64(mime_type: impl Into<String>, base64: impl Into<String>) -> Self {
        Self {
            kind: "image".into(),
            mime_type: mime_type.into(),
            data: AttachmentData::Base64 {
                base64: base64.into(),
            },
        }
    }

    /// Load a `Path` attachment from disk and convert it in-place to `Base64`
    /// so it can ride on JSON wires. URL and already-base64 attachments pass
    /// through untouched.
    pub fn materialize(&mut self) -> anyhow::Result<()> {
        if let AttachmentData::Path { path } = &self.data {
            use base64::Engine;
            let bytes =
                std::fs::read(path).map_err(|e| anyhow::anyhow!("read attachment {path}: {e}"))?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            self.data = AttachmentData::Base64 { base64: encoded };
        }
        Ok(())
    }
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }
    }

    pub fn tool_result(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: ChatRole::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }
    }

    /// Assistant turn whose response was a tool-call request.
    /// Preserves `{id, name, input}` so the Anthropic-Messages wire
    /// can re-emit the `tool_use` blocks that the subsequent
    /// `tool_result` messages correlate against.
    pub fn assistant_tool_calls(calls: Vec<ToolCall>, text: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: text.into(),
            tool_call_id: None,
            name: None,
            tool_calls: calls,
            attachments: Vec::new(),
        }
    }
}

/// How the model should decide whether / which tool to call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolChoice {
    /// Model decides freely (no constraint). Equivalent to omitting the field.
    #[default]
    Auto,
    /// Force the model to pick exactly one of the declared tools.
    Any,
    /// Disable tool calling for this turn — model must answer with text.
    None,
    /// Force the model to call this specific tool.
    Specific(String),
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDef>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub system_prompt: Option<String>,
    /// Optional list of stop sequences. Providers that support stop
    /// sequences pass them through; others log a warn and ignore.
    pub stop_sequences: Vec<String>,
    /// Constraint on which tool (if any) the model should call. Providers
    /// that don't map this cleanly default to `Auto`.
    pub tool_choice: ToolChoice,
    /// Optional structured system prompt with explicit cache breakpoints.
    /// When non-empty, providers that support prompt caching materialize
    /// each block with its `CachePolicy`. When empty, providers fall back
    /// to the legacy flat `system_prompt: Option<String>`. Both fields can
    /// be set simultaneously — providers join `system_prompt` after the
    /// blocks (uncached) for back-compat with callers that mix the two.
    pub system_blocks: Vec<PromptBlock>,
    /// When true and `tools` is non-empty, providers that support prompt
    /// caching apply `cache_control` (long TTL) to the tool catalog. The
    /// `system_blocks` path turns this on automatically; raw callers can
    /// flip it explicitly.
    pub cache_tools: bool,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: vec![],
            max_tokens: 4096,
            temperature: 0.7,
            system_prompt: None,
            tool_choice: ToolChoice::Auto,
            stop_sequences: Vec::new(),
            system_blocks: Vec::new(),
            cache_tools: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: ResponseContent,
    pub usage: TokenUsage,
    pub finish_reason: FinishReason,
    /// Provider-reported prompt-caching counters, when available.
    /// `None` for providers without caching, or when the response did
    /// not include cache fields (cache disabled or first-write turn).
    pub cache_usage: Option<CacheUsage>,
}

/// Prompt-cache accounting returned by the provider after a request.
/// Used for telemetry (`llm_cache_read_tokens_total`, hit-ratio gauge)
/// and to make billing predictable in dashboards.
///
/// Field semantics (Anthropic-aligned, generalized):
/// * `cache_read_input_tokens` — tokens served from cache at 0.1× cost.
/// * `cache_creation_input_tokens` — tokens written to cache at 1.25×
///   (5min) or 2× (1h) cost on this turn.
/// * `input_tokens` — uncached input tokens billed at base rate.
/// * `output_tokens` — completion tokens (mirrors `TokenUsage` for
///   provider clients that fill both atomically).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheUsage {
    pub cache_read_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl CacheUsage {
    /// Cache hit ratio for this turn: `read / (read + creation + uncached_input)`.
    /// Returns 0.0 when no input tokens were billed.
    pub fn hit_ratio(&self) -> f32 {
        let denom =
            self.cache_read_input_tokens + self.cache_creation_input_tokens + self.input_tokens;
        if denom == 0 {
            return 0.0;
        }
        self.cache_read_input_tokens as f32 / denom as f32
    }
}

#[derive(Debug, Clone)]
pub enum ResponseContent {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolUse,
    Length,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolDef {
    /// Maximum length allowed for a tool `name` by LLM providers. Both
    /// OpenAI (function calling) and Anthropic (tools) cap tool names at
    /// 64 characters as of 2024-11. A name that exceeds this causes the
    /// provider to reject the entire request, so plugin loaders and MCP
    /// catalog builders validate against it before registration.
    pub const MAX_NAME_LEN: usize = 64;

    /// Build a tool name of the form `{prefix}{id}_{tool}` that always
    /// fits within `MAX_NAME_LEN`. When the natural concatenation is too
    /// long, truncate the tool segment and suffix `_{hash6}` — a 6-char
    /// hex slice of `sha256(id\0tool)` — to preserve uniqueness while
    /// keeping the result deterministic across reloads. Used by extension
    /// and MCP tool registration to avoid silent-drop of long names.
    pub fn fit_name(prefix: &str, id: &str, tool: &str) -> String {
        let full = format!("{prefix}{id}_{tool}");
        if full.len() <= Self::MAX_NAME_LEN {
            return full;
        }
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(id.as_bytes());
        h.update([0u8]);
        h.update(tool.as_bytes());
        let digest = h.finalize();
        let hash = &hex::encode(digest)[..6];

        // Budget for the tool head: prefix + id + '_' + head + '_' + hash6.
        let fixed = prefix.len() + id.len() + 1 + 1 + 6;
        if fixed <= Self::MAX_NAME_LEN {
            let budget = Self::MAX_NAME_LEN - fixed;
            let head: String = tool.chars().take(budget).collect();
            return format!("{prefix}{id}_{head}_{hash}");
        }
        // id alone busts the budget — truncate id as well.
        let id_budget = Self::MAX_NAME_LEN.saturating_sub(prefix.len() + 1 + 6);
        let id_head: String = id.chars().take(id_budget).collect();
        format!("{prefix}{id_head}_{hash}")
    }
}

#[cfg(test)]
mod fit_name_tests {
    use super::ToolDef;

    #[test]
    fn passthrough_when_short() {
        assert_eq!(ToolDef::fit_name("ext_", "echo", "say"), "ext_echo_say");
    }

    #[test]
    fn hashes_overflow_tool_and_fits() {
        let tool = "long_".repeat(30);
        let name = ToolDef::fit_name("ext_", "mybot", &tool);
        assert!(name.starts_with("ext_mybot_"));
        assert_eq!(name.len(), ToolDef::MAX_NAME_LEN);
    }

    #[test]
    fn different_inputs_yield_different_hashes() {
        let tool_a = "process_data_batch_".to_string() + &"x".repeat(60);
        let tool_b = "process_data_batch_".to_string() + &"y".repeat(60);
        let a = ToolDef::fit_name("ext_", "mybot", &tool_a);
        let b = ToolDef::fit_name("ext_", "mybot", &tool_b);
        assert_ne!(a, b);
        assert_eq!(a.len(), ToolDef::MAX_NAME_LEN);
        assert_eq!(b.len(), ToolDef::MAX_NAME_LEN);
    }

    #[test]
    fn handles_id_that_busts_budget() {
        let id = "x".repeat(60);
        let name = ToolDef::fit_name("ext_", &id, "t");
        assert!(name.starts_with("ext_"));
        assert!(name.len() <= ToolDef::MAX_NAME_LEN);
    }

    #[test]
    fn is_deterministic() {
        let long = "a".repeat(80);
        let a = ToolDef::fit_name("mcp_", "server", &long);
        let b = ToolDef::fit_name("mcp_", "server", &long);
        assert_eq!(a, b);
    }
}
