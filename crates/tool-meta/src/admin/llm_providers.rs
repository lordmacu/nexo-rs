//! Phase 82.10.f — `nexo/admin/llm_providers/*` wire types.
//!
//! Operates on `llm.yaml.providers.<id>`. API keys stay as
//! `${ENV_VAR}` references — the operator owns the secret; the
//! admin RPC layer never sees plaintext keys.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One row of the `llm_providers/list` response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmProviderSummary {
    /// Provider id (matches `llm.yaml.providers.<id>`).
    pub id: String,
    /// HTTP base URL (e.g. `https://api.minimax.chat/v1`).
    pub base_url: String,
    /// Env var name holding the API key. Operator UIs render the
    /// VAR NAME (not the value); the value is read at runtime by
    /// the LLM client.
    pub api_key_env: String,
}

/// Response for `nexo/admin/llm_providers/list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmProvidersListResponse {
    /// Providers in stable alpha order by id.
    pub providers: Vec<LlmProviderSummary>,
}

/// Params for `nexo/admin/llm_providers/upsert`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmProviderUpsertInput {
    /// Provider id.
    pub id: String,
    /// HTTP base URL.
    pub base_url: String,
    /// Env var name holding the API key.
    pub api_key_env: String,
    /// Optional extra HTTP headers (e.g. `X-Custom-Auth`). Empty
    /// map if not needed.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// Params for `nexo/admin/llm_providers/delete`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmProvidersDeleteParams {
    /// Provider id to remove.
    pub provider_id: String,
}

/// Response for `nexo/admin/llm_providers/delete`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmProvidersDeleteResponse {
    /// `true` when the yaml block was removed. `false` when the
    /// id was already absent (idempotent).
    pub removed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_summary_round_trip() {
        let s = LlmProviderSummary {
            id: "minimax".into(),
            base_url: "https://api.minimax.chat/v1".into(),
            api_key_env: "MINIMAX_API_KEY".into(),
        };
        let v = serde_json::to_value(&s).unwrap();
        let back: LlmProviderSummary = serde_json::from_value(v).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn upsert_input_default_empty_headers() {
        let i = LlmProviderUpsertInput {
            id: "minimax".into(),
            base_url: "x".into(),
            api_key_env: "Y".into(),
            headers: BTreeMap::new(),
        };
        let v = serde_json::to_value(&i).unwrap();
        let back: LlmProviderUpsertInput = serde_json::from_value(v).unwrap();
        assert_eq!(i, back);
    }
}
