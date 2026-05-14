//! `nexo/admin/agents/*` wire types.
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
    /// Multi-tenant filter. `Some(id)` returns
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
    /// Workspace directory the framework loads each turn (IDENTITY,
    /// SOUL, USER, AGENTS, MEMORY, plus `extra_docs`). Empty when
    /// the agent has no workspace layer wired.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workspace: String,
    /// Workspace-relative knowledge files appended to the system
    /// prompt as `# RULES — <filename>` blocks. Empty when no
    /// knowledge is wired.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_docs: Vec<String>,
    /// Proactive tick-loop config. `None` when the
    /// yaml omits the `heartbeat` block (back-compat — pre-existing
    /// agents keep the framework default of disabled). When
    /// `Some`, the operator UI renders the on/off toggle + interval
    /// preset picker. Mirrors `nexo_config::types::agents::
    /// HeartbeatConfig`; humantime parsing of `interval` happens at
    /// the daemon yaml-load layer, so this wire shape is opaque
    /// to the microapp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat: Option<HeartbeatWire>,
    /// Phase 81.31 — localised persona catalog. `None` for legacy
    /// agents where the daemon doesn't have a `PersonaSnapshotReader`
    /// wired; otherwise carries the available locales + per-locale
    /// snapshots of `system_prompt + IDENTITY + SOUL + USER +
    /// AGENTS`. Populated by `agents/get` so the admin renders the
    /// wizard's "copy existing" + locale-picker flow without an
    /// extra roundtrip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_locales: Option<crate::admin::persona::PersonaLocales>,
}

/// Wire mirror of `nexo_config::types::agents::HeartbeatConfig`.
/// `interval` is a humantime literal (`"5m"`, `"30s"`, `"1h"`)
/// passed through verbatim — daemon yaml-load parses it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeartbeatWire {
    /// Master switch — `false` keeps the runtime quiet (default
    /// when the yaml omits the field).
    pub enabled: bool,
    /// humantime literal (`"5m"`, `"30s"`, `"1h"`, `"4h"`,
    /// `"1d"`). Empty / malformed values fall back to the
    /// daemon-side default (`"5m"`) at boot.
    pub interval: String,
}

/// LLM provider + model pointer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    /// Path where per-session JSONL transcripts are written. The
    /// `TranscriptWriter` is the sole emitter of `TranscriptAppended`
    /// firehose events, so an empty value disables live conversation
    /// updates in operator dashboards. `None` keeps existing yaml
    /// value (no write); pass `Some("")` to explicitly disable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcripts_dir: Option<String>,
    /// Workspace directory the framework loads on every turn. Holds
    /// the IDENTITY/SOUL/USER/AGENTS/MEMORY markdowns that compose
    /// the agent's persona + the knowledge docs referenced in
    /// `extra_docs`. `None` keeps existing yaml value; empty string
    /// disables the workspace layer entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Workspace-relative markdown files appended to the system prompt
    /// alongside IDENTITY/SOUL/USER/AGENTS. Used by the microapp
    /// uploader to expose .txt/.md/.pdf knowledge as source of truth
    /// (each entry renders as `# RULES — <filename>`). `None` keeps
    /// the existing list; empty vec clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_docs: Option<Vec<String>>,
    /// Proactive tick-loop. `None` keeps the existing
    /// yaml block (or default-disabled for new agents). `Some`
    /// replaces the whole `heartbeat` block on disk so flipping
    /// the toggle off explicitly persists `enabled: false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat: Option<HeartbeatWire>,
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
            workspace: String::new(),
            extra_docs: vec![],
            heartbeat: None,
            persona_locales: None,
        };
        let v = serde_json::to_value(&d).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("language"));
        // heartbeat skips when None.
        assert!(!obj.contains_key("heartbeat"));
    }

    /// `Some(HeartbeatWire)` round-trips through serde.
    #[test]
    fn agent_detail_heartbeat_round_trip() {
        let d = AgentDetail {
            id: "ana".into(),
            model: ModelRef {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            },
            active: true,
            allowed_tools: vec!["*".into()],
            inbound_bindings: vec![],
            system_prompt: String::new(),
            language: None,
            workspace: String::new(),
            extra_docs: vec![],
            heartbeat: Some(HeartbeatWire {
                enabled: true,
                interval: "30m".into(),
            }),
            persona_locales: None,
        };
        let v = serde_json::to_value(&d).unwrap();
        let back: AgentDetail = serde_json::from_value(v).unwrap();
        assert_eq!(d, back);
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
            transcripts_dir: None,
            workspace: None,
            extra_docs: None,
            heartbeat: None,
        };
        let v = serde_json::to_value(&i).unwrap();
        let obj = v.as_object().unwrap();
        // Only id + model present on the wire.
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("model"));
    }
}
