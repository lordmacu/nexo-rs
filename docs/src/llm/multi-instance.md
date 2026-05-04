# Multi-instance providers + secret-backed keys

> Phase 82.10.s ships a long-overdue split between **factory type** (the
> `crates/llm/src/<id>.rs` client the daemon dispatches against) and
> **provider instance** (the YAML key under `llm.yaml.providers.*`).
>
> Phase 82.10.t adds dynamic model discovery via `/v1/models` so SPA
> wizards show the live list a key actually has access to instead of
> a hardcoded catalog that drifts.

## Why

Pre-82.10.s, `providers.minimax` was both the YAML id AND the factory
name — there was exactly one MiniMax per daemon. Two problems:

1. **Two microapps in the same daemon couldn't have separate MiniMax
   keys.** The key was an env var (`MINIMAX_API_KEY`), and env vars are
   process-global. Microapp B would overwrite microapp A's key.
2. **A single tenant couldn't run two MiniMaxes** with different keys
   for billing isolation between their own clients.

Post-82.10.s, the YAML can name as many instances of the same factory
as the operator wants, each with its own key:

```yaml
providers:
  # Legacy single-instance path still works (factory_type omitted →
  # the YAML key IS the factory id).
  minimax:
    api_key: ${MINIMAX_API_KEY}
    base_url: https://api.minimax.chat/v1

  # Multi-instance: name the instance whatever you want, point
  # factory_type at a registered factory, supply a per-instance
  # secret reference instead of a shared env var.
  minimax-cliente-a:
    factory_type: minimax
    base_url: https://api.minimax.chat/v1
    api_key_secret_id: LLM_MINIMAX_CLIENTE_A

  minimax-cliente-b:
    factory_type: minimax
    base_url: https://api.minimax.chat/v1
    api_key_secret_id: LLM_MINIMAX_CLIENTE_B
```

Agents then point at the **instance** id, not the factory:

```yaml
agents:
  ana:
    model:
      provider: minimax-cliente-a   # ← instance id
      model: MiniMax-M2.5
  pedro:
    model:
      provider: minimax-cliente-b   # ← different instance, different key
      model: MiniMax-M2.5
```

Each agent dispatches against its own key. Quota / rate-limit / billing
all separate.

## API key sources — exactly one of three

`LlmProviderConfig` accepts the API key from one of three sources, and
the upsert RPC + boot resolver enforce **exactly one**:

| Source | Where it lives | When to use |
|---|---|---|
| `api_key` (inline) | YAML literal — usually `${ENV_VAR}` | Dev / single-tenant single-instance |
| `api_key_secret_id` | Reference to `<state_root>/secrets/<ID>.txt` mode 0600 | Production multi-instance |
| `api_key_env` (legacy) | Env var name — daemon resolves at boot | Pre-82.10.s back-compat |

Setting two of the above at once → loud boot failure with the
provider id and the conflicting sources listed.

## Boot resolution

After `AppConfig::load`, main.rs walks every provider instance (global
+ tenant-scoped) and:

1. **Resolves `api_key`** via `LlmConfig::resolve_all_keys(&secrets)`.
   - Errors collected per-instance (not fail-fast) so the operator
     sees every broken provider in one diagnostic, not fix-restart-loop.
   - `FsSecretsStore` impls `SecretsSource` (sync `read`) so config-load
     reads `<secrets_dir>/<id>.txt` without async machinery.

2. **Validates `factory_type`** via `LlmRegistry::validate_config`.
   - Each instance's resolved factory id (explicit `factory_type` or
     fallback to the YAML key) MUST be a registered factory.
   - Aggregates errors the same way; loud boot fail beats a runtime
     LLM dispatch error mid-traffic.

Sample boot failure:

```
Error: LLM provider API-key resolution failed for 2 instance(s):
  · minimax-cliente-a: secret 'cliente-a-key' read failed: No such file
  · openai: no API key source (set `api_key` inline or `api_key_secret_id`)
```

## Admin RPC — `nexo/admin/llm_providers/upsert`

The admin handler now accepts:

```jsonc
{
  "id": "minimax-cliente-a",
  "factory_type": "minimax",                  // optional — defaults to id
  "base_url": "https://api.minimax.chat/v1",
  "api_key_secret_value": "sk-...",           // write-through (audit-redacted)
  // mutually exclusive with:
  //   "api_key_env": "MINIMAX_API_KEY"       // legacy
  //   "api_key_secret_id": "PRE_STAGED_ID"   // pre-staged via secrets/write
  "tenant_id": "acme"                         // optional tenant scope
}
```

When `api_key_secret_value` is supplied, the daemon:

1. Stamps the value into the SecretsStore under a derived id
   (`LLM_<INSTANCE_UPPERCASE>`) — atomic file write mode 0600.
2. Sets `api_key_secret_id: LLM_<INSTANCE>` on the YAML.
3. Triggers reload signal so the rebuilt `LlmRegistry` picks up the
   key without daemon restart.

Audit redactor masks `api_key_secret_value` as `<redacted>` so the
cleartext only persists in the SecretsStore, never on disk in
`admin_audit.db`. `api_key_secret_id` (a name, not a value) stays
visible for diagnostics.

## Admin RPC — `nexo/admin/llm_providers/catalog`

Returns the list of registered factories with their default base URL
+ env var + curated model list. SPA wizards use this to render strict
provider/model dropdowns without a hardcoded catalog drifting from
the framework. Plugin-registered remote providers (Phase 81.25)
appear here too as long as they registered before bootstrap.

## Admin RPC — `nexo/admin/llm_providers/probe`

Phase 82.10.t extended the probe response with a `model_names` field
parsed from `data[].id` of an OpenAI-compat `/v1/models` payload:

```jsonc
{
  "ok": true,
  "status": 200,
  "latency_ms": 142,
  "model_count": 47,
  "model_names": ["gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "..."]
}
```

`model_names` is `null` when:
- The provider doesn't expose `/v1/models` (Anthropic, Gemini).
- The body isn't OpenAI-compat shaped.
- No `data[].id` strings could be extracted.

UI fallback in that case: the static factory catalog from
`llm_providers/catalog`. Names are capped at 200 to bound RPC payload
against pathological providers returning thousands of variants.

## Frontend behaviour (agent-creator microapp ≥ 0.0.44)

The Agents page surfaces both flows:

- **Top section** — list of configured LLM instances. "Nueva instancia"
  CTA opens a modal:
  - Factory dropdown (from `llm_providers/catalog`).
  - Instance id (validates slug, rejects duplicates client-side).
  - Base URL auto-filled from the catalog, editable.
  - API key (password input) — write-through via
    `api_key_secret_value`.

- **Edit modal per agent** — provider dropdown lists the configured
  *instances* (`minimax-cliente-a`, `minimax-cliente-b`), not the
  factory types. Model dropdown:
  - Probes the instance's `/v1/models` after open.
  - Live names → green "✓ N modelos en vivo" indicator.
  - Probe failure / non-OpenAI shape → static catalog fallback with
    a hint explaining the provider doesn't expose `/v1/models`.
  - 60 s in-memory cache per instance; concurrent calls deduped.

## Edge cases — defensive design notes

- **Empty `factory_type: ""`** is treated as absent (defensive against
  YAML typos that would otherwise match an empty-string factory).
- **Empty secret value** in the SecretsStore is treated as `NotFound`
  (an operator's `echo "" > file` doesn't half-succeed).
- **Same `factory_type` + same key across instances** is allowed — the
  operator owns fair-share quota when they explicitly clone.
- **Tenant-scoped instance + global instance with same id** —
  Phase 83.8.12 already wins-tenant; this layer doesn't change that.
- **Plugin-registered remote providers** appear in
  `llm_providers/catalog` after their `register` call. The catalogue
  snapshot used by admin RPC is taken at `AdminRpcBootstrap::build`
  time — providers registered after that don't show up until restart.

## Migration from legacy YAML

No migration needed — yamls without `factory_type` keep working under
the back-compat path (instance id IS the factory id). Operators only
touch their YAML when they want a second instance of the same factory
with a different key.
