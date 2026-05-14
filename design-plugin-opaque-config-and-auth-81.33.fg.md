# Brainstorm — Plugin Opaque Config + Generic Credential Store (Phase 81.33.f+g)

**Date:** 2026-05-14
**Status:** brainstorm (pre-spec)
**Trigger:** Phase 81.33.a step 5 + 81.33.b.real both blocked
on the same architectural debt: daemon imports
`nexo-plugin-{whatsapp,telegram,email}` because `cfg.plugins.X`
is a typed Rust field per plugin AND `bundle.stores.X` /
`nexo_auth::handle::X` are typed credential accessors keyed by
plugin name as a const string. Until both are opaque
(plugin-id-keyed maps), the daemon can't be plugin-agnostic.

## What the user asked for

> "no es buena idea que queemos nunca esto"
> "que esta logica este dentro del mismo plugin"
> "para que el framework no dependa de configuracion de plugins
> no que esta logica este dentro del mismo plugin"

Framework treats plugin config + credentials as opaque blobs
keyed by plugin id. Plugin owns the schema, validation, and
runtime interpretation.

## Inventory of typed-plugin coupling

### F — typed `cfg.plugins.X` fields

```rust
// crates/config/src/types/plugins.rs (generated impl)
pub struct PluginsConfig {
    pub telegram: Vec<TelegramPluginConfig>,
    pub whatsapp: Vec<WhatsappPluginConfig>,
    pub email: Option<EmailPluginConfig>,
    pub browser: Option<BrowserPluginConfig>,
    pub google: Option<GooglePluginConfig>,
    // ...
}
```

Read sites:
- `src/main.rs:3374` `cfg.plugins.telegram` (boot subprocess factory)
- `src/main.rs:3453` `cfg.plugins.whatsapp` (boot subprocess factory)
- `src/main.rs:3176` `nexo_plugin_email::EmailPlugin::new(...)`
- `src/main.rs:6738` `cfg.plugins.telegram.clone()` (spawner closure capture)
- `src/main.rs:9069` migration reader `telegram.yaml`
- `src/main.rs:15791` boot diagnostic `cfg.plugins.telegram.iter().enumerate()`
- `crates/setup/src/services/*` setup wizard per-channel branches

Each typed config has its own Serde derive + YAML round-trip
tests. Adding a new plugin = editing `PluginsConfig` struct +
serializer + every downstream reader.

### G — typed credential store accessors

```rust
// crates/auth/src/wire.rs
pub struct CredentialsBundle {
    pub stores: CredentialStores,
    pub resolver: Arc<AgentCredentialResolver>,
    pub breakers: Arc<BreakerRegistry>,
}

pub struct CredentialStores {
    pub whatsapp: WhatsappStore,
    pub telegram: TelegramStore,
    pub email: EmailStore,
    pub google: GoogleStore,
}

// crates/auth/src/handle.rs
pub const TELEGRAM: &str = "telegram";
pub const WHATSAPP: &str = "whatsapp";
pub const EMAIL: &str = "email";
pub const GOOGLE: &str = "google";
```

Read sites:
- `src/main.rs:2099` `bundle.stores.telegram.list().len()` (diagnostic)
- Telegram outbound `publish_outbound` resolves via
  `ctx.credentials.resolve(&agent_id, nexo_auth::handle::TELEGRAM)`
- `crates/auth/src/audit.rs::audit_outbound(&handle, ...)` —
  per-handle audit channel
- `crates/auth/src/telemetry.rs::inc_usage(handle::TELEGRAM, ...)` —
  per-handle telemetry
- Breaker registry uses handle.account_id_raw() for keying

Each store has its own DB schema + accessor methods. New plugin
= new store type + new const + new resolver branch.

## OpenClaw mining (per IRROMPIBLE rule)

### `research/src/config/channel-configured-shared.ts:5-12` — opaque-by-id pattern

```typescript
export function resolveChannelConfigRecord(
  cfg: OpenClawConfig,
  channelId: string,
): Record<string, unknown> | null {
  const channels = cfg.channels as Record<string, unknown> | undefined;
  const entry = channels?.[channelId];
  return isRecord(entry) ? entry : null;
}
```

OpenClaw's `cfg.channels: Record<channelId, unknown>` is the
direct analogue of what we want for `cfg.plugins`. The host
treats each entry as `unknown` (TypeScript) / `serde_json::Value`
(Rust); per-channel typed interpretation lives inside the plugin
that consumes it.

### `research/src/secrets/runtime-config-collectors-channels.ts` — per-channel collector registry

Per-channel secret collection logic lives in **per-plugin
modules** registered against a `Map<channelId, Collector>`. The
host calls `collectors.get(channelId)?.run(env)` generically. No
host-side switch on `channelId === "telegram"`.

This is the direct analogue for what we'd want for credential
store accessors: `Map<plugin_id, CredentialStore>` keyed by
plugin id, plugins register their store at boot.

### `research/src/secrets/configure.ts:..` — bundled schema map

`getBundledChannelConfigSchemaMap()` returns
`Map<channelId, JsonSchema>` built from the bundled-channel
metadata generator. Host validates each cfg entry against its
declared schema generically; per-channel validation logic stays
inside the plugin's module.

Direct analogue: each plugin's `nexo-plugin.toml` declares a
`config_schema` (JSON schema string, see Phase 81.33.a outbound
pattern); daemon validates `cfg.plugins[plugin_id]` against the
declared schema; ZERO daemon-side typed configs.

## Architectural target

### F: opaque config

```rust
// crates/config/src/types/plugins.rs
pub struct PluginsConfig {
    /// Per-plugin opaque config. Key = plugin id (matches
    /// manifest.plugin.id); value = whatever JSON shape the
    /// plugin declares in `[plugin.config_schema]`.
    pub entries: std::collections::BTreeMap<String, serde_yaml::Value>,
}
```

YAML wire:

```yaml
plugins:
  telegram:
    # Plugin-defined schema. Multi-instance plugins (telegram /
    # whatsapp) keep the `Vec<...>` shape; single-instance
    # plugins use a map / object. The daemon doesn't care.
    - instance: "main"
      bot_token_env: "TELEGRAM_BOT_TOKEN_MAIN"
    - instance: "secondary"
      bot_token_env: "TELEGRAM_BOT_TOKEN_SECONDARY"
  whatsapp:
    - instance: "personal"
      session_dir: "/var/lib/nexo/wa/personal"
      enabled: true
  email:
    imap_host: "imap.gmail.com"
    smtp_host: "smtp.gmail.com"
    username_env: "GMAIL_USER"
```

Daemon boot path:

```rust
for (plugin_id, value) in &cfg.plugins.entries {
    if let Some(handle) = plugin_handles.get(plugin_id) {
        handle.configure(value).await?;
    }
}
```

`NexoPlugin::configure(&self, value: &serde_yaml::Value) ->
Result<(), PluginConfigError>` is a new trait method
(default no-op). `SubprocessNexoPlugin::configure` ships the
value via JSON-RPC `plugin.configure { value }` to the
subprocess; the plugin's own code deserialises against its
declared schema.

### G: opaque credential store

```rust
// crates/auth/src/store_generic.rs
pub struct CredentialStores {
    /// Per-plugin store. Key = plugin id; value = a trait object
    /// the plugin's `Arc<dyn NexoPlugin>::credential_store()`
    /// returns at boot.
    stores: DashMap<String, Arc<dyn GenericCredentialStore>>,
}

#[async_trait]
pub trait GenericCredentialStore: Send + Sync {
    /// List configured account ids.
    async fn list(&self) -> Vec<String>;
    /// Resolve raw credential bytes for `account_id`. Plugin
    /// owns interpretation.
    async fn resolve(&self, account_id: &str) -> Result<CredentialBytes, CredentialError>;
    /// Hot-reload hook.
    async fn reload(&self) -> Result<(), CredentialError>;
}
```

`bundle.stores.telegram.list()` becomes
`bundle.stores.get("telegram").map(|s| s.list().await).unwrap_or_default()`.

Resolver lookup:

```rust
// Was:
ctx.credentials.resolve(&agent_id, nexo_auth::handle::TELEGRAM)?

// Becomes:
ctx.credentials.resolve(&agent_id, plugin_id)?
```

Per-handle audit + telemetry + breaker stay keyed by the
plugin_id string instead of typed const. Function signatures
stay; only the constants disappear.

## Capability layers + migration phases

| Sub | Scope | Effort | Blocks |
|-----|-------|--------|--------|
| 81.33.f.1 | `[plugin.config_schema]` manifest section + parser | 1 session | f.2 |
| 81.33.f.2 | `NexoPlugin::configure(value)` trait + RPC method | 1 session | f.3 |
| 81.33.f.3 | `PluginsConfig.entries: BTreeMap` parallel field | 1 session | f.4 |
| 81.33.f.4 | Plugins migrate their typed config readers to opaque | 2-3 sessions/plugin (× 5 plugins) | f.5 |
| 81.33.f.5 | Drop typed `cfg.plugins.X` fields | 1 session | g.1 |
| 81.33.g.1 | `GenericCredentialStore` trait | 1 session | g.2 |
| 81.33.g.2 | `bundle.stores: DashMap<id, …>` parallel field | 1 session | g.3 |
| 81.33.g.3 | Plugins migrate their typed stores | 1-2 sessions/plugin (× 4 plugins) | g.4 |
| 81.33.g.4 | Drop typed store fields + `nexo_auth::handle::*` constants | 1 session | A finalize |

**Total realistic estimate:** 15-25 sessions across 3-5 weeks
calendar time, plus 4-5 plugin patch publishes.

## Risks

- **Config schema migration:** existing `agents.yaml` files have
  the typed shape baked in. Migration must be backward-compat:
  daemon parses BOTH old typed fields AND new `entries` map
  during a deprecation window, prefers `entries` when present,
  warns when typed fields are used. After 2-3 release cycles
  drop the typed fields.
- **JSON schema in TOML strings**: see Phase 81.33.a brainstorm.
  Plugin SDK macro generates both TOML + Rust struct for the
  plugin author.
- **Credential store backward-compat:** `bundle.stores.telegram`
  is the LIBRARY-level API consumed by every outbound tool +
  every audit hook + every breaker call. Mass call-site refactor
  AND every plugin crate's outbound code updates.
- **Trait object credential resolver:** `resolve()` returns
  `CredentialBytes` (raw); each plugin reinterprets. Type-erased
  surface loses the compile-time guarantee that a tool's
  expected credential type matches what the resolver returns.
  Mitigation: plugin-side helper `CredentialBytes::deserialize::<TelegramHandle>()`
  with proper error mapping.
- **Pure refactor with no user-visible feature:** 25-session
  refactor with zero new feature = high opportunity cost vs
  shipping new plugins / channels. Sequencing AFTER the
  microapp roadmap stabilises is probably correct.
- **Plugin authors discover their config is wire-defined:**
  current `TelegramPluginConfig` struct lives in the daemon-side
  config crate; the plugin authors don't own it. New shape means
  plugin authors own their config — better separation but
  requires SDK + tooling support.

## Should we do this NOW?

**Honest take:** the trigger ("daemon hardcodes plugin names")
exists, but the cure (25-session refactor) is disproportionate
to the symptom unless one of the following holds:

1. We're about to ship 3+ new plugins (slack, discord, sms) in
   quick succession and each requires daemon-side typed config
   + store edits.
2. External community-tier plugins are blocked because the
   daemon refuses to load anything outside the typed plugin
   set.
3. The microapp / customer roadmap depends on plugin discovery
   that the typed coupling prevents.

If none of those hold, the right move is:
- Document this brainstorm.
- Ship the helper consolidations (81.33.b, .e — already done).
- Defer the full refactor to a future quarter where the
  trade-off justifies the investment.

## Next: /forge spec only when triggered

Spec + plan land when a concrete need surfaces (a new plugin
that doesn't fit the typed shape, an external request, a
performance issue, etc.). Until then this brainstorm sits
on disk as the authoritative design doc.
