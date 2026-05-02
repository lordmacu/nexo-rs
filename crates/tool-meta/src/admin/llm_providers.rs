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
    /// Provider id (matches `llm.yaml.providers.<id>` or
    /// `llm.yaml.tenants.<tenant_id>.providers.<id>`).
    pub id: String,
    /// HTTP base URL (e.g. `https://api.minimax.chat/v1`).
    pub base_url: String,
    /// Env var name holding the API key. Operator UIs render the
    /// VAR NAME (not the value); the value is read at runtime by
    /// the LLM client.
    pub api_key_env: String,
    /// Phase 83.8.12.5.c — owning tenant. `None` for the global
    /// provider table; `Some(tenant_id)` when the row lives
    /// under `llm.yaml.tenants.<tenant_id>.providers.<id>`.
    /// Operator UI uses this to badge per-tenant providers
    /// without re-querying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_scope: Option<String>,
}

/// Phase 83.8.12.5.c — list filter shared by all
/// `llm_providers` admin RPC methods.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmProvidersListFilter {
    /// `Some(tenant_id)` returns only providers under
    /// `llm.yaml.tenants.<id>.providers`. `Some("")` is invalid
    /// (-32602). `None` returns the global table only (matches
    /// pre-Phase 83.8.12.5 behaviour). Operator-level UIs that
    /// want EVERY scope set this to `None` and merge with
    /// per-tenant lists themselves — explicit > implicit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
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
    /// Phase 83.8.12.5.c — when `Some(tenant_id)`, the upsert
    /// targets `llm.yaml.tenants.<tenant_id>.providers.<id>`
    /// instead of the global `providers.<id>`. `None` keeps
    /// pre-83.8.12.5 behaviour (writes the global table).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

/// Params for `nexo/admin/llm_providers/delete`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmProvidersDeleteParams {
    /// Provider id to remove.
    pub provider_id: String,
    /// Phase 83.8.12.5.c — when `Some(tenant_id)`, the delete
    /// targets the tenant-scoped namespace. `None` removes
    /// from the global table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
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
            tenant_scope: None,
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
            tenant_id: None,
        };
        let v = serde_json::to_value(&i).unwrap();
        let back: LlmProviderUpsertInput = serde_json::from_value(v).unwrap();
        assert_eq!(i, back);
    }

    /// Phase 83.8.12.5.c — `tenant_scope` round-trips when
    /// present and is omitted when `None` (graceful absence
    /// for legacy operators).
    #[test]
    fn provider_summary_tenant_scope_round_trip() {
        let with = LlmProviderSummary {
            id: "minimax".into(),
            base_url: "https://api.minimax.io".into(),
            api_key_env: "MINIMAX_KEY_ACME".into(),
            tenant_scope: Some("acme".into()),
        };
        let s = serde_json::to_string(&with).unwrap();
        assert!(s.contains("\"tenant_scope\":\"acme\""));
        let back: LlmProviderSummary = serde_json::from_str(&s).unwrap();
        assert_eq!(back, with);

        let without = LlmProviderSummary {
            id: "minimax".into(),
            base_url: "https://api.minimax.io".into(),
            api_key_env: "MINIMAX_KEY_GLOBAL".into(),
            tenant_scope: None,
        };
        let s = serde_json::to_string(&without).unwrap();
        assert!(!s.contains("tenant_scope"));
    }

    /// Phase 83.8.12.5.c — pre-Phase 83.8.12.5 microapps emit
    /// no `tenant_scope` field on summaries; deserialise must
    /// default to `None`.
    #[test]
    fn provider_summary_legacy_payload_deserialises() {
        let raw = r#"{"id":"minimax","base_url":"https://x","api_key_env":"K"}"#;
        let s: LlmProviderSummary = serde_json::from_str(raw).unwrap();
        assert!(s.tenant_scope.is_none());
    }

    #[test]
    fn upsert_input_with_tenant_id_round_trip() {
        let i = LlmProviderUpsertInput {
            id: "minimax".into(),
            base_url: "https://api.minimax.io".into(),
            api_key_env: "MINIMAX_KEY_ACME".into(),
            headers: BTreeMap::new(),
            tenant_id: Some("acme".into()),
        };
        let s = serde_json::to_string(&i).unwrap();
        assert!(s.contains("\"tenant_id\":\"acme\""));
        let back: LlmProviderUpsertInput = serde_json::from_str(&s).unwrap();
        assert_eq!(back, i);
    }

    #[test]
    fn delete_params_with_tenant_id_round_trip() {
        let p = LlmProvidersDeleteParams {
            provider_id: "minimax".into(),
            tenant_id: Some("acme".into()),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"tenant_id\":\"acme\""));
        let back: LlmProvidersDeleteParams = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn list_filter_round_trip_with_and_without_tenant() {
        let with = LlmProvidersListFilter {
            tenant_id: Some("acme".into()),
        };
        let s = serde_json::to_string(&with).unwrap();
        assert_eq!(s, r#"{"tenant_id":"acme"}"#);
        let back: LlmProvidersListFilter = serde_json::from_str(&s).unwrap();
        assert_eq!(back, with);

        let without = LlmProvidersListFilter::default();
        let s = serde_json::to_string(&without).unwrap();
        assert_eq!(s, "{}", "tenant_id None must be omitted");
    }
}
