//! Phase 82.10.f — `nexo/admin/llm_providers/*` handlers.
//!
//! Operates on `llm.yaml.providers.<id>`. Abstracted via
//! [`LlmYamlPatcher`] so this crate stays cycle-free vs
//! `nexo-setup`.

use serde_json::Value;

use async_trait::async_trait;
use nexo_tool_meta::admin::llm_providers::{
    AuthMode, CredentialFieldDescriptor, LlmProviderCatalogEntry, LlmProviderError,
    LlmProviderProbeDraftInput, LlmProviderProbeInput, LlmProviderProbeResponse,
    LlmProviderSummary, LlmProviderUpsertInput, LlmProvidersCatalogResponse,
    LlmProvidersDeleteParams, LlmProvidersDeleteResponse, LlmProvidersListResponse,
    OAuthFinishInput, OAuthFinishResponse, OAuthStartInput, OAuthStartResponse,
};

use super::agents::YamlPatcher;
use super::secrets::SecretsStore;
use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Phase 82.10.u — minimal schema lookup the upsert handler needs to
/// validate operator payloads against the factory the daemon has
/// registered. Production wraps a `nexo_llm::LlmRegistry`; tests can
/// inject an inline mock. Kept small + read-only so the handler
/// stays cycle-free vs `nexo-llm` while still being able to gate
/// every persistence step on the schema.
pub trait FactorySchemaLookup: Send + Sync {
    /// Returns the credential schema for `factory_id`, or `None` when
    /// the factory isn't registered. The handler maps `None` to
    /// `LlmProviderError::InvalidAuthMode { factory: factory_id, mode: "<unknown>" }`
    /// — operator picked a factory the daemon can't instantiate.
    fn credential_schema(
        &self,
        factory_id: &str,
    ) -> Option<Vec<CredentialFieldDescriptor>>;

    /// Returns the auth modes `factory_id` supports. Used to reject
    /// upsert / oauth_start with an unsupported mode early.
    fn supported_auth_modes(&self, factory_id: &str) -> Option<Vec<AuthMode>>;
}

/// Phase 82.10.s.3.b — derive a valid `SecretsStore` name from an
/// LLM provider instance id. The store enforces
/// `^[A-Z][A-Z0-9_]{1,63}$`, but instance ids are lowercase slugs
/// (e.g. `minimax-cliente-a`). Transform: prefix `LLM_`, uppercase,
/// replace any non-alnum with `_`. Keeps the mapping deterministic
/// + reversible for diagnostics.
pub fn secret_id_for_instance(instance_id: &str) -> String {
    let mut out = String::with_capacity(4 + instance_id.len());
    out.push_str("LLM_");
    for c in instance_id.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    out
}

/// Yaml mutation surface for `llm.yaml`. Production wraps an
/// `nexo_setup::yaml_patch`-style adapter pointed at the LLM
/// config file.
pub trait LlmYamlPatcher: Send + Sync {
    /// List provider ids in source order.
    fn list_provider_ids(&self) -> anyhow::Result<Vec<String>>;
    /// Read a dotted field under `providers.<id>.*`.
    fn read_provider_field(
        &self,
        provider_id: &str,
        dotted: &str,
    ) -> anyhow::Result<Option<Value>>;
    /// Upsert a dotted field under `providers.<id>.*`.
    fn upsert_provider_field(
        &self,
        provider_id: &str,
        dotted: &str,
        value: Value,
    ) -> anyhow::Result<()>;
    /// Remove the entire `providers.<id>` block.
    fn remove_provider(&self, provider_id: &str) -> anyhow::Result<()>;

    /// Phase 83.8.12.5.c.b — list provider ids under
    /// `tenants.<tenant_id>.providers`. Empty when the tenant
    /// has no providers block (or the tenant doesn't exist).
    /// Default impl returns empty so legacy patchers without
    /// tenant support compile cleanly while behaving as if no
    /// tenant overrides existed.
    fn list_tenant_provider_ids(
        &self,
        _tenant_id: &str,
    ) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Phase 83.8.12.5.c.b — read a dotted field under
    /// `tenants.<tenant_id>.providers.<provider_id>.*`.
    /// Default impl returns `None`.
    fn read_tenant_provider_field(
        &self,
        _tenant_id: &str,
        _provider_id: &str,
        _dotted: &str,
    ) -> anyhow::Result<Option<Value>> {
        Ok(None)
    }

    /// Phase 83.8.12.5.c.b — upsert a dotted field under
    /// `tenants.<tenant_id>.providers.<provider_id>.*`.
    /// Default impl errors so unimplemented adapters surface
    /// the gap explicitly rather than silently no-op.
    fn upsert_tenant_provider_field(
        &self,
        _tenant_id: &str,
        _provider_id: &str,
        _dotted: &str,
        _value: Value,
    ) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(
            "upsert_tenant_provider_field not implemented for this LlmYamlPatcher"
        ))
    }

    /// Phase 83.8.12.5.c.b — remove the
    /// `tenants.<tenant_id>.providers.<provider_id>` block.
    fn remove_tenant_provider(
        &self,
        _tenant_id: &str,
        _provider_id: &str,
    ) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(
            "remove_tenant_provider not implemented for this LlmYamlPatcher"
        ))
    }
}

/// `nexo/admin/llm_providers/catalog` — return the static metadata
/// for every LLM provider factory the daemon has registered. Pure
/// snapshot read; no yaml access. Operator UIs use this to render a
/// strict provider/model dropdown without keeping their own list.
pub fn catalog(catalog: &[LlmProviderCatalogEntry]) -> AdminRpcResult {
    AdminRpcResult::ok(
        serde_json::to_value(LlmProvidersCatalogResponse {
            providers: catalog.to_vec(),
        })
        .unwrap_or(Value::Null),
    )
}

/// `nexo/admin/llm_providers/list` — return all providers from
/// `llm.yaml`.
pub fn list(patcher: &dyn LlmYamlPatcher) -> AdminRpcResult {
    let ids = match patcher.list_provider_ids() {
        Ok(i) => i,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "llm.yaml read: {e}"
            )));
        }
    };
    let mut providers: Vec<LlmProviderSummary> = ids
        .into_iter()
        .filter_map(|id| read_summary(patcher, &id).ok().flatten())
        .collect();
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    AdminRpcResult::ok(
        serde_json::to_value(LlmProvidersListResponse { providers })
            .unwrap_or(Value::Null),
    )
}

/// `nexo/admin/llm_providers/upsert` — create or update a
/// provider block.
///
/// Two execution paths:
///
/// * **Schema-driven (Phase 82.10.u)**: when `input.fields` is
///   non-empty, the handler resolves the factory's
///   `credential_schema` via `factory_schema`, validates the
///   payload, then persists each field — `secret == true` to the
///   SecretsStore, others inline in yaml. Triggered by Phase 82.10.u
///   microapp wizard.
/// * **Legacy (Phase 82.10.s)**: when `input.fields` is empty,
///   falls back to the api_key_env / api_key_secret_id /
///   api_key_secret_value path (exactly-one-source rule). Existing
///   pre-82.10.u microapps keep working unchanged.
pub async fn upsert(
    patcher: &dyn LlmYamlPatcher,
    secrets: Option<&dyn SecretsStore>,
    factory_schema: Option<&dyn FactorySchemaLookup>,
    params: Value,
    reload_signal: &(dyn Fn() + Send + Sync),
) -> AdminRpcResult {
    let input: LlmProviderUpsertInput = match serde_json::from_value(params) {
        Ok(i) => i,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if input.id.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("id is empty".into()));
    }
    if input.base_url.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("base_url is empty".into()));
    }

    // Phase 82.10.u — branch into the schema-driven path when the
    // operator submitted a `fields` payload. Legacy path stays
    // intact for callers that still use api_key_env etc.
    if !input.fields.is_empty() {
        return upsert_schema_driven(
            patcher,
            secrets,
            factory_schema,
            input,
            reload_signal,
        )
        .await;
    }
    let _ = factory_schema; // silence unused when fields empty

    // Phase 82.10.s.3 — exactly-one-key-source rule. Caller must
    // pick a SINGLE path:
    //   * api_key_env (legacy, deprecated): name of an env var
    //     already exported in the daemon process.
    //   * api_key_secret_id: name of a pre-written secret in the
    //     SecretsStore (write it via secrets/write first).
    //   * api_key_secret_value: write-through (deferred to .3.b).
    // Refuse mixed sources loud — operator intent must be explicit.
    let env_set = !input.api_key_env.is_empty();
    let secret_id_set = input
        .api_key_secret_id
        .as_deref()
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let secret_value_set = input
        .api_key_secret_value
        .as_deref()
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let source_count =
        usize::from(env_set) + usize::from(secret_id_set) + usize::from(secret_value_set);
    if source_count == 0 {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "exactly one API key source required: api_key_env, api_key_secret_id, or api_key_secret_value".into(),
        ));
    }
    if source_count > 1 {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "conflicting API key sources: pick exactly ONE of api_key_env, api_key_secret_id, api_key_secret_value".into(),
        ));
    }
    // Phase 82.10.s.3.b — write-through. Stamp the value into the
    // SecretsStore under a derived id (`LLM_<INSTANCE>`), then on
    // successful persist swap the in-memory input to point at the
    // secret_id so the rest of the handler treats it as the
    // pre-staged path. Atomic ordering: SECRET WRITE FIRST, yaml
    // second — if the yaml write fails the operator can retry
    // with the same value (idempotent overwrite) without leaking
    // a half-written provider block.
    let mut effective_secret_id: Option<String> = input.api_key_secret_id.clone();
    if secret_value_set {
        let store = match secrets {
            Some(s) => s,
            None => {
                return AdminRpcResult::err(AdminRpcError::Internal(
                    "secrets store not configured — cannot write api_key_secret_value".into(),
                ));
            }
        };
        let value = input.api_key_secret_value.as_deref().unwrap_or_default();
        let derived = secret_id_for_instance(&input.id);
        match store.write(&derived, value).await {
            Ok(_) => {
                effective_secret_id = Some(derived);
            }
            Err(e) => return AdminRpcResult::err(e),
        }
    }
    if env_set && std::env::var(&input.api_key_env).is_err() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
            "api_key_env `{}` is not set in process env",
            input.api_key_env
        )));
    }

    // Phase 83.8.12.5.c.b — split the write path: tenant-scoped
    // upserts call `upsert_tenant_provider_field` (writes under
    // `tenants.<id>.providers.<provider_id>.*`); global upserts
    // call the legacy `upsert_provider_field` exactly as before.
    // Tenant id "" treated as None for defense-in-depth (caller
    // bug should not silently write to `tenants..providers.*`).
    let tenant_id = input
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let write_field = |dotted: &str, value: Value| -> anyhow::Result<()> {
        match tenant_id.as_deref() {
            Some(tid) => patcher.upsert_tenant_provider_field(tid, &input.id, dotted, value),
            None => patcher.upsert_provider_field(&input.id, dotted, value),
        }
    };

    if let Err(e) = write_field("base_url", Value::String(input.base_url.clone())) {
        return AdminRpcResult::err(AdminRpcError::Internal(format!(
            "yaml write: {e}"
        )));
    }
    // Phase 82.10.s.3 — persist factory_type when supplied so the
    // registry can split instance-id from factory-id at runtime.
    // Empty / None ⇒ skip the write so legacy yamls stay tidy
    // (factory_type field stays absent, instance id is the
    // factory id by fallback).
    if let Some(ft) = input
        .factory_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Err(e) = write_field("factory_type", Value::String(ft.to_string())) {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml write: {e}"
            )));
        }
    }
    // Phase 82.10.s.3 — write whichever single key source was
    // provided. The pre-validation above guaranteed exactly one is
    // set; we still defensively check before writing each.
    if env_set {
        if let Err(e) =
            write_field("api_key_env", Value::String(input.api_key_env.clone()))
        {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml write: {e}"
            )));
        }
    }
    if let Some(sid) = effective_secret_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Err(e) = write_field("api_key_secret_id", Value::String(sid.to_string())) {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml write: {e}"
            )));
        }
    }
    if !input.headers.is_empty() {
        let map: serde_json::Map<String, Value> = input
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        if let Err(e) = write_field("headers", Value::Object(map)) {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml write: {e}"
            )));
        }
    }
    reload_signal();

    let summary = LlmProviderSummary {
        id: input.id,
        base_url: input.base_url,
        api_key_env: input.api_key_env,
        tenant_scope: tenant_id,
    };
    AdminRpcResult::ok(serde_json::to_value(summary).unwrap_or(Value::Null))
}

/// Phase 82.10.u — schema-driven persistence path. Triggered when
/// `LlmProviderUpsertInput::fields` is non-empty. Validates the
/// payload against the factory's declared
/// [`CredentialFieldDescriptor`] schema before any disk write so
/// the SecretsStore + yaml never end up in a partial state from a
/// bad payload.
///
/// Order of operations:
///
/// 1. Resolve factory id (`factory_type` ?? instance id) +
///    schema lookup. Unknown factory ⇒
///    `LlmProviderError::InvalidAuthMode { mode: "<unknown>" }`.
/// 2. Validate `auth_mode` against
///    `factory.supported_auth_modes()`. Unsupported ⇒
///    `InvalidAuthMode`.
/// 3. Walk the operator's `fields`:
///    a. Each key MUST be in schema (else `UnknownField`).
///    b. If the descriptor has `depends_on` and it isn't
///       satisfied, the field is silently ignored (the SPA
///       shouldn't have shown it; defensive).
/// 4. Walk the schema:
///    a. Required fields whose `depends_on` is satisfied MUST be
///       present + non-empty (else `MissingField`).
///    b. Apply `validation` regex / length (else `InvalidFormat`).
/// 5. Persist:
///    a. `secret == true` fields → SecretsStore under
///       `LLM_<INSTANCE>_<NAME_UPPER>`. yaml gets a
///       `<name>_secret_id` reference.
///    b. `secret == false` fields → yaml inline at
///       `providers.<id>.<name>`.
/// 6. If `auth_mode != Some(ApiKey)` and not `None`, persist
///    yaml `auth.mode = <wire form>`.
/// 7. `reload_signal()`.
async fn upsert_schema_driven(
    patcher: &dyn LlmYamlPatcher,
    secrets: Option<&dyn SecretsStore>,
    factory_schema: Option<&dyn FactorySchemaLookup>,
    input: LlmProviderUpsertInput,
    reload_signal: &(dyn Fn() + Send + Sync),
) -> AdminRpcResult {
    // ── Step 1: resolve factory + schema ─────────────────────────
    let factory_id = input
        .factory_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(input.id.as_str())
        .to_string();
    let lookup = match factory_schema {
        Some(l) => l,
        None => {
            return AdminRpcResult::err(AdminRpcError::Internal(
                "factory schema lookup not configured — cannot honour `fields` payload".into(),
            ));
        }
    };
    let schema = match lookup.credential_schema(&factory_id) {
        Some(s) => s,
        None => {
            return err_typed(LlmProviderError::InvalidAuthMode {
                factory: factory_id.clone(),
                mode: "<unknown factory>".into(),
            });
        }
    };

    // ── Step 2: auth_mode validation ─────────────────────────────
    if let Some(mode) = input.auth_mode {
        let supported = lookup
            .supported_auth_modes(&factory_id)
            .unwrap_or_default();
        if !supported.contains(&mode) {
            return err_typed(LlmProviderError::InvalidAuthMode {
                factory: factory_id.clone(),
                mode: auth_mode_wire(mode).to_string(),
            });
        }
    }

    // ── Step 3: unknown / ignored field check ────────────────────
    for k in input.fields.keys() {
        if !schema.iter().any(|d| &d.name == k) {
            return err_typed(LlmProviderError::UnknownField { field: k.clone() });
        }
    }

    // ── Step 4: required + validation pass ───────────────────────
    for descriptor in &schema {
        let active = descriptor
            .depends_on
            .as_ref()
            .map(|d| d.satisfied(&input.fields))
            .unwrap_or(true);
        let value = input
            .fields
            .get(&descriptor.name)
            .map(String::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if descriptor.required && active && value.is_none() {
            return err_typed(LlmProviderError::MissingField {
                field: descriptor.name.clone(),
            });
        }
        if let (Some(v), Some(rule)) = (value, descriptor.validation.as_ref()) {
            if let Err(hint) = apply_validation(v, rule) {
                return err_typed(LlmProviderError::InvalidFormat {
                    field: descriptor.name.clone(),
                    hint,
                });
            }
        }
    }

    // ── Step 5: persistence ──────────────────────────────────────
    let tenant_id = input
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let write_field = |dotted: &str, value: Value| -> anyhow::Result<()> {
        match tenant_id.as_deref() {
            Some(tid) => patcher.upsert_tenant_provider_field(tid, &input.id, dotted, value),
            None => patcher.upsert_provider_field(&input.id, dotted, value),
        }
    };

    if let Err(e) = write_field("base_url", Value::String(input.base_url.clone())) {
        return err_typed(LlmProviderError::YamlWriteFailed {
            detail: e.to_string(),
        });
    }
    if let Some(ft) = input
        .factory_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Err(e) = write_field("factory_type", Value::String(ft.to_string())) {
            return err_typed(LlmProviderError::YamlWriteFailed {
                detail: e.to_string(),
            });
        }
    }

    // Per-field persistence: secret → SecretsStore + yaml ref.
    // non-secret → yaml inline. depends_on-inactive fields are
    // skipped (SPA shouldn't have submitted them; if it did,
    // dropping them is the safe choice).
    for descriptor in &schema {
        let active = descriptor
            .depends_on
            .as_ref()
            .map(|d| d.satisfied(&input.fields))
            .unwrap_or(true);
        if !active {
            continue;
        }
        let Some(value) = input
            .fields
            .get(&descriptor.name)
            .map(String::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };

        if descriptor.secret {
            let store = match secrets {
                Some(s) => s,
                None => {
                    return AdminRpcResult::err(AdminRpcError::Internal(format!(
                        "secrets store not configured — cannot persist secret field `{}`",
                        descriptor.name
                    )));
                }
            };
            let derived = secret_id_for_field(&input.id, &descriptor.name);
            if let Err(e) = store.write(&derived, value).await {
                return err_typed(LlmProviderError::SecretWriteFailed {
                    detail: e.to_string(),
                });
            }
            let yaml_key = format!("{}_secret_id", descriptor.name);
            if let Err(e) = write_field(&yaml_key, Value::String(derived)) {
                return err_typed(LlmProviderError::YamlWriteFailed {
                    detail: e.to_string(),
                });
            }
        } else if let Err(e) = write_field(&descriptor.name, Value::String(value.to_string())) {
            return err_typed(LlmProviderError::YamlWriteFailed {
                detail: e.to_string(),
            });
        }
    }

    // ── Step 6: persist auth.mode when explicit + non-default ────
    if let Some(mode) = input.auth_mode {
        if mode != AuthMode::ApiKey {
            if let Err(e) = write_field(
                "auth.mode",
                Value::String(auth_mode_wire(mode).to_string()),
            ) {
                return err_typed(LlmProviderError::YamlWriteFailed {
                    detail: e.to_string(),
                });
            }
        }
    }

    // ── Step 7: reload + summary ─────────────────────────────────
    reload_signal();
    let summary = LlmProviderSummary {
        id: input.id,
        base_url: input.base_url,
        api_key_env: String::new(),
        tenant_scope: tenant_id,
    };
    AdminRpcResult::ok(serde_json::to_value(summary).unwrap_or(Value::Null))
}

/// Phase 82.10.u — derive a per-field secret id from
/// `(instance_id, field_name)`. Combines the instance-id transform
/// (`secret_id_for_instance`) with the field name to produce e.g.
/// `LLM_MINIMAX_CLIENTE_A_API_KEY`. Deterministic + reversible.
fn secret_id_for_field(instance_id: &str, field_name: &str) -> String {
    let mut out = secret_id_for_instance(instance_id);
    out.push('_');
    for c in field_name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    out
}

/// Apply a `FieldValidation` rule to a value. Returns the operator-
/// facing hint on failure.
fn apply_validation(
    value: &str,
    rule: &nexo_tool_meta::admin::llm_providers::FieldValidation,
) -> Result<(), String> {
    use nexo_tool_meta::admin::llm_providers::FieldValidation;
    match rule {
        FieldValidation::Regex { pattern, hint } => {
            // Best-effort compile. A bad regex from a factory is a
            // bug; we treat it as "no validation" rather than
            // crashing the dispatch.
            match regex::Regex::new(pattern) {
                Ok(re) if re.is_match(value) => Ok(()),
                Ok(_) => Err(hint.clone()),
                Err(_) => Ok(()),
            }
        }
        FieldValidation::Length { min, max } => {
            let n = value.chars().count();
            if n < *min || n > *max {
                Err(format!("length {n} not in [{min}, {max}]"))
            } else {
                Ok(())
            }
        }
    }
}

/// Wire form for an [`AuthMode`] — must match the
/// `#[serde(rename = "...")]` discriminators in `nexo-tool-meta`.
fn auth_mode_wire(mode: AuthMode) -> &'static str {
    match mode {
        AuthMode::ApiKey => "api_key",
        AuthMode::SetupToken => "setup_token",
        AuthMode::OAuthAuthCode => "oauth_auth_code",
        AuthMode::OAuthDeviceCode => "oauth_device_code",
        AuthMode::OAuthBundleImport => "oauth_bundle_import",
    }
}

/// Build an [`AdminRpcError::InvalidParams`] carrying a typed
/// [`LlmProviderError`] in `data` so the SPA can discriminate by
/// `code` without parsing free-form strings.
fn err_typed(err: LlmProviderError) -> AdminRpcResult {
    let data = serde_json::to_value(&err).unwrap_or(Value::Null);
    let msg = match &err {
        LlmProviderError::MissingField { field } => format!("missing required field `{field}`"),
        LlmProviderError::UnknownField { field } => format!("unknown field `{field}`"),
        LlmProviderError::InvalidFormat { field, hint } => {
            format!("invalid format for `{field}`: {hint}")
        }
        LlmProviderError::InvalidAuthMode { factory, mode } => {
            format!("auth_mode `{mode}` not supported by factory `{factory}`")
        }
        LlmProviderError::SessionExpired => "OAuth session expired".into(),
        LlmProviderError::SessionNotFound => "OAuth session not found".into(),
        LlmProviderError::OAuthExchangeFailed {
            upstream_status,
            message,
        } => format!("OAuth exchange failed (HTTP {upstream_status}): {message}"),
        LlmProviderError::ProbeFailed {
            upstream_status,
            message,
        } => format!("probe failed (HTTP {upstream_status}): {message}"),
        LlmProviderError::YamlWriteFailed { detail } => format!("yaml write failed: {detail}"),
        LlmProviderError::SecretWriteFailed { detail } => format!("secret write failed: {detail}"),
    };
    AdminRpcResult::err(AdminRpcError::InvalidParamsWithData { msg, data })
}

/// `nexo/admin/llm_providers/delete` — remove a provider block.
/// Reject when any agent in `agents.yaml` still references this
/// provider (caller must `agents/upsert` to swap providers first).
pub fn delete(
    llm: &dyn LlmYamlPatcher,
    agents: &dyn YamlPatcher,
    params: Value,
    reload_signal: &dyn Fn(),
) -> AdminRpcResult {
    let p: LlmProvidersDeleteParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    let tenant_id = p
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Refuse when any agent of the SAME scope uses this
    // provider. Cross-scope agents are unaffected — a global
    // delete doesn't break tenant-scoped agents pointing at a
    // tenant-scoped provider with the same id, and vice versa.
    let agent_ids = match agents.list_agent_ids() {
        Ok(ids) => ids,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "agents.yaml read: {e}"
            )));
        }
    };
    for aid in &agent_ids {
        let Some(Value::String(provider)) = agents
            .read_agent_field(aid, "model.provider")
            .ok()
            .flatten()
        else {
            continue;
        };
        if provider != p.provider_id {
            continue;
        }
        // Match agent's tenant_id against the delete scope.
        let agent_tenant = agents
            .read_agent_field(aid, "tenant_id")
            .ok()
            .flatten()
            .and_then(|v| match v {
                Value::String(s) => Some(s),
                _ => None,
            });
        if agent_tenant.as_deref() == tenant_id.as_deref() {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                "provider `{}` still in use by agent `{aid}` (scope: {}); \
                 swap providers via agents/upsert before deleting",
                p.provider_id,
                tenant_id.as_deref().unwrap_or("global"),
            )));
        }
    }

    // Phase 83.8.12.5.c.b — split the delete path mirroring
    // upsert. Tenant scope checks the tenant's provider list +
    // calls `remove_tenant_provider`; global scope keeps the
    // legacy path.
    let existed = match tenant_id.as_deref() {
        Some(tid) => matches!(
            llm.list_tenant_provider_ids(tid),
            Ok(ids) if ids.iter().any(|id| id == &p.provider_id)
        ),
        None => matches!(
            llm.list_provider_ids(),
            Ok(ids) if ids.iter().any(|id| id == &p.provider_id)
        ),
    };
    if !existed {
        return AdminRpcResult::ok(
            serde_json::to_value(LlmProvidersDeleteResponse { removed: false })
                .unwrap_or(Value::Null),
        );
    }
    let removed = match tenant_id.as_deref() {
        Some(tid) => llm.remove_tenant_provider(tid, &p.provider_id),
        None => llm.remove_provider(&p.provider_id),
    };
    match removed {
        Ok(()) => {
            reload_signal();
            AdminRpcResult::ok(
                serde_json::to_value(LlmProvidersDeleteResponse { removed: true })
                    .unwrap_or(Value::Null),
            )
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "yaml remove: {e}"
        ))),
    }
}

/// Phase 82.10.l — daemon-side probe surface.
///
/// Production adapter (`nexo_setup::llm_provider_probe::HttpLlmProviderProbe`)
/// reads the daemon's resolved `llm.yaml` config + env var,
/// hits `GET {base_url}/models` with bearer auth, and returns
/// a sanitised result. Mock impls in tests capture invocations
/// without touching the network.
#[async_trait]
pub trait LlmProvidersProbe: Send + Sync {
    /// Probe a configured provider end-to-end. Returns
    /// `Ok(_)` for every reachable outcome (including 4xx /
    /// 5xx with `ok: false`); returns `Err(_)` only for
    /// pre-flight problems (unknown provider, env var unset)
    /// so the wizard surfaces actionable signals.
    async fn probe(
        &self,
        provider_id: &str,
        tenant_id: Option<&str>,
    ) -> Result<LlmProviderProbeResponse, AdminRpcError>;

    /// Phase 82.10.u — probe a DRAFT payload that hasn't landed in
    /// `llm.yaml` yet. Used by the SPA wizard to validate
    /// credentials BEFORE persistence so a bad key never lands on
    /// disk.
    ///
    /// Default impl returns `Internal "probe_draft not implemented"`
    /// so adapters that haven't migrated keep the legacy probe
    /// path working.
    async fn probe_draft(
        &self,
        _draft: LlmProviderProbeDraftInput,
    ) -> Result<LlmProviderProbeResponse, AdminRpcError> {
        Err(AdminRpcError::Internal(
            "probe_draft not implemented for this LlmProvidersProbe adapter".into(),
        ))
    }
}

/// Dispatcher entry point for `nexo/admin/llm_providers/probe`.
/// Validates the input, then forwards to the configured probe
/// impl. The handler does NOT touch `llm.yaml` itself; the
/// adapter wraps the existing [`LlmYamlPatcher`] for that.
pub async fn probe(
    probe_impl: &dyn LlmProvidersProbe,
    raw_params: Value,
) -> AdminRpcResult {
    match try_probe(probe_impl, raw_params).await {
        Ok(v) => AdminRpcResult::ok(v),
        Err(e) => AdminRpcResult::err(e),
    }
}

async fn try_probe(
    probe_impl: &dyn LlmProvidersProbe,
    raw_params: Value,
) -> Result<Value, AdminRpcError> {
    let input: LlmProviderProbeInput = serde_json::from_value(raw_params)
        .map_err(|e| AdminRpcError::InvalidParams(e.to_string()))?;
    if input.provider_id.is_empty() {
        return Err(AdminRpcError::InvalidParams(
            "provider_id cannot be empty".into(),
        ));
    }
    let response = probe_impl
        .probe(&input.provider_id, input.tenant_id.as_deref())
        .await?;
    serde_json::to_value(response).map_err(|e| AdminRpcError::Internal(e.to_string()))
}

/// Phase 82.10.u — `nexo/admin/llm_providers/probe_draft` handler.
///
/// Forwards a not-yet-persisted credential payload to the probe
/// adapter. Validates the input shape, then defers entirely to
/// `probe_impl.probe_draft(draft).await`. Errors return the same
/// `AdminRpcError` taxonomy as the regular probe — including the
/// `Internal "not implemented"` fallback for adapters that haven't
/// migrated.
pub async fn probe_draft(
    probe_impl: &dyn LlmProvidersProbe,
    raw_params: Value,
) -> AdminRpcResult {
    match try_probe_draft(probe_impl, raw_params).await {
        Ok(v) => AdminRpcResult::ok(v),
        Err(e) => AdminRpcResult::err(e),
    }
}

async fn try_probe_draft(
    probe_impl: &dyn LlmProvidersProbe,
    raw_params: Value,
) -> Result<Value, AdminRpcError> {
    let draft: LlmProviderProbeDraftInput = serde_json::from_value(raw_params)
        .map_err(|e| AdminRpcError::InvalidParams(e.to_string()))?;
    if draft.factory_type.trim().is_empty() {
        return Err(AdminRpcError::InvalidParams(
            "factory_type cannot be empty".into(),
        ));
    }
    if draft.base_url.trim().is_empty() {
        return Err(AdminRpcError::InvalidParams(
            "base_url cannot be empty".into(),
        ));
    }
    let response = probe_impl.probe_draft(draft).await?;
    serde_json::to_value(response).map_err(|e| AdminRpcError::Internal(e.to_string()))
}

// ──────────────────────────────────────────────────────────────────
// Phase 82.10.u — OAuth start/finish handlers.
//
// Two-step flow that suspends PKCE state in `VerifierStore` so the
// SPA never sees the verifier. Single-use sessions: `oauth_finish`
// removes the entry before exchanging the code, so a replay returns
// `SESSION_NOT_FOUND` even if the first call failed mid-exchange.
// TTL 10 min — operators reasonably finish a browser approval in
// under that.
// ──────────────────────────────────────────────────────────────────

const OAUTH_SESSION_TTL_SECS: i64 = 600;

/// `nexo/admin/llm_providers/oauth_start` — generate PKCE, persist
/// the verifier in the store, return `session_id + authorize_url`
/// (auth-code) or `session_id + user_code + verification_uri`
/// (device-code).
pub async fn oauth_start(
    verifier_store: &dyn nexo_llm_auth::VerifierStore,
    raw_params: Value,
) -> AdminRpcResult {
    let input: OAuthStartInput = match serde_json::from_value(raw_params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if input.factory_type.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "factory_type is empty".into(),
        ));
    }

    match (input.factory_type.as_str(), input.auth_mode) {
        ("anthropic", AuthMode::OAuthAuthCode) => {
            oauth_start_anthropic(verifier_store, input).await
        }
        ("minimax", AuthMode::OAuthDeviceCode) => {
            oauth_start_minimax(verifier_store, input).await
        }
        (factory, mode) => err_typed(LlmProviderError::InvalidAuthMode {
            factory: factory.to_string(),
            mode: auth_mode_wire(mode).to_string(),
        }),
    }
}

async fn oauth_start_anthropic(
    verifier_store: &dyn nexo_llm_auth::VerifierStore,
    input: OAuthStartInput,
) -> AdminRpcResult {
    let pkce = nexo_llm_auth::pkce::gen_pkce(nexo_llm_auth::pkce::StateEncoding::HexOnly);
    let authorize_url = nexo_llm_auth::anthropic::build_authorize_url(&pkce);
    let now = unix_now();
    let entry = nexo_llm_auth::VerifierEntry {
        pkce,
        factory_type: "anthropic".to_string(),
        flow_kind: "auth_code".to_string(),
        device_code: None,
        tenant_id: input.tenant_id,
        expires_at_unix: now + OAUTH_SESSION_TTL_SECS,
        created_at_unix: now,
    };
    let session_id = verifier_store.put(entry).await;
    let resp = OAuthStartResponse {
        session_id,
        authorize_url,
        expires_at_ms: (now + OAUTH_SESSION_TTL_SECS) * 1000,
        flow_kind: "auth_code".to_string(),
        user_code: None,
        polling_interval_ms: None,
    };
    AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
}

async fn oauth_start_minimax(
    verifier_store: &dyn nexo_llm_auth::VerifierStore,
    input: OAuthStartInput,
) -> AdminRpcResult {
    let pkce = nexo_llm_auth::pkce::gen_pkce(nexo_llm_auth::pkce::StateEncoding::Base64Url);
    let region = nexo_llm_auth::minimax::Region::Global;
    let device = match nexo_llm_auth::minimax::request_user_code(region, &pkce, None).await
    {
        Ok(d) => d,
        Err(e) => {
            return err_typed(LlmProviderError::OAuthExchangeFailed {
                upstream_status: 0,
                message: format!("request_user_code: {e}"),
            });
        }
    };
    let now = unix_now();
    let entry = nexo_llm_auth::VerifierEntry {
        pkce,
        factory_type: "minimax".to_string(),
        flow_kind: "device_code".to_string(),
        device_code: Some(nexo_llm_auth::DeviceCodeContext {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            deadline_unix: device.deadline_unix,
            interval: device.interval,
        }),
        tenant_id: input.tenant_id,
        expires_at_unix: device.deadline_unix.min(now + OAUTH_SESSION_TTL_SECS),
        created_at_unix: now,
    };
    let session_id = verifier_store.put(entry).await;
    let resp = OAuthStartResponse {
        session_id,
        authorize_url: device.verification_uri,
        expires_at_ms: device.deadline_unix * 1000,
        flow_kind: "device_code".to_string(),
        user_code: Some(device.user_code),
        polling_interval_ms: Some(device.interval.as_millis() as u64),
    };
    AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
}

/// `nexo/admin/llm_providers/oauth_finish` — exchange the code (or
/// poll the device-code endpoint) for an OAuth bundle, persist it
/// to the SecretsStore, patch yaml, and trigger reload. Single-
/// use session: the verifier entry is taken (removed) BEFORE the
/// exchange, so a failed exchange surfaces `SESSION_NOT_FOUND` on
/// retry — the operator must restart with a fresh `oauth_start`.
pub async fn oauth_finish(
    verifier_store: &dyn nexo_llm_auth::VerifierStore,
    secrets: &dyn SecretsStore,
    patcher: &dyn LlmYamlPatcher,
    raw_params: Value,
    reload_signal: &(dyn Fn() + Send + Sync),
) -> AdminRpcResult {
    let input: OAuthFinishInput = match serde_json::from_value(raw_params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if input.session_id.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "session_id is empty".into(),
        ));
    }
    if input.instance_id.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "instance_id is empty".into(),
        ));
    }

    // Single-use: peek_status discriminates expired vs missing
    // for diagnostic-quality error codes; take() removes it.
    use nexo_llm_auth::SessionStatus;
    match verifier_store.peek_status(&input.session_id).await {
        SessionStatus::Live => {}
        SessionStatus::Expired => {
            // Burn the entry so it doesn't linger.
            let _ = verifier_store.take(&input.session_id).await;
            return err_typed(LlmProviderError::SessionExpired);
        }
        SessionStatus::Missing => {
            return err_typed(LlmProviderError::SessionNotFound);
        }
    }
    let entry = match verifier_store.take(&input.session_id).await {
        Some(e) => e,
        None => return err_typed(LlmProviderError::SessionNotFound),
    };

    let bundle = match (entry.factory_type.as_str(), entry.flow_kind.as_str()) {
        ("anthropic", "auth_code") => {
            let code_payload = match input.code.as_deref() {
                Some(c) if !c.trim().is_empty() => c,
                _ => {
                    return AdminRpcResult::err(AdminRpcError::InvalidParams(
                        "auth_code flow requires `code` payload".into(),
                    ));
                }
            };
            let (code, state) = match nexo_llm_auth::pkce::parse_code_payload(code_payload) {
                Ok(t) => t,
                Err(e) => {
                    return err_typed(LlmProviderError::OAuthExchangeFailed {
                        upstream_status: 0,
                        message: format!("parse code: {e}"),
                    });
                }
            };
            match nexo_llm_auth::anthropic::exchange_code(
                &entry.pkce,
                &code,
                &state,
                nexo_llm_auth::anthropic::TOKEN_URL,
            )
            .await
            {
                Ok(b) => b,
                Err(e) => {
                    return err_typed(LlmProviderError::OAuthExchangeFailed {
                        upstream_status: 0,
                        message: e.to_string(),
                    });
                }
            }
        }
        ("minimax", "device_code") => {
            let device_ctx = match entry.device_code.as_ref() {
                Some(d) => d,
                None => {
                    return AdminRpcResult::err(AdminRpcError::Internal(
                        "device_code session missing device_code context".into(),
                    ));
                }
            };
            let device = nexo_llm_auth::minimax::DeviceCodeResponse {
                user_code: device_ctx.user_code.clone(),
                verification_uri: device_ctx.verification_uri.clone(),
                deadline_unix: device_ctx.deadline_unix,
                interval: device_ctx.interval,
            };
            match nexo_llm_auth::minimax::poll_token(
                nexo_llm_auth::minimax::Region::Global,
                &entry.pkce,
                &device,
                None,
            )
            .await
            {
                Ok(b) => b,
                Err(e) => {
                    return err_typed(LlmProviderError::OAuthExchangeFailed {
                        upstream_status: 0,
                        message: e.to_string(),
                    });
                }
            }
        }
        (factory, kind) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "unsupported (factory={factory}, flow_kind={kind})"
            )));
        }
    };

    // Persist bundle JSON to the SecretsStore. Derived id
    // mirrors the Anthropic CLI convention so the runtime
    // `OAuthState::load(path)` can read it back.
    let bundle_json = match bundle.to_json_pretty() {
        Ok(s) => s,
        Err(e) => {
            return err_typed(LlmProviderError::SecretWriteFailed {
                detail: format!("serialise bundle: {e}"),
            });
        }
    };
    let secret_id = oauth_bundle_secret_id(&input.instance_id);
    let bundle_path = match secrets.write(&secret_id, &bundle_json).await {
        Ok(r) => r.path,
        Err(e) => {
            return err_typed(LlmProviderError::SecretWriteFailed {
                detail: e.to_string(),
            });
        }
    };

    // Patch yaml: auth.mode = oauth_bundle, auth.bundle = path.
    let tenant_id = entry.tenant_id.clone();
    let write_field = |dotted: &str, value: Value| -> anyhow::Result<()> {
        match tenant_id.as_deref() {
            Some(tid) => {
                patcher.upsert_tenant_provider_field(tid, &input.instance_id, dotted, value)
            }
            None => patcher.upsert_provider_field(&input.instance_id, dotted, value),
        }
    };
    if let Err(e) = write_field("auth.mode", Value::String("oauth_bundle".into())) {
        return err_typed(LlmProviderError::YamlWriteFailed {
            detail: e.to_string(),
        });
    }
    if let Err(e) = write_field(
        "auth.bundle",
        Value::String(bundle_path.display().to_string()),
    ) {
        return err_typed(LlmProviderError::YamlWriteFailed {
            detail: e.to_string(),
        });
    }
    reload_signal();

    let resp = OAuthFinishResponse {
        ok: true,
        account_email: bundle.account_email.clone(),
        expires_at_ms: bundle.expires_at * 1000,
        secret_id,
    };
    AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
}

/// Derive a SecretsStore name for an OAuth bundle from the
/// instance id. Mirrors `secret_id_for_field` shape: prefix
/// `LLM_`, uppercase, non-alnum → `_`, suffix `_OAUTH_BUNDLE`.
fn oauth_bundle_secret_id(instance_id: &str) -> String {
    let mut out = secret_id_for_instance(instance_id);
    out.push_str("_OAUTH_BUNDLE");
    out
}

fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn read_summary(
    patcher: &dyn LlmYamlPatcher,
    provider_id: &str,
) -> anyhow::Result<Option<LlmProviderSummary>> {
    let base_url = match patcher.read_provider_field(provider_id, "base_url")? {
        Some(Value::String(s)) => s,
        _ => return Ok(None),
    };
    let api_key_env = match patcher.read_provider_field(provider_id, "api_key_env")? {
        Some(Value::String(s)) => s,
        _ => String::new(),
    };
    Ok(Some(LlmProviderSummary {
        id: provider_id.to_string(),
        base_url,
        api_key_env,
        // Phase 83.8.12.5.c — global-scope reads stamp `None`.
        // Tenant-scoped reads (when the handler honours
        // `tenant_id` filter) populate this field with the
        // owning tenant id. Wire-up of the tenant read path
        // lands as a follow-up; today the reader is global
        // only so emitting None here is correct.
        tenant_scope: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockLlm {
        providers: Mutex<HashMap<String, HashMap<String, Value>>>,
        /// Phase 83.8.12.5.c.b — tenant providers keyed by
        /// `(tenant_id, provider_id)`.
        tenant_providers: Mutex<
            HashMap<(String, String), HashMap<String, Value>>,
        >,
    }
    impl MockLlm {
        fn with(provs: &[(&str, &str, &str)]) -> Self {
            let me = Self::default();
            for (id, base_url, env) in provs {
                me.providers
                    .lock()
                    .unwrap()
                    .entry(id.to_string())
                    .or_default()
                    .insert("base_url".into(), Value::String((*base_url).into()));
                me.providers
                    .lock()
                    .unwrap()
                    .entry(id.to_string())
                    .or_default()
                    .insert("api_key_env".into(), Value::String((*env).into()));
            }
            me
        }
    }
    impl LlmYamlPatcher for MockLlm {
        fn list_provider_ids(&self) -> anyhow::Result<Vec<String>> {
            let mut v: Vec<String> = self.providers.lock().unwrap().keys().cloned().collect();
            v.sort();
            Ok(v)
        }
        fn read_provider_field(
            &self,
            id: &str,
            dotted: &str,
        ) -> anyhow::Result<Option<Value>> {
            Ok(self
                .providers
                .lock()
                .unwrap()
                .get(id)
                .and_then(|m| m.get(dotted).cloned()))
        }
        fn upsert_provider_field(
            &self,
            id: &str,
            dotted: &str,
            value: Value,
        ) -> anyhow::Result<()> {
            self.providers
                .lock()
                .unwrap()
                .entry(id.to_string())
                .or_default()
                .insert(dotted.to_string(), value);
            Ok(())
        }
        fn remove_provider(&self, id: &str) -> anyhow::Result<()> {
            self.providers.lock().unwrap().remove(id);
            Ok(())
        }
        fn list_tenant_provider_ids(
            &self,
            tenant_id: &str,
        ) -> anyhow::Result<Vec<String>> {
            let mut v: Vec<String> = self
                .tenant_providers
                .lock()
                .unwrap()
                .keys()
                .filter(|(t, _)| t == tenant_id)
                .map(|(_, p)| p.clone())
                .collect();
            v.sort();
            Ok(v)
        }
        fn read_tenant_provider_field(
            &self,
            tenant_id: &str,
            provider_id: &str,
            dotted: &str,
        ) -> anyhow::Result<Option<Value>> {
            Ok(self
                .tenant_providers
                .lock()
                .unwrap()
                .get(&(tenant_id.to_string(), provider_id.to_string()))
                .and_then(|m| m.get(dotted).cloned()))
        }
        fn upsert_tenant_provider_field(
            &self,
            tenant_id: &str,
            provider_id: &str,
            dotted: &str,
            value: Value,
        ) -> anyhow::Result<()> {
            self.tenant_providers
                .lock()
                .unwrap()
                .entry((tenant_id.to_string(), provider_id.to_string()))
                .or_default()
                .insert(dotted.to_string(), value);
            Ok(())
        }
        fn remove_tenant_provider(
            &self,
            tenant_id: &str,
            provider_id: &str,
        ) -> anyhow::Result<()> {
            self.tenant_providers
                .lock()
                .unwrap()
                .remove(&(tenant_id.to_string(), provider_id.to_string()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct MockAgents {
        agents: Mutex<HashMap<String, HashMap<String, Value>>>,
    }
    impl MockAgents {
        fn with_provider(provider: &str) -> Self {
            let me = Self::default();
            me.agents
                .lock()
                .unwrap()
                .entry("ana".into())
                .or_default()
                .insert("model.provider".into(), Value::String(provider.into()));
            me
        }
    }
    impl YamlPatcher for MockAgents {
        fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> {
            Ok(self.agents.lock().unwrap().keys().cloned().collect())
        }
        fn read_agent_field(
            &self,
            id: &str,
            dotted: &str,
        ) -> anyhow::Result<Option<Value>> {
            Ok(self
                .agents
                .lock()
                .unwrap()
                .get(id)
                .and_then(|m| m.get(dotted).cloned()))
        }
        fn upsert_agent_field(
            &self,
            _id: &str,
            _dotted: &str,
            _value: Value,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        fn remove_agent(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn llm_providers_list_returns_alpha_order() {
        let llm = MockLlm::with(&[
            ("zphi", "u1", "K1"),
            ("anthropic", "u2", "K2"),
            ("minimax", "u3", "K3"),
        ]);
        let result = list(&llm);
        let response: LlmProvidersListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.providers.len(), 3);
        assert_eq!(response.providers[0].id, "anthropic");
        assert_eq!(response.providers[1].id, "minimax");
        assert_eq!(response.providers[2].id, "zphi");
    }

    #[tokio::test]
    async fn llm_providers_upsert_validates_env_var_exists() {
        let llm = MockLlm::default();
        // Use an env var guaranteed not present.
        let result = upsert(
            &llm,
            None,
            None,
            serde_json::json!({
                "id": "newp",
                "base_url": "https://x",
                "api_key_env": "NEXO_DEFINITELY_NOT_SET_VAR_42"
            }),
            &|| {},
        )
        .await;
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn llm_providers_upsert_writes_when_env_var_present() {
        let llm = MockLlm::default();
        // PATH is always set.
        let result = upsert(
            &llm,
            None,
            None,
            serde_json::json!({
                "id": "newp",
                "base_url": "https://x",
                "api_key_env": "PATH"
            }),
            &|| {},
        )
        .await;
        assert!(result.result.is_some());
        // Stored.
        assert_eq!(llm.list_provider_ids().unwrap(), vec!["newp".to_string()]);
    }

    #[test]
    fn llm_providers_delete_rejects_when_agent_uses_provider() {
        let llm = MockLlm::with(&[("minimax", "u", "K")]);
        let agents = MockAgents::with_provider("minimax");
        let result = delete(
            &llm,
            &agents,
            serde_json::json!({ "provider_id": "minimax" }),
            &|| {},
        );
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
        // Provider still present.
        assert!(llm.list_provider_ids().unwrap().contains(&"minimax".into()));
    }

    #[test]
    fn llm_providers_delete_removes_unused_provider() {
        let llm = MockLlm::with(&[("retired", "u", "K")]);
        let agents = MockAgents::with_provider("minimax"); // not retired
        let result = delete(
            &llm,
            &agents,
            serde_json::json!({ "provider_id": "retired" }),
            &|| {},
        );
        let response: LlmProvidersDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.removed);
        assert!(llm.list_provider_ids().unwrap().is_empty());
    }

    #[test]
    fn llm_providers_delete_unknown_id_idempotent() {
        let llm = MockLlm::default();
        let agents = MockAgents::default();
        let result = delete(
            &llm,
            &agents,
            serde_json::json!({ "provider_id": "ghost" }),
            &|| {},
        );
        let response: LlmProvidersDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(!response.removed);
    }

    // ── Phase 83.8.12.5.c.b — tenant-scoped CRUD ──

    /// `tenant_id: Some` upsert writes under tenant namespace
    /// + leaves the global table untouched. Resulting summary
    /// echoes `tenant_scope`.
    #[tokio::test]
    async fn llm_providers_upsert_with_tenant_id_writes_under_tenant_namespace() {
        // Make sure the env var the handler validates exists.
        std::env::set_var("MINIMAX_KEY_ACME_TEST", "value");
        let llm = MockLlm::default();
        let result = upsert(
            &llm,
            None,
            None,
            serde_json::json!({
                "id": "minimax",
                "base_url": "https://api.minimax.io",
                "api_key_env": "MINIMAX_KEY_ACME_TEST",
                "tenant_id": "acme",
            }),
            &|| {},
        )
        .await;
        assert!(result.error.is_none(), "{result:?}");
        let summary: LlmProviderSummary =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(summary.tenant_scope.as_deref(), Some("acme"));
        // Global table untouched.
        assert!(llm.list_provider_ids().unwrap().is_empty());
        // Tenant namespace populated.
        assert_eq!(
            llm.list_tenant_provider_ids("acme").unwrap(),
            vec!["minimax".to_string()]
        );
        let v = llm
            .read_tenant_provider_field("acme", "minimax", "base_url")
            .unwrap();
        assert_eq!(v, Some(Value::String("https://api.minimax.io".into())));
    }

    /// Tenant delete removes ONLY the tenant-scoped block. A
    /// global provider with the same id stays intact (the two
    /// scopes are independent).
    #[tokio::test]
    async fn llm_providers_delete_with_tenant_id_isolates_from_global() {
        std::env::set_var("MINIMAX_KEY", "v");
        // Seed both: a global "minimax" + a tenant "acme.minimax".
        let llm = MockLlm::with(&[("minimax", "https://global", "MINIMAX_KEY")]);
        llm.upsert_tenant_provider_field(
            "acme",
            "minimax",
            "base_url",
            Value::String("https://acme".into()),
        )
        .unwrap();
        let agents = MockAgents::default(); // no agents reference these
        let result = delete(
            &llm,
            &agents,
            serde_json::json!({ "provider_id": "minimax", "tenant_id": "acme" }),
            &|| {},
        );
        let response: LlmProvidersDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.removed);
        // Global still there.
        assert!(llm.list_provider_ids().unwrap().contains(&"minimax".into()));
        // Tenant scope cleared.
        assert!(llm.list_tenant_provider_ids("acme").unwrap().is_empty());
    }

    /// Tenant delete refuses when an agent OF THE SAME tenant
    /// scope still uses the provider.
    #[tokio::test]
    async fn llm_providers_delete_tenant_rejects_when_same_scope_agent_uses_it() {
        let llm = MockLlm::default();
        llm.upsert_tenant_provider_field(
            "acme",
            "minimax",
            "base_url",
            Value::String("https://acme".into()),
        )
        .unwrap();
        let agents = MockAgents::default();
        // Seed an agent in tenant `acme` using minimax.
        agents
            .agents
            .lock()
            .unwrap()
            .entry("ana".into())
            .or_default()
            .insert("model.provider".into(), Value::String("minimax".into()));
        agents
            .agents
            .lock()
            .unwrap()
            .entry("ana".into())
            .or_default()
            .insert("tenant_id".into(), Value::String("acme".into()));
        let result = delete(
            &llm,
            &agents,
            serde_json::json!({ "provider_id": "minimax", "tenant_id": "acme" }),
            &|| {},
        );
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
        // Tenant block still present.
        assert_eq!(
            llm.list_tenant_provider_ids("acme").unwrap(),
            vec!["minimax".to_string()]
        );
    }

    /// Cross-scope delete: a global delete does NOT block on
    /// tenant-scoped agents using a same-named tenant
    /// provider. Tenant agent still has its own provider; the
    /// global delete is allowed.
    #[tokio::test]
    async fn llm_providers_delete_global_unaffected_by_tenant_agent_using_same_id() {
        let llm = MockLlm::with(&[("minimax", "u", "K")]);
        llm.upsert_tenant_provider_field(
            "acme",
            "minimax",
            "base_url",
            Value::String("https://acme".into()),
        )
        .unwrap();
        let agents = MockAgents::default();
        // Tenant `acme` agent uses minimax — but the delete
        // targets the GLOBAL minimax, not acme's.
        agents
            .agents
            .lock()
            .unwrap()
            .entry("ana".into())
            .or_default()
            .insert("model.provider".into(), Value::String("minimax".into()));
        agents
            .agents
            .lock()
            .unwrap()
            .entry("ana".into())
            .or_default()
            .insert("tenant_id".into(), Value::String("acme".into()));
        let result = delete(
            &llm,
            &agents,
            // No tenant_id → global scope.
            serde_json::json!({ "provider_id": "minimax" }),
            &|| {},
        );
        let response: LlmProvidersDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.removed);
        // Global minimax gone, tenant minimax intact.
        assert!(llm.list_provider_ids().unwrap().is_empty());
        assert_eq!(
            llm.list_tenant_provider_ids("acme").unwrap(),
            vec!["minimax".to_string()]
        );
    }

    // ────────────────────────────────────────────────────────
    // Phase 82.10.l — probe handler tests.
    // ────────────────────────────────────────────────────────

    /// Mock that captures invocations + returns a canned
    /// `LlmProviderProbeResponse` (or `Err`).
    struct MockProbe {
        result: std::sync::Mutex<Result<LlmProviderProbeResponse, AdminRpcError>>,
        calls: tokio::sync::Mutex<Vec<(String, Option<String>)>>,
    }

    impl MockProbe {
        fn ok(response: LlmProviderProbeResponse) -> Self {
            Self {
                result: std::sync::Mutex::new(Ok(response)),
                calls: tokio::sync::Mutex::new(Vec::new()),
            }
        }

        fn err(e: AdminRpcError) -> Self {
            Self {
                result: std::sync::Mutex::new(Err(e)),
                calls: tokio::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmProvidersProbe for MockProbe {
        async fn probe(
            &self,
            provider_id: &str,
            tenant_id: Option<&str>,
        ) -> Result<LlmProviderProbeResponse, AdminRpcError> {
            self.calls
                .lock()
                .await
                .push((provider_id.to_string(), tenant_id.map(String::from)));
            let mut guard = self.result.lock().unwrap();
            // Replace the stored Result with a fresh
            // placeholder so we can move out of it without
            // requiring `Clone` on AdminRpcError.
            let placeholder = Ok(LlmProviderProbeResponse::default());
            std::mem::replace(&mut *guard, placeholder)
        }
    }

    #[tokio::test]
    async fn probe_validates_empty_provider_id() {
        let mock = MockProbe::ok(LlmProviderProbeResponse::default());
        let result = probe(&mock, serde_json::json!({"provider_id": ""})).await;
        let err = result.error.expect("expected error");
        match err {
            AdminRpcError::InvalidParams(msg) => assert!(msg.contains("empty")),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
        assert!(mock.calls.lock().await.is_empty());
    }

    #[tokio::test]
    async fn probe_propagates_store_error() {
        let mock = MockProbe::err(AdminRpcError::InvalidParams(
            "env var FOO not set".into(),
        ));
        let result = probe(&mock, serde_json::json!({"provider_id": "minimax"})).await;
        let err = result.error.expect("expected error");
        match err {
            AdminRpcError::InvalidParams(msg) => assert!(msg.contains("env var")),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_returns_response_on_success() {
        let mock = MockProbe::ok(LlmProviderProbeResponse {
            ok: true,
            status: 200,
            latency_ms: 142,
            model_count: Some(5),
            model_names: None,
            error: None,
        });
        let result = probe(
            &mock,
            serde_json::json!({"provider_id": "minimax", "tenant_id": "acme"}),
        )
        .await;
        let value = result.result.expect("ok");
        assert_eq!(value["ok"], true);
        assert_eq!(value["status"], 200);
        assert_eq!(value["model_count"], 5);
        let calls = mock.calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "minimax");
        assert_eq!(calls[0].1.as_deref(), Some("acme"));
    }

    // ── Phase 82.10.u — schema-driven upsert ────────────────────

    use nexo_tool_meta::admin::llm_providers::{
        AuthMode, CredentialFieldDescriptor, FieldKind, FieldValidation,
    };

    /// In-memory `FactorySchemaLookup` mock: holds one factory's
    /// schema + auth modes. Convenient for the schema-driven
    /// upsert tests below.
    struct MockSchema {
        factory_id: String,
        schema: Vec<CredentialFieldDescriptor>,
        modes: Vec<AuthMode>,
    }
    impl FactorySchemaLookup for MockSchema {
        fn credential_schema(
            &self,
            factory_id: &str,
        ) -> Option<Vec<CredentialFieldDescriptor>> {
            (factory_id == self.factory_id).then(|| self.schema.clone())
        }
        fn supported_auth_modes(&self, factory_id: &str) -> Option<Vec<AuthMode>> {
            (factory_id == self.factory_id).then(|| self.modes.clone())
        }
    }

    fn minimax_schema() -> MockSchema {
        MockSchema {
            factory_id: "minimax".into(),
            schema: vec![
                CredentialFieldDescriptor {
                    name: "api_key".into(),
                    label: "API key".into(),
                    kind: FieldKind::Password,
                    required: true,
                    secret: true,
                    default: None,
                    help: None,
                    validation: Some(FieldValidation::Length { min: 1, max: 200 }),
                    depends_on: None,
                },
                CredentialFieldDescriptor {
                    name: "group_id".into(),
                    label: "Group ID".into(),
                    kind: FieldKind::Text,
                    required: true,
                    secret: false,
                    default: None,
                    help: None,
                    validation: Some(FieldValidation::Regex {
                        pattern: "^[0-9]{10,20}$".into(),
                        hint: "10-20 digits".into(),
                    }),
                    depends_on: None,
                },
            ],
            modes: vec![AuthMode::ApiKey, AuthMode::OAuthDeviceCode],
        }
    }

    /// In-memory `SecretsStore` mock: records every write so the
    /// test can assert what landed where.
    struct MockSecrets {
        writes: Mutex<Vec<(String, String)>>,
    }
    #[async_trait]
    impl SecretsStore for MockSecrets {
        async fn write(
            &self,
            name: &str,
            value: &str,
        ) -> Result<
            nexo_tool_meta::admin::secrets::SecretsWriteResponse,
            AdminRpcError,
        > {
            self.writes
                .lock()
                .unwrap()
                .push((name.to_string(), value.to_string()));
            Ok(nexo_tool_meta::admin::secrets::SecretsWriteResponse {
                path: std::path::PathBuf::from(format!("/mock/{name}.txt")),
                overwrote_env: false,
            })
        }
    }

    /// Happy path — full minimax payload validates, persists the
    /// secret api_key under a derived id, writes group_id inline,
    /// and reload_signal fires once.
    #[tokio::test]
    async fn schema_driven_upsert_persists_secret_and_yaml() {
        let llm = MockLlm::default();
        let secrets = MockSecrets {
            writes: Mutex::new(Vec::new()),
        };
        let schema = minimax_schema();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let result = upsert(
            &llm,
            Some(&secrets),
            Some(&schema),
            serde_json::json!({
                "id": "minimax-cliente-a",
                "factory_type": "minimax",
                "base_url": "https://api.minimax.io/v1",
                "auth_mode": "api_key",
                "fields": {
                    "api_key": "sk-test-key",
                    "group_id": "1234567890123",
                },
            }),
            &|| {
                calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            },
        )
        .await;
        assert!(result.error.is_none(), "{result:?}");
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "reload_signal must fire exactly once"
        );
        // Secret persisted under derived id with the field name suffix.
        let writes = secrets.writes.lock().unwrap().clone();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, "LLM_MINIMAX_CLIENTE_A_API_KEY");
        assert_eq!(writes[0].1, "sk-test-key");
        // Yaml has factory_type + group_id inline + api_key_secret_id ref.
        let providers = llm.providers.lock().unwrap();
        let entry = providers.get("minimax-cliente-a").unwrap();
        assert_eq!(
            entry.get("factory_type"),
            Some(&Value::String("minimax".into()))
        );
        assert_eq!(
            entry.get("group_id"),
            Some(&Value::String("1234567890123".into()))
        );
        assert_eq!(
            entry.get("api_key_secret_id"),
            Some(&Value::String("LLM_MINIMAX_CLIENTE_A_API_KEY".into()))
        );
    }

    /// Required field absent → MissingField error code, no
    /// side-effect on disk.
    #[tokio::test]
    async fn schema_driven_upsert_rejects_missing_required_field() {
        let llm = MockLlm::default();
        let secrets = MockSecrets {
            writes: Mutex::new(Vec::new()),
        };
        let schema = minimax_schema();
        let result = upsert(
            &llm,
            Some(&secrets),
            Some(&schema),
            serde_json::json!({
                "id": "minimax-cliente-b",
                "factory_type": "minimax",
                "base_url": "https://x",
                "fields": { "api_key": "sk-x" },  // group_id missing
            }),
            &|| {},
        )
        .await;
        let err = result.error.expect("MissingField error");
        let data = err.data().expect("typed data");
        assert_eq!(data["code"], "MISSING_FIELD");
        assert_eq!(data["field"], "group_id");
        // Nothing written.
        assert!(secrets.writes.lock().unwrap().is_empty());
        assert!(llm.list_provider_ids().unwrap().is_empty());
    }

    /// Field present but failing the regex validation → InvalidFormat.
    #[tokio::test]
    async fn schema_driven_upsert_rejects_invalid_format() {
        let llm = MockLlm::default();
        let secrets = MockSecrets {
            writes: Mutex::new(Vec::new()),
        };
        let schema = minimax_schema();
        let result = upsert(
            &llm,
            Some(&secrets),
            Some(&schema),
            serde_json::json!({
                "id": "minimax-cliente-c",
                "factory_type": "minimax",
                "base_url": "https://x",
                "fields": {
                    "api_key": "sk-x",
                    "group_id": "not-numeric",  // regex fails
                },
            }),
            &|| {},
        )
        .await;
        let err = result.error.expect("InvalidFormat error");
        let data = err.data().expect("typed data");
        assert_eq!(data["code"], "INVALID_FORMAT");
        assert_eq!(data["field"], "group_id");
        assert!(secrets.writes.lock().unwrap().is_empty());
    }

    /// Field outside the schema → UnknownField. Defensive against
    /// payload typos.
    #[tokio::test]
    async fn schema_driven_upsert_rejects_unknown_field() {
        let llm = MockLlm::default();
        let secrets = MockSecrets {
            writes: Mutex::new(Vec::new()),
        };
        let schema = minimax_schema();
        let result = upsert(
            &llm,
            Some(&secrets),
            Some(&schema),
            serde_json::json!({
                "id": "minimax-cliente-d",
                "factory_type": "minimax",
                "base_url": "https://x",
                "fields": {
                    "api_key": "sk-x",
                    "group_id": "1234567890",
                    "garbage": "what",
                },
            }),
            &|| {},
        )
        .await;
        let err = result.error.expect("UnknownField error");
        let data = err.data().expect("typed data");
        assert_eq!(data["code"], "UNKNOWN_FIELD");
    }

    /// Factory id absent from the lookup → InvalidAuthMode with
    /// `<unknown factory>`. Tests the case where the operator
    /// supplies a factory the daemon doesn't have registered.
    #[tokio::test]
    async fn schema_driven_upsert_rejects_unknown_factory() {
        let llm = MockLlm::default();
        let secrets = MockSecrets {
            writes: Mutex::new(Vec::new()),
        };
        let schema = minimax_schema();
        let result = upsert(
            &llm,
            Some(&secrets),
            Some(&schema),
            serde_json::json!({
                "id": "ghost-instance",
                "factory_type": "ghost",
                "base_url": "https://x",
                "fields": { "api_key": "sk-x" },
            }),
            &|| {},
        )
        .await;
        let err = result.error.expect("InvalidAuthMode error");
        let data = err.data().expect("typed data");
        assert_eq!(data["code"], "INVALID_AUTH_MODE");
    }

    /// Legacy path: empty `fields` → falls back to `api_key_env`
    /// branch, preserves pre-82.10.u behaviour for existing
    /// microapps.
    #[tokio::test]
    async fn schema_driven_upsert_falls_back_to_legacy_when_fields_empty() {
        let llm = MockLlm::default();
        // Schema absent — the legacy path doesn't need a lookup.
        let result = upsert(
            &llm,
            None,
            None,
            serde_json::json!({
                "id": "legacy-minimax",
                "base_url": "https://x",
                "api_key_env": "PATH",  // PATH always set
            }),
            &|| {},
        )
        .await;
        assert!(result.error.is_none(), "legacy path must still work");
    }
}
