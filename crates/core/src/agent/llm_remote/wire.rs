//! Phase 81.25 — wire types for `RemoteLlmClient`.
//!
//! Subprocess plugins exchange `ChatRequest` / `ChatResponse` /
//! `StreamChunk` over stdio JSON-RPC. The in-tree types in
//! `nexo-llm` aren't all `Serialize + Deserialize` (e.g.
//! `PromptBlock.label: &'static str`), so we ship parallel
//! wire-only types here with full serde derives + conversion
//! fns. Wire fields drop information that's host-only telemetry
//! (e.g. `PromptBlock.label`) — providers never see those.
//!
//! Wire-type drift policy: when `ChatRequest` adds a field, the
//! conversion fn here MUST list it (compiler enforces). New
//! fields surface as a deliberate decision: pass through to wire
//! or drop at the boundary.

use nexo_llm::prompt_block::{CachePolicy, PromptBlock};
use nexo_llm::stream::StreamChunk;
use nexo_llm::types::{
    Attachment, AttachmentData, CacheUsage, ChatMessage, ChatRequest, ChatResponse, ChatRole,
    FinishReason, ResponseContent, TokenUsage, ToolCall, ToolChoice, ToolDef,
};
use serde::{Deserialize, Serialize};

// ── Wire types ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireChatRequest {
    pub model: String,
    pub messages: Vec<WireChatMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireToolDef>,
    pub max_tokens: u32,
    pub temperature: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(default)]
    pub tool_choice: WireToolChoice,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system_blocks: Vec<WirePromptBlock>,
    #[serde(default)]
    pub cache_tools: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireChatResponse {
    pub content: WireResponseContent,
    pub usage: WireTokenUsage,
    pub finish_reason: WireFinishReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_usage: Option<WireCacheUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireChatMessage {
    pub role: WireChatRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<WireToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<WireAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WireChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireToolChoice {
    #[default]
    Auto,
    Any,
    None,
    Specific {
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireResponseContent {
    Text { text: String },
    ToolCalls { tool_calls: Vec<WireToolCall> },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireTokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireCacheUsage {
    pub cache_read_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireFinishReason {
    Stop,
    ToolUse,
    Length,
    Other { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WireCachePolicy {
    None,
    Ephemeral5m,
    Ephemeral1h,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePromptBlock {
    /// `PromptBlock.label` (`&'static str`, telemetry-only) is
    /// dropped at the wire boundary — providers never see it.
    pub text: String,
    pub cache: WireCachePolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireAttachment {
    pub kind: String,
    pub mime_type: String,
    #[serde(flatten)]
    pub data: WireAttachmentData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "data_kind", rename_all = "snake_case")]
pub enum WireAttachmentData {
    Base64 { base64: String },
    Url { url: String },
    Path { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireStreamChunk {
    TextDelta { delta: String },
    ToolCallStart { id: String, name: String },
    ToolCallArgsDelta { id: String, delta: String },
    ToolCallEnd { id: String },
    Usage { usage: WireTokenUsage },
    End { finish_reason: WireFinishReason },
}

// ── Conversions ──────────────────────────────────────────────────

pub fn request_to_wire(req: &ChatRequest) -> WireChatRequest {
    WireChatRequest {
        model: req.model.clone(),
        messages: req.messages.iter().map(message_to_wire).collect(),
        tools: req.tools.iter().map(tool_def_to_wire).collect(),
        max_tokens: req.max_tokens,
        temperature: req.temperature,
        system_prompt: req.system_prompt.clone(),
        stop_sequences: req.stop_sequences.clone(),
        tool_choice: tool_choice_to_wire(&req.tool_choice),
        system_blocks: req.system_blocks.iter().map(prompt_block_to_wire).collect(),
        cache_tools: req.cache_tools,
    }
}

pub fn wire_to_response(w: WireChatResponse) -> ChatResponse {
    ChatResponse {
        content: wire_to_content(w.content),
        usage: wire_to_token_usage(w.usage),
        finish_reason: wire_to_finish_reason(w.finish_reason),
        cache_usage: w.cache_usage.map(wire_to_cache_usage),
    }
}

pub fn wire_to_chunk(w: WireStreamChunk) -> StreamChunk {
    match w {
        WireStreamChunk::TextDelta { delta } => StreamChunk::TextDelta { delta },
        WireStreamChunk::ToolCallStart { id, name } => StreamChunk::ToolCallStart { id, name },
        WireStreamChunk::ToolCallArgsDelta { id, delta } => {
            StreamChunk::ToolCallArgsDelta { id, delta }
        }
        WireStreamChunk::ToolCallEnd { id } => StreamChunk::ToolCallEnd { id },
        WireStreamChunk::Usage { usage } => StreamChunk::Usage(wire_to_token_usage(usage)),
        WireStreamChunk::End { finish_reason } => StreamChunk::End {
            finish_reason: wire_to_finish_reason(finish_reason),
        },
    }
}

fn message_to_wire(m: &ChatMessage) -> WireChatMessage {
    WireChatMessage {
        role: chat_role_to_wire(&m.role),
        content: m.content.clone(),
        tool_call_id: m.tool_call_id.clone(),
        name: m.name.clone(),
        tool_calls: m.tool_calls.iter().map(tool_call_to_wire).collect(),
        attachments: m.attachments.iter().map(attachment_to_wire).collect(),
    }
}

fn chat_role_to_wire(r: &ChatRole) -> WireChatRole {
    match r {
        ChatRole::System => WireChatRole::System,
        ChatRole::User => WireChatRole::User,
        ChatRole::Assistant => WireChatRole::Assistant,
        ChatRole::Tool => WireChatRole::Tool,
    }
}

fn tool_def_to_wire(t: &ToolDef) -> WireToolDef {
    WireToolDef {
        name: t.name.clone(),
        description: t.description.clone(),
        parameters: t.parameters.clone(),
    }
}

fn tool_choice_to_wire(c: &ToolChoice) -> WireToolChoice {
    match c {
        ToolChoice::Auto => WireToolChoice::Auto,
        ToolChoice::Any => WireToolChoice::Any,
        ToolChoice::None => WireToolChoice::None,
        ToolChoice::Specific(name) => WireToolChoice::Specific { name: name.clone() },
    }
}

fn tool_call_to_wire(c: &ToolCall) -> WireToolCall {
    WireToolCall {
        id: c.id.clone(),
        name: c.name.clone(),
        arguments: c.arguments.clone(),
    }
}

fn prompt_block_to_wire(b: &PromptBlock) -> WirePromptBlock {
    WirePromptBlock {
        text: b.text.clone(),
        cache: cache_policy_to_wire(&b.cache),
    }
}

fn cache_policy_to_wire(p: &CachePolicy) -> WireCachePolicy {
    match p {
        CachePolicy::None => WireCachePolicy::None,
        CachePolicy::Ephemeral5m => WireCachePolicy::Ephemeral5m,
        CachePolicy::Ephemeral1h => WireCachePolicy::Ephemeral1h,
    }
}

fn attachment_to_wire(a: &Attachment) -> WireAttachment {
    WireAttachment {
        kind: a.kind.clone(),
        mime_type: a.mime_type.clone(),
        data: match &a.data {
            AttachmentData::Base64 { base64 } => WireAttachmentData::Base64 {
                base64: base64.clone(),
            },
            AttachmentData::Url { url } => WireAttachmentData::Url { url: url.clone() },
            AttachmentData::Path { path } => WireAttachmentData::Path { path: path.clone() },
        },
    }
}

fn wire_to_content(c: WireResponseContent) -> ResponseContent {
    match c {
        WireResponseContent::Text { text } => ResponseContent::Text(text),
        WireResponseContent::ToolCalls { tool_calls } => {
            ResponseContent::ToolCalls(tool_calls.into_iter().map(wire_to_tool_call).collect())
        }
    }
}

fn wire_to_tool_call(c: WireToolCall) -> ToolCall {
    ToolCall {
        id: c.id,
        name: c.name,
        arguments: c.arguments,
    }
}

fn wire_to_token_usage(u: WireTokenUsage) -> TokenUsage {
    TokenUsage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
    }
}

fn wire_to_cache_usage(u: WireCacheUsage) -> CacheUsage {
    CacheUsage {
        cache_read_input_tokens: u.cache_read_input_tokens,
        cache_creation_input_tokens: u.cache_creation_input_tokens,
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
    }
}

fn wire_to_finish_reason(f: WireFinishReason) -> FinishReason {
    match f {
        WireFinishReason::Stop => FinishReason::Stop,
        WireFinishReason::ToolUse => FinishReason::ToolUse,
        WireFinishReason::Length => FinishReason::Length,
        WireFinishReason::Other { reason } => FinishReason::Other(reason),
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_round_trip() {
        let req = ChatRequest::new(
            "command-r",
            vec![ChatMessage {
                role: ChatRole::User,
                content: "hi".into(),
                tool_call_id: None,
                name: None,
                tool_calls: Vec::new(),
                attachments: Vec::new(),
            }],
        );
        let wire = request_to_wire(&req);
        let s = serde_json::to_string(&wire).unwrap();
        let back: WireChatRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.model, "command-r");
        assert_eq!(back.messages.len(), 1);
        assert_eq!(back.messages[0].role, WireChatRole::User);
        assert_eq!(back.max_tokens, 4096);
    }

    #[test]
    fn chat_response_round_trip() {
        let wire = WireChatResponse {
            content: WireResponseContent::Text {
                text: "hello".into(),
            },
            usage: WireTokenUsage {
                prompt_tokens: 5,
                completion_tokens: 1,
            },
            finish_reason: WireFinishReason::Stop,
            cache_usage: None,
        };
        let s = serde_json::to_string(&wire).unwrap();
        let back: WireChatResponse = serde_json::from_str(&s).unwrap();
        let resp = wire_to_response(back);
        match resp.content {
            ResponseContent::Text(t) => assert_eq!(t, "hello"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(resp.finish_reason, FinishReason::Stop);
    }

    #[test]
    fn stream_chunk_serializes_per_variant() {
        let cases: Vec<WireStreamChunk> = vec![
            WireStreamChunk::TextDelta {
                delta: "hello".into(),
            },
            WireStreamChunk::ToolCallStart {
                id: "1".into(),
                name: "fetch".into(),
            },
            WireStreamChunk::ToolCallArgsDelta {
                id: "1".into(),
                delta: "{\"x\":".into(),
            },
            WireStreamChunk::ToolCallEnd { id: "1".into() },
            WireStreamChunk::Usage {
                usage: WireTokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 2,
                },
            },
            WireStreamChunk::End {
                finish_reason: WireFinishReason::Stop,
            },
        ];
        for w in cases {
            let s = serde_json::to_string(&w).unwrap();
            let back: WireStreamChunk = serde_json::from_str(&s).unwrap();
            // Re-serialize for equality check (no Eq on WireStreamChunk).
            assert_eq!(serde_json::to_string(&back).unwrap(), s);
        }
    }
}
