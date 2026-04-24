//! Plain data types for `sampling/createMessage` request/response.

#[derive(Debug, Clone)]
pub struct SamplingRequest {
    pub server_id: String,
    pub messages: Vec<SamplingMessage>,
    pub model_preferences: Option<ModelPreferences>,
    pub system_prompt: Option<String>,
    pub include_context: IncludeContext,
    pub temperature: Option<f32>,
    pub max_tokens: u32,
    pub stop_sequences: Vec<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct SamplingMessage {
    pub role: SamplingRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplingRole {
    User,
    Assistant,
}

impl SamplingRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelPreferences {
    pub hints: Vec<String>,
    pub cost_priority: Option<f32>,
    pub speed_priority: Option<f32>,
    pub intelligence_priority: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludeContext {
    None,
    ThisServer,
    AllServers,
}

impl IncludeContext {
    pub fn parse(s: &str) -> Self {
        match s {
            "thisServer" => Self::ThisServer,
            "allServers" => Self::AllServers,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SamplingResponse {
    pub role: SamplingRole,
    pub text: String,
    pub model: String,
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
}

impl StopReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::EndTurn => "endTurn",
            Self::MaxTokens => "maxTokens",
            Self::StopSequence => "stopSequence",
        }
    }
}
