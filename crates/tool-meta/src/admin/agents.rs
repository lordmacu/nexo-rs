//! Phase 82.10.c — `nexo/admin/agents/*` wire types.
//!
//! Daemon side handlers in `nexo_core::agent::admin_rpc::domains
//! ::agents` consume these as params / produce as results.
//! SDK side `AdminClient::agents()` accessor takes / returns
//! these types.

use serde::{Deserialize, Serialize};

/// Params for `nexo/admin/agents/list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentsListFilter {
    /// When `true`, omit agents whose `active` flag is `false`.
    /// Default `false` returns all agents.
    pub active_only: bool,
    /// Filter by primary plugin id (e.g. `"whatsapp"`). `None`
    /// returns every plugin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_filter: Option<String>,
    /// Phase 83.8.12 — multi-tenant filter. `Some(id)` returns
    /// only agents whose `agents.yaml.<id>.tenant_id` matches.
    /// `None` returns every agent regardless of tenant
    /// (operator scope).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

/// One row of the `agents/list` result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSummary {
    /// Stable agent id (matches `agents.yaml.<id>`).
    pub id: String,
    /// Whether the agent is active. False = soft-deleted but
    /// the yaml block still present (drain in flight).
    pub active: bool,
    /// LLM provider (`minimax`, `anthropic`, `openai`, `gemini`,
    /// `deepseek`, `xai`, `mistral`, future).
    pub model_provider: String,
    /// Inbound binding count. Operators use this to spot agents
    /// without any binding configured.
    pub bindings_count: usize,
}

/// Result of `nexo/admin/agents/list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentsListResponse {
    /// Matching agents in stable order (alpha by id).
    pub agents: Vec<AgentSummary>,
}

/// Params for `nexo/admin/agents/get`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentsGetParams {
    /// Stable agent id.
    pub agent_id: String,
}

/// Result of `nexo/admin/agents/get` and `agents/upsert`. Subset
/// of yaml-readable fields the operator UI needs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentDetail {
    /// Stable agent id.
    pub id: String,
    /// LLM provider + model.
    pub model: ModelRef,
    /// Active flag.
    pub active: bool,
    /// Allowed tools glob list (`["*"]` = all).
    pub allowed_tools: Vec<String>,
    /// Inbound bindings (whatsapp, future telegram/email, …).
    pub inbound_bindings: Vec<BindingSummary>,
    /// System prompt (may be large; future page may stream).
    pub system_prompt: String,
    /// Optional output language directive (`"es"`, `"en"`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// LLM provider + model pointer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelRef {
    /// Provider id from `llm.yaml.providers.*`.
    pub provider: String,
    /// Model name within the provider (e.g. `"MiniMax-M2.5"`,
    /// `"claude-opus-4-7"`).
    pub model: String,
}

/// Per-binding summary surfaced to admin UIs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BindingSummary {
    /// Plugin id (e.g. `"whatsapp"`).
    pub plugin: String,
    /// Optional account/instance discriminator
    /// (`"personal"` / `"business"` / …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

/// Params for `nexo/admin/agents/upsert`.
///
/// Upsert semantic: if `id` exists, fields supplied here REPLACE
/// the corresponding yaml block. Fields set to `None` inherit the
/// existing yaml value. New agent creation sets every required
/// field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentUpsertInput {
    /// Stable agent id.
    pub id: String,
    /// LLM provider + model.
    pub model: ModelRef,
    /// `None` keeps the existing yaml value (or default if new).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    /// `None` keeps existing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbound_bindings: Option<Vec<BindingSummary>>,
    /// `None` keeps existing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// `None` keeps existing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// `None` keeps existing; defaults to `true` for new agents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

/// Params for `nexo/admin/agents/delete`. Soft-delete:
/// daemon marks `active=false`, drains in-flight sessions, then
/// removes the yaml block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentsDeleteParams {
    /// Stable agent id to remove.
    pub agent_id: String,
}

/// Empty-body successful delete result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentsDeleteResponse {
    /// Whether the yaml block was actually removed (false → was
    /// already absent — idempotent delete).
    pub removed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_list_filter_default_serialises_compact() {
        let f = AgentsListFilter::default();
        let v = serde_json::to_value(&f).unwrap();
        // active_only defaults to false, plugin_filter omitted via
        // skip_serializing_if.
        assert_eq!(v, serde_json::json!({ "active_only": false }));
    }

    #[test]
    fn agent_summary_round_trip() {
        let s = AgentSummary {
            id: "ana".into(),
            active: true,
            model_provider: "minimax".into(),
            bindings_count: 2,
        };
        let v = serde_json::to_value(&s).unwrap();
        let back: AgentSummary = serde_json::from_value(v).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn agent_detail_skips_none_language() {
        let d = AgentDetail {
            id: "ana".into(),
            model: ModelRef {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            },
            active: true,
            allowed_tools: vec!["*".into()],
            inbound_bindings: vec![],
            system_prompt: "hi".into(),
            language: None,
        };
        let v = serde_json::to_value(&d).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("language"));
    }

    #[test]
    fn agent_upsert_input_omits_none_fields_on_serialise() {
        let i = AgentUpsertInput {
            id: "ana".into(),
            model: ModelRef {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            },
            allowed_tools: None,
            inbound_bindings: None,
            system_prompt: None,
            language: None,
            active: None,
        };
        let v = serde_json::to_value(&i).unwrap();
        let obj = v.as_object().unwrap();
        // Only id + model present on the wire.
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("model"));
    }
}
