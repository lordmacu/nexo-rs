//! `nexo/admin/llm/complete` wire types.
//!
//! Lets a microapp / extension delegate an LLM completion to the
//! daemon, which holds the provider configs (`llm.yaml`) +
//! credentials (`secrets/<NAME>.txt`). The caller chooses the
//! provider id (matching `llm.yaml.providers.<id>`) + model;
//! daemon picks the right factory, builds the request with the
//! authenticated key, and returns the assembled body.
//!
//! Use case: marketing extension generates a draft reply for a
//! lead. It owns the prompt-building logic (system_prompt from
//! the bound agent, multi-turn thread history, operator hint)
//! but doesn't want to duplicate the per-provider HTTP plumbing
//! or hold credentials. Marketing calls `llm/complete` with the
//! agent's `ModelRef` + the full conversation; daemon returns
//! the model output.
//!
//! Capability: `llm_complete`. Operator grants in
//! `extensions.yaml.<id>.capabilities_grant` so a compromised
//! microapp can't run unbounded LLM spend without explicit
//! approval.

use serde::{Deserialize, Serialize};

/// JSON-RPC method for `llm/complete`. Single round-trip;
/// streaming is intentionally not exposed at the admin layer
/// (use the plugin SDK's `complete_llm_stream` for that path —
/// plugins, not extensions, are the streaming surface).
pub const LLM_COMPLETE_METHOD: &str = "nexo/admin/llm/complete";

/// One message in the conversation log handed to the LLM.
/// Mirrors the OpenAI / Anthropic chat-message shape so every
/// factory the daemon hosts can map it 1:1 with no re-shaping.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmChatMessage {
    /// `system` | `user` | `assistant`. Daemon validates the
    /// vocabulary; unknown roles return `invalid_params`.
    pub role: String,
    /// Plain text content. Tool calls / vision / structured
    /// outputs land later via additive variants.
    pub content: String,
}

/// Params for `nexo/admin/llm/complete`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmCompleteInput {
    /// Provider id from `llm.yaml.providers.<id>` (e.g.
    /// `deepseek-19c7`, `anthropic-default`). Daemon resolves
    /// the factory + reads the API key from the daemon-stamped
    /// process env.
    pub provider: String,
    /// Model name passed verbatim to the factory (e.g.
    /// `deepseek-v4-flash`, `claude-haiku-4-5`). The factory
    /// validates against its `known_models` list when present.
    pub model: String,
    /// Full conversation log, oldest first. Caller is
    /// responsible for fitting the budget — daemon does NOT
    /// truncate. `messages[0].role` is typically `system`.
    pub messages: Vec<LlmChatMessage>,
    /// Optional ceiling for the response. Factory default when
    /// `None` (typically 1024–2048 depending on provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// 0.0..=2.0 sampling temperature. Factory default when
    /// `None` (typically 0.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Optional system prompt extracted from the first
    /// message — purely a convenience for callers that prefer
    /// the Anthropic-style separated `system` field. When set,
    /// daemon prepends it to `messages` (or routes via the
    /// factory's native system slot when available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// Token counts as reported by the upstream provider. Always
/// best-effort — some providers don't return usage on every
/// response (Anthropic streaming aggregates separately); fields
/// fall back to 0 when unknown.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmUsage {
    /// Tokens billed for the prompt (system + messages).
    #[serde(default)]
    pub prompt_tokens: u64,
    /// Tokens billed for the assistant response.
    #[serde(default)]
    pub completion_tokens: u64,
    /// Sum (`prompt_tokens + completion_tokens`). Some providers
    /// return this directly; others let us derive it.
    #[serde(default)]
    pub total_tokens: u64,
}

/// Response for `nexo/admin/llm/complete`. Single-turn — caller
/// stitches consecutive responses into a chat loop themselves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmCompleteResponse {
    /// Assistant content, plain text. Tool / function call
    /// shape lands as an additive `tool_calls` field if the
    /// factory ever surfaces them — never replaces this.
    pub content: String,
    /// Echo of the model id the factory actually called. Useful
    /// when the caller passed an alias (e.g. `claude-latest`)
    /// and wants to log the resolved version.
    pub model: String,
    /// Token counts. Defaults to all-zeros when the provider
    /// didn't report.
    #[serde(default)]
    pub usage: LlmUsage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_input() {
        let i = LlmCompleteInput {
            provider: "deepseek-19c7".into(),
            model: "deepseek-v4-flash".into(),
            messages: vec![
                LlmChatMessage {
                    role: "system".into(),
                    content: "Eres Ana, asistente comercial.".into(),
                },
                LlmChatMessage {
                    role: "user".into(),
                    content: "¿Tienen plan empresarial?".into(),
                },
            ],
            max_tokens: Some(512),
            temperature: Some(0.7),
            system_prompt: None,
        };
        let v = serde_json::to_value(&i).unwrap();
        let back: LlmCompleteInput = serde_json::from_value(v).unwrap();
        assert_eq!(i, back);
    }

    #[test]
    fn round_trip_response() {
        let r = LlmCompleteResponse {
            content: "Hola, sí — el plan empresarial cubre…".into(),
            model: "deepseek-v4-flash".into(),
            usage: LlmUsage {
                prompt_tokens: 42,
                completion_tokens: 18,
                total_tokens: 60,
            },
        };
        let v = serde_json::to_value(&r).unwrap();
        let back: LlmCompleteResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn method_constant() {
        assert_eq!(LLM_COMPLETE_METHOD, "nexo/admin/llm/complete");
    }
}
