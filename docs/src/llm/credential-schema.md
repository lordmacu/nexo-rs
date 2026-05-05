# Credential schema (Phase 82.10.u)

> Each LLM provider factory declares its credential field schema.
> The admin RPC `llm_providers/catalog` surfaces it; the SPA
> wizard renders one input per descriptor; the upsert handler
> validates the operator's payload against the same schema before
> persistence. Single source of truth — no drift.

## Why

Pre-82.10.u, the operator's `llm_providers/upsert` always boiled
down to a single `api_key`. Two problems:

1. **MiniMax also needs `group_id`** — without it, `/v1/models`
   returns the provider's empty default list and the SPA shows
   nothing. There was no way to surface the field through the
   admin RPC.
2. **Anthropic supports OAuth** — but the wire shape couldn't
   express "auth_mode dropdown + setup_token field that only
   appears when mode=setup_token + bundle JSON paste alternative".

Phase 82.10.u introduces a declarative `CredentialFieldDescriptor`
shape every factory advertises. The SPA renders dynamically. The
handler validates the same schema server-side.

## Schema shape

```rust
pub struct CredentialFieldDescriptor {
    pub name: String,           // yaml key + secret-store id suffix
    pub label: String,          // operator-facing
    pub kind: FieldKind,        // Text | Password | Select { options }
    pub required: bool,
    pub secret: bool,           // → SecretsStore vs yaml inline
    pub default: Option<String>,
    pub help: Option<String>,
    pub validation: Option<FieldValidation>,  // Regex | Length
    pub depends_on: Option<DependsOn>,        // visibility predicate
}
```

### Persistence rule

- `secret == true` → value lands in the SecretsStore under
  derived id `LLM_<INSTANCE>_<FIELD_UPPER>`. Yaml carries only
  `<field>_secret_id` reference.
- `secret == false` → value lands inline in
  `llm.yaml.providers.<id>.<field>`.

### Validation

| Rule | Server check | SPA check |
|---|---|---|
| `required` + `depends_on.satisfied` | `MISSING_FIELD` if absent | red border on blur |
| `Regex { pattern, hint }` | `INVALID_FORMAT` with hint | hint shown inline |
| `Length { min, max }` | `INVALID_FORMAT` length n not in [min,max] | char count |

## Per-factory schemas

| Factory | Fields | Auth modes | `/v1/models`? |
|---|---|---|---|
| `minimax` | api_key (secret) · group_id (10-20 digits) · region (select) · key_kind (api/plan) | api_key, oauth_device_code | ✓ |
| `anthropic` | auth_mode (select) · api_key (depends_on api_key) · setup_token (depends_on setup_token) | api_key, setup_token, oauth_auth_code, oauth_bundle_import | ✗ (static catalog) |
| `openai` / `deepseek` / `gemini` | api_key | api_key | ✓ / ✓ / via Gemini-specific path |

## Wire flow — operator creates a MiniMax instance

```jsonc
// 1) GET catalog
"nexo/admin/llm_providers/catalog" → {
  "providers": [{
    "id": "minimax",
    "credential_schema": [
      {"name":"api_key","kind":{"type":"password"},"required":true,"secret":true,...},
      {"name":"group_id","kind":{"type":"text"},"required":true,"secret":false,
       "validation":{"type":"regex","pattern":"^[0-9]{10,20}$","hint":"10-20 digits"}},
      {"name":"region","kind":{"type":"select","options":[...]},"default":"global"},
      {"name":"key_kind","kind":{"type":"select","options":[...]},"default":"api"}
    ],
    "supported_auth_modes":["api_key","oauth_device_code"],
    "supports_models_probe": true
  }, ...]
}

// 2) Validate without persisting (Phase 82.10.u probe_draft)
"nexo/admin/llm_providers/probe_draft" {
  "factory_type":"minimax",
  "base_url":"https://api.minimax.io/v1",
  "auth_mode":"api_key",
  "fields":{"api_key":"sk-...", "group_id":"1234567890123", "region":"global", "key_kind":"api"}
}
→ { "ok":true, "status":200, "model_count":12, "model_names":[...] }

// 3) Persist
"nexo/admin/llm_providers/upsert" {
  "id":"minimax-cliente-a",
  "factory_type":"minimax",
  "base_url":"https://api.minimax.io/v1",
  "auth_mode":"api_key",
  "fields":{ /* same as probe */ }
}
→ summary
```

## Error taxonomy

`LlmProviderError` rides in `AdminRpcError::data` so the SPA
discriminates by `code`:

```rust
pub enum LlmProviderError {
    MissingField { field },
    UnknownField { field },
    InvalidFormat { field, hint },
    InvalidAuthMode { factory, mode },
    SessionExpired,           // OAuth — TTL elapsed
    SessionNotFound,          // OAuth — never issued or replayed
    OAuthExchangeFailed { upstream_status, message },
    ProbeFailed { upstream_status, message },
    YamlWriteFailed { detail },
    SecretWriteFailed { detail },
}
```

## Audit

Schema-driven payloads are walked by `redact_secret_keys` so any
field whose name matches `api_key`, `setup_token`,
`access_token`, `refresh_token`, `oauth_bundle`, `password`,
`token`, `secret` is masked as `<redacted>`. Non-secret
identifiers (group_id, region, key_kind) stay literal in the
audit log for diagnostics.

## Back-compat

- Pre-82.10.u microapps that send `api_key_env` /
  `api_key_secret_value` keep working — the handler picks the
  legacy path when `fields` is empty.
- Pre-82.10.u daemons that don't carry `credential_schema` in the
  catalog response → SPA falls back to the legacy single-api_key
  UI (default `[]` from the optional `?? []`).
