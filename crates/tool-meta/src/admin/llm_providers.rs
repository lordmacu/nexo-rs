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
///
/// NOT marked `#[non_exhaustive]` because operators in other
/// crates (e.g. agent-creator microapp's onboarding routes)
/// construct it with literal struct expressions + `..Default`.
/// New optional fields keep landing additively under
/// `#[serde(default)]` so wire-level back-compat is preserved.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmProviderUpsertInput {
    /// Provider INSTANCE id. Phase 82.10.s — distinct from the
    /// factory id; can be e.g. `"minimax-cliente-a"` while
    /// `factory_type: Some("minimax")` routes against the
    /// `MiniMaxFactory`.
    pub id: String,
    /// HTTP base URL.
    pub base_url: String,
    /// LEGACY env var name holding the API key. Phase 82.10.s
    /// recommends `api_key_secret_value` instead — env vars
    /// collide between microapps in the same daemon. Kept for
    /// back-compat with pre-82.10.s yamls and the M9 wizard's
    /// existing flow.
    #[serde(default)]
    pub api_key_env: String,
    /// Optional extra HTTP headers (e.g. `X-Custom-Auth`). Empty
    /// map if not needed.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Phase 82.10.s — factory id from the catalog the daemon
    /// reports via `llm_providers/catalog`. When `Some`, the yaml
    /// instance can be named anything; when `None`, the daemon
    /// treats the instance id as the factory id (legacy path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub factory_type: Option<String>,
    /// Phase 82.10.s — name of the secret in the daemon's
    /// SecretsStore that holds the API key. Mutually exclusive
    /// with `api_key_secret_value` AND `api_key_env`. Useful when
    /// the operator has already written the secret out-of-band
    /// (e.g. via `nexo/admin/secrets/write`) and just wants to
    /// point the provider at it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_secret_id: Option<String>,
    /// Phase 82.10.s — write-through API key. The daemon stamps
    /// the value into the SecretsStore (under a generated id) AND
    /// sets `api_key_secret_id` on the yaml in one transaction.
    /// Audit redaction MUST mask this field — it never lands in
    /// the audit log. Mutually exclusive with `api_key_secret_id`
    /// AND `api_key_env` (loud -32602 on multi-source).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_secret_value: Option<String>,
    /// Phase 83.8.12.5.c — when `Some(tenant_id)`, the upsert
    /// targets `llm.yaml.tenants.<tenant_id>.providers.<id>`
    /// instead of the global `providers.<id>`. `None` keeps
    /// pre-83.8.12.5 behaviour (writes the global table).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Phase 82.10.u — selected auth mode for this instance.
    /// `None` ⇒ `AuthMode::ApiKey` (back-compat). When set to an
    /// OAuth mode, the operator must run `oauth_start` /
    /// `oauth_finish` BEFORE upsert; the resulting bundle's
    /// secret_id then lives in `fields["api_key_secret_id"]` (or
    /// equivalent — the factory's schema decides the field name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<AuthMode>,
    /// Phase 82.10.u — schema-driven credential payload. Each key
    /// MUST match a `CredentialFieldDescriptor.name` from the
    /// factory's `credential_schema`. Values are validated server-
    /// side against the descriptor's `validation` + `required` +
    /// `depends_on`. `secret == true` fields are persisted to the
    /// SecretsStore + redacted in audit logs; `secret == false`
    /// fields land inline in yaml.
    ///
    /// Empty map ⇒ legacy back-compat path (handler uses
    /// `api_key_env` / `api_key_secret_id` / `api_key_secret_value`
    /// instead). Mixed mode (legacy field + non-empty `fields`) is
    /// rejected with `INVALID_FORMAT`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
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

/// Phase 82.10.u — JSON-RPC method that probes a DRAFT provider
/// payload before it lands in `llm.yaml`. Used by the SPA wizard
/// between the "fill credentials" and "pick model" steps so the
/// operator confirms the API key is accepted + enumerates live
/// models WITHOUT polluting the daemon's persisted state on a
/// failed key.
pub const LLM_PROVIDERS_PROBE_DRAFT_METHOD: &str =
    "nexo/admin/llm_providers/probe_draft";

/// Params for [`LLM_PROVIDERS_PROBE_DRAFT_METHOD`].
///
/// Carries the same `fields` shape as `LlmProviderUpsertInput` so
/// the SPA can reuse the form payload directly: probe first, then
/// upsert if the result is `ok`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmProviderProbeDraftInput {
    /// Factory id from the catalog (`minimax`, `anthropic`,
    /// `openai`, etc). The handler resolves the schema + the
    /// downstream HTTP shape from this id.
    pub factory_type: String,
    /// HTTP base URL to probe. SPA pre-fills from
    /// `LlmProviderCatalogEntry::default_base_url` but operators
    /// can override (custom gateway).
    pub base_url: String,
    /// Selected auth mode. `None` ⇒ legacy api_key flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<AuthMode>,
    /// Schema-driven credential payload. Same key set as the
    /// upsert path. The handler reads `fields["api_key"]` (or the
    /// equivalent secret-flagged descriptor name) for the bearer
    /// token; `secret == false` fields (e.g. MiniMax `group_id`)
    /// are forwarded as required headers per factory.
    #[serde(default)]
    pub fields: std::collections::BTreeMap<String, String>,
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
    /// Phase 82.10.t — model ids parsed from `data[].id` of an
    /// OpenAI-compat `/v1/models` response. `None` when the
    /// provider doesn't expose that shape (Anthropic + Gemini use
    /// distinct endpoints) or the body wasn't parseable. UI falls
    /// back to the static `models` from `llm_providers/catalog`
    /// when this is `None`. Capped at 200 entries to bound the
    /// RPC payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_names: Option<Vec<String>>,
    /// Sanitised error string. Never echoes the API key —
    /// every match of the key value (and its 8-char prefix) is
    /// replaced with `<redacted>` before populating this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// JSON-RPC method that returns the static metadata for every
/// LLM provider the daemon has registered factories for. Operator
/// UIs use this to render a strict provider/model dropdown without
/// keeping their own hardcoded list in sync with the framework.
pub const LLM_PROVIDERS_CATALOG_METHOD: &str = "nexo/admin/llm_providers/catalog";

/// One row of [`LLM_PROVIDERS_CATALOG_METHOD`]'s response.
///
/// NOT marked `#[non_exhaustive]` because `src/main.rs` (and
/// downstream consumers) construct it with literal struct
/// expressions when bridging from `LlmRegistry::catalog()`. New
/// optional fields land additively under `#[serde(default)]` for
/// wire-level back-compat.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmProviderCatalogEntry {
    /// Provider id (matches `llm.yaml.providers.<id>` and the
    /// `crates/llm/<id>.rs` factory's `name()`).
    pub id: String,
    /// Suggested HTTP base URL. Empty when the factory hasn't
    /// declared one.
    pub default_base_url: String,
    /// Conventional env var holding the API key (e.g.
    /// `MINIMAX_API_KEY`). Empty when the factory hasn't declared
    /// one.
    pub default_env_var: String,
    /// Curated list of model ids the provider's factory accepts.
    /// Empty when the factory hasn't declared any — UIs fall back
    /// to a free-text input in that case.
    pub models: Vec<String>,
    /// Phase 82.10.u — declarative credential schema. Empty for
    /// factories that haven't migrated yet (SPA falls back to the
    /// legacy "single api_key field" UI). When non-empty, the SPA
    /// renders one input per descriptor + the upsert handler
    /// validates the operator's payload against this schema.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_schema: Vec<CredentialFieldDescriptor>,
    /// Phase 82.10.u — auth modes the factory supports. Empty
    /// implies `[ApiKey]` (back-compat with pre-82.10.u factories).
    /// When > 1, the SPA renders an `auth_mode` dropdown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_auth_modes: Vec<AuthMode>,
    /// Phase 82.10.u — `true` when the factory exposes an
    /// OpenAI-compat `/v1/models` endpoint that `probe_draft` can
    /// hit to enumerate live models. `false` for Anthropic +
    /// Gemini (their model lists are static and exposed via the
    /// `models` field). When `false`, the SPA skips the "validate
    /// → live models" wizard step and offers the static list
    /// directly.
    #[serde(default)]
    pub supports_models_probe: bool,
}

/// Response shape for [`LLM_PROVIDERS_CATALOG_METHOD`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmProvidersCatalogResponse {
    /// Providers in stable alpha order by id.
    pub providers: Vec<LlmProviderCatalogEntry>,
}

// ──────────────────────────────────────────────────────────────────
// Phase 82.10.u — schema-driven credential descriptors.
//
// Each LLM factory declares the credential fields it accepts. The
// admin RPC catalog surfaces the schema; the SPA wizard renders one
// input per descriptor; the upsert handler validates the operator's
// payload against the schema before touching disk.
// ──────────────────────────────────────────────────────────────────

/// Renderable credential field declared by an `LlmProviderFactory`.
///
/// `secret == true` ⇒ value is persisted to the daemon's
/// SecretsStore (mode 0600 file) and the yaml carries only an
/// `<name>_secret_id` reference. `secret == false` ⇒ value lands
/// inline in `llm.yaml.providers.<id>.<name>`.
///
/// Audit redaction MUST mask every field where `secret == true` —
/// the redactor reads this schema at runtime to decide what to
/// scrub from the audit log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CredentialFieldDescriptor {
    /// Stable machine-readable name. Becomes the yaml key (for
    /// non-secret fields) or the secret id suffix (for secret
    /// fields). Must match `^[a-z][a-z0-9_]{1,40}$`.
    pub name: String,
    /// Operator-facing label. Free-form; the SPA may translate.
    pub label: String,
    /// Renderable input shape — see [`FieldKind`].
    pub kind: FieldKind,
    /// `true` ⇒ admin RPC rejects upsert when this field is missing
    /// (or empty after trim). Subject to [`Self::depends_on`].
    pub required: bool,
    /// `true` ⇒ value is sensitive: persisted to SecretsStore +
    /// redacted in audit logs. `false` ⇒ value is plaintext yaml.
    pub secret: bool,
    /// Default the SPA pre-fills (e.g. `"global"` for MiniMax
    /// region). `None` ⇒ no pre-fill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Operator-facing help text — origin URL, format example, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// Optional shape validation applied client-side AND server-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<FieldValidation>,
    /// Conditional visibility: this field only appears (and is only
    /// validated) when [`DependsOn::satisfied`] returns true against
    /// the upsert payload. Use to hide e.g. `setup_token` unless
    /// `auth_mode = "setup_token"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<DependsOn>,
}

/// What HTML-input shape the SPA should render for this field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldKind {
    /// Plain text input.
    Text,
    /// Password input — masked by default in the SPA. Implies
    /// `secret = true` semantically (audit / persistence) but the
    /// flag remains explicit on [`CredentialFieldDescriptor`] for
    /// clarity.
    Password,
    /// `<select>` with the listed options. SPA pre-fills with the
    /// descriptor's `default` when present.
    Select {
        /// Allowed values + display labels in display order.
        options: Vec<SelectOption>,
    },
}

/// One option inside a [`FieldKind::Select`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelectOption {
    /// Stored value (yaml-side / secret-side).
    pub value: String,
    /// Operator-facing label (i18n-ready).
    pub label: String,
}

/// Optional validation rule applied to a field's value before
/// persistence. The SPA may apply it client-side for instant
/// feedback; the admin RPC handler ALWAYS re-applies it server-side
/// so a custom client cannot smuggle invalid values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldValidation {
    /// Value must match the supplied regex (anchored implicitly —
    /// callers should write `^...$` if they want full-match
    /// semantics; otherwise it's a substring check).
    Regex {
        /// Regex pattern (Rust `regex` crate syntax).
        pattern: String,
        /// Operator-facing hint shown when the regex fails.
        hint: String,
    },
    /// Value length (in unicode chars) must fall within `[min, max]`.
    Length {
        /// Minimum length (inclusive). Use 0 for unbounded below.
        min: usize,
        /// Maximum length (inclusive). Use `usize::MAX` for
        /// unbounded above.
        max: usize,
    },
}

/// Conditional-visibility predicate used by
/// [`CredentialFieldDescriptor::depends_on`].
///
/// The most common case is "this field only matters when
/// `auth_mode` equals one of these values" — see
/// [`DependsOn::any_of`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DependsOn {
    /// Field name in the upsert payload (commonly `"auth_mode"`).
    pub field: String,
    /// Field is satisfied when its value is one of these.
    pub any_of: Vec<String>,
}

impl DependsOn {
    /// Convenience constructor: this descriptor depends on
    /// `field` taking one of `values`.
    pub fn any_of(field: impl Into<String>, values: &[&str]) -> Self {
        Self {
            field: field.into(),
            any_of: values.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Evaluate against an upsert payload. Returns `true` when the
    /// dependency is satisfied (i.e. the field is present AND its
    /// value is in `any_of`). Returns `false` when the field is
    /// absent or holds a value outside the allow-list — the
    /// dependent descriptor is then hidden / skipped.
    pub fn satisfied(&self, fields: &BTreeMap<String, String>) -> bool {
        match fields.get(&self.field) {
            Some(v) => self.any_of.iter().any(|allowed| allowed == v),
            None => false,
        }
    }
}

/// Authentication mode supported by an `LlmProviderFactory`.
///
/// Each factory advertises its supported modes via
/// `LlmProviderCatalogEntry::supported_auth_modes`. The SPA shows a
/// dropdown when more than one is supported; the admin RPC handler
/// rejects upsert + oauth_start with an unsupported mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMode {
    /// Static API key (the legacy default).
    #[serde(rename = "api_key")]
    ApiKey,
    /// Anthropic-style setup-token (`sk-ant-oat01-…`).
    #[serde(rename = "setup_token")]
    SetupToken,
    /// Authorization-code OAuth with PKCE (Anthropic Claude.ai).
    #[serde(rename = "oauth_auth_code")]
    OAuthAuthCode,
    /// Device-code OAuth user-code polling (MiniMax Token Plan).
    #[serde(rename = "oauth_device_code")]
    OAuthDeviceCode,
    /// Operator pastes a pre-existing OAuth bundle JSON.
    #[serde(rename = "oauth_bundle_import")]
    OAuthBundleImport,
}

/// Typed error surfaced by `llm_providers/upsert`,
/// `llm_providers/probe_draft`, `llm_providers/oauth_*` handlers.
///
/// Travels in `AdminRpcError::data` so the SPA can discriminate by
/// `code` and render localised messages without parsing free-form
/// strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "code")]
pub enum LlmProviderError {
    /// Required field absent or empty after trim.
    #[serde(rename = "MISSING_FIELD")]
    MissingField {
        /// Field name from the factory's credential schema.
        field: String,
    },
    /// Field present in payload but absent from the factory's
    /// schema. Defensive: prevents silent typo land.
    #[serde(rename = "UNKNOWN_FIELD")]
    UnknownField {
        /// The unrecognised field name.
        field: String,
    },
    /// Field present + non-empty but failed [`FieldValidation`].
    #[serde(rename = "INVALID_FORMAT")]
    InvalidFormat {
        /// Field name.
        field: String,
        /// Operator-facing hint from the descriptor.
        hint: String,
    },
    /// `auth_mode` not in the factory's `supported_auth_modes`.
    #[serde(rename = "INVALID_AUTH_MODE")]
    InvalidAuthMode {
        /// Factory id.
        factory: String,
        /// The mode the operator supplied.
        mode: String,
    },
    /// OAuth session id has elapsed its TTL.
    #[serde(rename = "SESSION_EXPIRED")]
    SessionExpired,
    /// OAuth session id was never issued or has been consumed.
    #[serde(rename = "SESSION_NOT_FOUND")]
    SessionNotFound,
    /// Upstream `oauth_finish` exchange / poll failed.
    #[serde(rename = "OAUTH_EXCHANGE_FAILED")]
    OAuthExchangeFailed {
        /// HTTP status of the upstream call (0 for transport
        /// errors).
        upstream_status: u16,
        /// Sanitised error body. NEVER contains the OAuth code or
        /// token.
        message: String,
    },
    /// `probe_draft` upstream call failed.
    #[serde(rename = "PROBE_FAILED")]
    ProbeFailed {
        /// HTTP status.
        upstream_status: u16,
        /// Sanitised error body.
        message: String,
    },
    /// Yaml patch step failed mid-upsert. Operator can retry —
    /// secret writes (if any) are idempotent under the same
    /// instance id.
    #[serde(rename = "YAML_WRITE_FAILED")]
    YamlWriteFailed {
        /// Lower-level error detail.
        detail: String,
    },
    /// SecretsStore write step failed mid-upsert.
    #[serde(rename = "SECRET_WRITE_FAILED")]
    SecretWriteFailed {
        /// Lower-level error detail.
        detail: String,
    },
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    #[test]
    fn credential_field_descriptor_round_trip() {
        let d = CredentialFieldDescriptor {
            name: "api_key".into(),
            label: "API key".into(),
            kind: FieldKind::Password,
            required: true,
            secret: true,
            default: None,
            help: Some("sk-…".into()),
            validation: Some(FieldValidation::Length { min: 1, max: 200 }),
            depends_on: None,
        };
        let v = serde_json::to_value(&d).unwrap();
        let back: CredentialFieldDescriptor = serde_json::from_value(v).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn field_kind_select_serializes_with_options() {
        let k = FieldKind::Select {
            options: vec![
                SelectOption {
                    value: "global".into(),
                    label: "Global".into(),
                },
                SelectOption {
                    value: "cn".into(),
                    label: "China".into(),
                },
            ],
        };
        let s = serde_json::to_string(&k).unwrap();
        assert!(s.contains("\"type\":\"select\""));
        assert!(s.contains("\"value\":\"global\""));
        let back: FieldKind = serde_json::from_str(&s).unwrap();
        assert_eq!(k, back);
    }

    #[test]
    fn auth_mode_wire_form_is_lowercase_oauth() {
        // Hand-written wire forms — these strings are part of the
        // public protocol contract; renaming a variant must NOT
        // change them.
        for (mode, wire) in [
            (AuthMode::ApiKey, "\"api_key\""),
            (AuthMode::SetupToken, "\"setup_token\""),
            (AuthMode::OAuthAuthCode, "\"oauth_auth_code\""),
            (AuthMode::OAuthDeviceCode, "\"oauth_device_code\""),
            (AuthMode::OAuthBundleImport, "\"oauth_bundle_import\""),
        ] {
            let s = serde_json::to_string(&mode).unwrap();
            assert_eq!(s, wire);
            let back: AuthMode = serde_json::from_str(&s).unwrap();
            assert_eq!(back, mode);
        }
    }

    #[test]
    fn llm_provider_error_round_trip_typed_data() {
        let e = LlmProviderError::MissingField {
            field: "group_id".into(),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"code\":\"MISSING_FIELD\""));
        assert!(s.contains("\"field\":\"group_id\""));
        let back: LlmProviderError = serde_json::from_str(&s).unwrap();
        assert_eq!(back, e);

        let e = LlmProviderError::OAuthExchangeFailed {
            upstream_status: 401,
            message: "invalid grant".into(),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"code\":\"OAUTH_EXCHANGE_FAILED\""));
        let back: LlmProviderError = serde_json::from_str(&s).unwrap();
        assert_eq!(back, e);
    }

    /// Phase 82.10.u back-compat: old microapps that don't know
    /// about the new `auth_mode` + `fields` keys must still be
    /// able to serialise legacy upserts. The new fields default
    /// to absent on the wire.
    #[test]
    fn upsert_input_legacy_payload_round_trips_without_new_fields() {
        let raw = r#"{"id":"minimax","base_url":"https://x","api_key_env":"K"}"#;
        let i: LlmProviderUpsertInput = serde_json::from_str(raw).unwrap();
        assert_eq!(i.id, "minimax");
        assert!(i.fields.is_empty());
        assert!(i.auth_mode.is_none());
        let back = serde_json::to_string(&i).unwrap();
        assert!(!back.contains("auth_mode"));
        assert!(!back.contains("\"fields\":"));
    }

    /// New `fields` payload survives a full round-trip and the
    /// legacy `api_key_env` stays empty when the operator opts
    /// into the schema-driven path.
    #[test]
    fn upsert_input_schema_payload_round_trip() {
        let mut fields = BTreeMap::new();
        fields.insert("api_key".into(), "sk-test".into());
        fields.insert("group_id".into(), "1234567890123".into());
        fields.insert("region".into(), "global".into());
        let i = LlmProviderUpsertInput {
            id: "minimax-cliente-a".into(),
            base_url: "https://api.minimax.io/v1".into(),
            factory_type: Some("minimax".into()),
            auth_mode: Some(AuthMode::ApiKey),
            fields,
            ..Default::default()
        };
        let s = serde_json::to_string(&i).unwrap();
        assert!(s.contains("\"auth_mode\":\"api_key\""));
        assert!(s.contains("\"fields\":{"));
        let back: LlmProviderUpsertInput = serde_json::from_str(&s).unwrap();
        assert_eq!(back.fields.len(), 3);
        assert_eq!(back.auth_mode, Some(AuthMode::ApiKey));
    }

    /// `LlmProviderCatalogEntry` legacy payloads (without
    /// `credential_schema` + `supported_auth_modes` +
    /// `supports_models_probe`) deserialise cleanly into the
    /// post-82.10.u shape so older operator UIs stay compatible.
    #[test]
    fn catalog_entry_legacy_payload_deserialises_into_82_10_u_shape() {
        let raw = r#"{
            "id": "minimax",
            "default_base_url": "https://api.minimax.io/v1",
            "default_env_var": "MINIMAX_API_KEY",
            "models": ["MiniMax-M2.5"]
        }"#;
        let e: LlmProviderCatalogEntry = serde_json::from_str(raw).unwrap();
        assert!(e.credential_schema.is_empty());
        assert!(e.supported_auth_modes.is_empty());
        assert!(!e.supports_models_probe);
    }

    #[test]
    fn depends_on_satisfied_matches_value_in_allow_list() {
        let d = DependsOn::any_of("auth_mode", &["setup_token", "api_key"]);
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        assert!(!d.satisfied(&fields), "missing key ⇒ unsatisfied");
        fields.insert("auth_mode".into(), "oauth_auth_code".into());
        assert!(!d.satisfied(&fields), "value not in allow-list ⇒ unsatisfied");
        fields.insert("auth_mode".into(), "setup_token".into());
        assert!(d.satisfied(&fields), "value in allow-list ⇒ satisfied");
    }
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
            ..Default::default()
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
            tenant_id: Some("acme".into()),
            ..Default::default()
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
            model_names: Some(vec!["gpt-4o".into()]),
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
