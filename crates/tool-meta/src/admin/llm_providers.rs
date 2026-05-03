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

/// Phase 82.10.l — JSON-RPC method that probes a configured LLM
/// provider's reachability + key validity from the daemon's
/// network position.
///
/// Operator UIs (e.g. M9 wizard's Step 1) call this AFTER
/// `secrets/write` + `llm_providers/upsert` to confirm the
/// daemon successfully resolved the env var AND can reach the
/// provider AND the key is accepted. Microapp's own probe
/// (`/api/onboarding/llm/probe`) only validates browser → provider;
/// this RPC closes the gap by validating daemon → provider.
pub const LLM_PROVIDERS_PROBE_METHOD: &str = "nexo/admin/llm_providers/probe";

/// Params for [`LLM_PROVIDERS_PROBE_METHOD`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmProviderProbeInput {
    /// Provider id matching `llm.yaml.providers.<id>` (or
    /// `tenants.<tenant_id>.providers.<id>` when scoped).
    pub provider_id: String,
    /// Phase 83.8.12.5.c — tenant scope. `None` reads the global
    /// table; `Some(id)` reads the tenant namespace. v1 adapter
    /// ignores tenant scope (always reads global) — full
    /// support lands as `82.10.l.tenant`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

/// Result for [`LLM_PROVIDERS_PROBE_METHOD`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmProviderProbeResponse {
    /// `true` when the HTTP request returned 2xx.
    pub ok: bool,
    /// HTTP status from `GET {base_url}/models`. `0` for
    /// pre-request errors (DNS, connect timeout, env var unset,
    /// provider id missing).
    pub status: u16,
    /// End-to-end latency including DNS + TLS + body read.
    pub latency_ms: u64,
    /// Number of models in `data: [...]` (OpenAI-compat shape).
    /// `None` when the body isn't JSON or doesn't have `data`.
    /// Non-fatal — the probe still reports `ok: true` if HTTP
    /// status was 2xx.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_count: Option<usize>,
    /// Sanitised error string. Never echoes the API key —
    /// every match of the key value (and its 8-char prefix) is
    /// replaced with `<redacted>` before populating this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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

    /// Phase 82.10.l — probe wire shapes round-trip cleanly +
    /// `tenant_id` skips when None.
    #[test]
    fn probe_input_round_trip() {
        let with = LlmProviderProbeInput {
            provider_id: "minimax".into(),
            tenant_id: Some("acme".into()),
        };
        let v = serde_json::to_value(&with).unwrap();
        let back: LlmProviderProbeInput = serde_json::from_value(v).unwrap();
        assert_eq!(back, with);

        let without = LlmProviderProbeInput {
            provider_id: "minimax".into(),
            tenant_id: None,
        };
        let s = serde_json::to_string(&without).unwrap();
        assert!(!s.contains("tenant_id"), "None tenant_id must be omitted");
    }

    #[test]
    fn probe_response_round_trip() {
        let r = LlmProviderProbeResponse {
            ok: true,
            status: 200,
            latency_ms: 142,
            model_count: Some(5),
            error: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        let back: LlmProviderProbeResponse = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn probe_method_constant() {
        assert_eq!(
            LLM_PROVIDERS_PROBE_METHOD,
            "nexo/admin/llm_providers/probe"
        );
    }
}
