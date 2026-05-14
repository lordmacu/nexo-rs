# Brainstorm — Runtime Plugin Self-Discovery (Phase 81.33)

**Date:** 2026-05-14
**Status:** brainstorm (pre-spec)
**Trigger:** Phase 81.32 c7.c.1 review surfaced ~30 hardcoded
plugin-name references in `src/main.rs` + `crates/setup/`. The
daemon still imports `nexo_plugin_whatsapp`,
`nexo_plugin_telegram`, `nexo_plugin_email` as library deps just
to call `register_*_tools(...)` + construct typed pairing state.
Goal: framework treats plugins opaquely; plugin self-contains its
outbound tools, pairing flow, and setup hints.

## What the user asked for

> "que esta logica este dentro del mismo plugin"
> "no es buena idea que queemos nunca esto"

Plugin logic lives **inside the plugin**, not in the daemon.
Daemon discovers capabilities at boot via a generic protocol
(subprocess RPC or manifest declaration) and dispatches calls
back to the plugin via the same channel. No daemon-side `if
plugin == "whatsapp"`.

## Inventory of current hardcoded sites

### A. Outbound tool registration (in scope — biggest win)

```text
src/main.rs:5011  plugins.any(|p| p == "memory")    → MemoryTool::new
src/main.rs:5046  plugins.any(|p| p == "whatsapp")  → register_whatsapp_tools(&tools)
src/main.rs:5052  plugins.any(|p| p == "telegram")  → register_telegram_tools(&tools)
src/main.rs:5059  plugins.any(|p| p == "email")     → register_email_tools_filtered(...)
src/main.rs:5181  plugins.any(|p| p == "email")     → Start/Check/CancelFollowup tools
src/main.rs:6777+ same hardcode in spawner closure  (Phase 81.32 c7.c.1)
```

### B. Pairing adapter construction

```text
src/main.rs:6786-6790 (spawner)
crates/core/src/agent/spawn.rs:348-349 (RuntimeAssemblyDeps doc)
  PairingAdapterRegistry.register(WhatsappPairingAdapter::new(broker))
  PairingAdapterRegistry.register(TelegramPairingAdapter::new(broker))
```

### C. Plugin lookup by id

```text
src/main.rs:3362  find(|p| p.manifest.plugin.id == "telegram")
src/main.rs:3439  find(|p| p.manifest.plugin.id == "whatsapp")
```

### D. WhatsApp pairing state (typed Rust)

```text
src/main.rs:710, 1028, 1116, 2379, 3131
  nexo_plugin_whatsapp::pairing::SharedPairingState
  nexo_plugin_whatsapp::pairing::PairingState::new()
  nexo_plugin_whatsapp::pairing::QrSnapshot { ... }
  nexo_plugin_whatsapp::pairing_trigger::CHANNEL_ID
  nexo_plugin_whatsapp::pairing_trigger::WhatsappPairingTrigger
```

### E. Email plugin construction

```text
src/main.rs:715, 3176, 3186, 3657, 3725-3726
  Option<Arc<nexo_plugin_email::EmailPlugin>>
  nexo_plugin_email::EmailPlugin::new(...)
  nexo_plugin_email::EmailToolContext
  nexo_plugin_email::inbound::HealthMap
```

### F. Setup wizard hardcodes

```text
crates/setup/src/lib.rs:641           if svc.id == "telegram" && should_offer_telegram_link()
crates/setup/src/services/channels_dashboard.rs:240   if channel == "telegram"
crates/setup/src/writer.rs:789        nexo_plugin_whatsapp::session::pair_once(...)
crates/setup/src/services/email.rs    (entire file imports nexo_plugin_email::*)
```

## OpenClaw mining (per IRROMPIBLE rule)

### `research/extensions/whatsapp/package.json` — manifest-declared metadata

```json
{
  "openclaw": {
    "extensions": ["./index.ts"],
    "setupEntry": "./setup-entry.ts",
    "channel": {
      "id": "whatsapp",
      "label": "WhatsApp",
      "docsPath": "/channels/whatsapp",
      "persistedAuthState": {
        "specifier": "./auth-presence",
        "exportName": "hasAnyWhatsAppAuth"
      }
    }
  }
}
```

Pattern: **everything that would otherwise be hardcoded in the host
(label, docs path, auth-state check function) is declared in the
plugin's own `package.json`**. The host loads `./auth-presence`
dynamically and calls `hasAnyWhatsAppAuth()`. No host-side switch
on plugin id.

### `research/src/agents/openclaw-plugin-tools.ts:27-50`

```typescript
export function resolveOpenClawPluginToolsForOptions(params: {...}): AnyAgentTool[] {
  const runtimeSnapshot = getActiveSecretsRuntimeSnapshot();
  const pluginTools = resolvePluginTools({
    ...resolveOpenClawPluginToolInputs({...}),
    existingToolNames: params.existingToolNames ?? new Set<string>(),
    toolAllowlist: params.options?.pluginToolAllowlist,
  });
  return applyPluginToolDeliveryDefaults({ tools: pluginTools, deliveryContext });
}
```

**One generic function** asks the plugin runtime for all tools.
Per-plugin behavior is encapsulated inside each plugin's runtime
module — the agent never branches on plugin id. Direct analogue
for our daemon: `register_outbound_tools(plugin_handle, &registry)`
delegating through the plugin's RPC handler.

### `research/src/channels/registry.ts:25-50`

```typescript
function listRegisteredChannelPluginEntries(): RegisteredChannelPluginEntry[] {
  const channelRegistry = getActivePluginChannelRegistryFromState();
  return channelRegistry?.channels ?? [];
}
```

Channel registry is a **list iterated** by the host. Each entry
self-describes (id, aliases, meta). The host's per-channel
behavior reads from the entry, never from a switch on id.

### `research/extensions/telegram/` parallel structure

Same `package.json.openclaw.channel` block declares telegram's
`id`/`label`/`persistedAuthState`/`docsPath` independently. Adding
a new channel (slack, discord) = drop a new extension dir + run
install; host needs zero edits.

## Architectural target

```text
┌─────────────────────────────────────────────────┐
│ daemon (nexo-core, src/main.rs)                  │
│                                                  │
│ for (id, handle) in plugin_handles {            │
│   handle.register_outbound_tools(&tools, broker);│  ← generic
│   if let Some(adapter) = handle.pairing_adapter(│
│           broker.clone()) {                     │
│     pairing_registry.register(adapter);         │  ← generic
│   }                                              │
│ }                                                │
└────────────────────┬────────────────────────────┘
                     │ JSON-RPC over stdio
┌────────────────────▼────────────────────────────┐
│ nexo-plugin-whatsapp / -telegram / -email …      │
│                                                  │
│ • declares tools schema in nexo-plugin.toml      │
│ • subprocess handles `outbound_tool.invoke`      │
│ • subprocess handles `pairing.start/stop/state`  │
│ • exposes `setup_hint` in manifest               │
└──────────────────────────────────────────────────┘
```

Daemon depends on **zero plugin-specific crates** for the runtime
hot path. The only contract: the JSON-RPC wire shape +
manifest schema (`nexo-plugin-manifest`).

## Capability layers + migration phases

### 81.33.a — outbound tools manifest-declared (PoC)

**Manifest extension** (`crates/plugin-manifest/src/manifest.rs`):

```toml
[[plugin.outbound_tools]]
name = "whatsapp_send_message"
description = "Send a WhatsApp text message to a phone number"
input_schema = """{"type":"object","properties":{...},"required":[...]}"""
# RPC method the daemon calls; subprocess implements.
rpc_method = "outbound_tool.invoke"
# Optional: per-call timeout override.
timeout_ms = 30000
```

**Daemon side** (`crates/core/src/agent/nexo_plugin_registry/subprocess.rs`):

```rust
impl NexoPlugin for SubprocessNexoPlugin {
    fn register_outbound_tools(&self, registry: &ToolRegistry) {
        for spec in &self.manifest.plugin.outbound_tools {
            let def = ToolDefinition::from_manifest_spec(spec);
            let handler = GenericRpcToolHandler::new(
                Arc::clone(&self.inner),
                spec.rpc_method.clone(),
                spec.name.clone(),
                spec.timeout_ms,
            );
            registry.register_arc(def, Arc::new(handler));
        }
    }
}
```

`GenericRpcToolHandler::call()` serializes the LLM-supplied
arguments + plugin tool name into a single JSON-RPC request to
the subprocess, awaits the response, returns it to the agent.
The subprocess uses the existing stdio bridge (Phase 81.14.b);
no new transport.

**main.rs side** (boot + spawner):

```rust
let tools = Arc::new(ToolRegistry::new());
tools.register(DelegationTool::tool_def(), DelegationTool);
register_always_on_tools(&tools);  // todo_write, tool_search, ...

for plugin_id in &cfg.plugins {
    if let Some(handle) = plugin_handles.get(plugin_id) {
        handle.register_outbound_tools(&tools);
    }
}
```

Removes the 6 hardcoded `if cfg.plugins.iter().any(|p| p == "X")`
blocks. Daemon's `Cargo.toml` drops `nexo-plugin-whatsapp`,
`-telegram`, `-email` from the dep set (only nexo-pairing +
nexo-plugin-manifest remain).

### 81.33.b — pairing adapter via RPC

**Manifest extension:**

```toml
[plugin.pairing.adapter]
# subprocess handles these RPC methods:
#   pairing.normalize_sender(sender) -> Result<String>
#   pairing.deliver_challenge(challenge_id, sender) -> Result<()>
# Optional: outbound topic override (defaults to plugin.outbound.<id>).
outbound_topic = "plugin.outbound.whatsapp.send"
```

**Daemon side:**

```rust
impl NexoPlugin for SubprocessNexoPlugin {
    fn pairing_adapter(&self, broker: AnyBroker) -> Option<Arc<dyn PairingAdapter>> {
        self.manifest.plugin.pairing.adapter.as_ref().map(|spec| {
            let adapter = GenericRpcPairingAdapter::new(
                self.id().to_string(),
                Arc::clone(&self.inner),
                spec.clone(),
                broker,
            );
            Arc::new(adapter) as Arc<dyn PairingAdapter>
        })
    }
}
```

`GenericRpcPairingAdapter` implements the
`nexo_pairing::PairingAdapter` trait by translating each call
into the matching subprocess RPC method (already defined by the
contract; plugin authors implement on their side).

### 81.33.c — pairing state opaque blob

`SharedPairingState` becomes `serde_json::Value`
keyed by plugin id in a generic `PairingStateRegistry`. Plugin
RPC methods replace the typed accessors:

```
pairing.state.get() -> JsonValue  // plugin's current state
pairing.state.subscribe()         // streams state changes
pairing.state.snapshot()          // QR/code/info snapshot
```

Frontend (admin UI) consumes via the JSON descriptor (Phase 81.30
already manifest-driven on the UI side — backend just plumbs).

### 81.33.d — setup wizard manifest-driven

```toml
[plugin.setup_hint]
suggest_link = true
label = "Configure your Telegram bot"
order = 10
docs_path = "/channels/telegram"
# Optional: subprocess RPC that prompts the operator for credentials.
configure_rpc = "setup.configure"
```

Setup wizard iterates `plugin_handles` + reads each
`manifest.plugin.setup_hint`. Renders the corresponding screen.
Removes the `crates/setup/src/lib.rs:641` hardcode +
`should_offer_telegram_link()` helper.

### 81.33.e — email plugin construction via RPC

The trickiest: `EmailPlugin` currently does TCP probes (IMAP TLS
handshake) at boot. To make this plugin-side, the subprocess must
expose `setup.probe_credentials()` RPC. Daemon delegates the probe
without importing `nexo_plugin_email`. ~1 week of plugin-side
work.

## Migration sequencing (multi-phase)

| Phase | Sub | Scope | Effort | Plugins touched |
|-------|-----|-------|--------|-----------------|
| 81.33 | a | outbound tools manifest+RPC | 1 session | telegram (PoC) |
| 81.33 | b | pairing adapter via RPC | 1 session | telegram + whatsapp |
| 81.34 | a | pairing state opaque blob | 2 sessions | whatsapp |
| 81.34 | b | drop `nexo-plugin-whatsapp` Cargo dep | 1 session | daemon |
| 81.35 | a | setup wizard manifest-driven | 2 sessions | telegram + whatsapp |
| 81.35 | b | drop `nexo-plugin-telegram` Cargo dep | 1 session | daemon |
| 81.36 | a | email setup.probe RPC | 1 week | email |
| 81.36 | b | drop `nexo-plugin-email` Cargo dep | 1 session | daemon |

After 81.36.b, `src/main.rs` imports zero `nexo_plugin_*` crates.
The daemon's plugin surface is the JSON-RPC contract + manifest
schema, period.

## Risks + open questions

- **Tool schemas in TOML** — current `register_whatsapp_tools`
  uses typed Rust structs for arg validation. Migrating to JSON
  schema string in TOML loses compile-time guarantees on the
  plugin side. Mitigation: plugin SDK exposes `tool_def!` macro
  that generates both the TOML manifest entry AND the Rust struct
  for in-plugin validation.
- **Subprocess RPC roundtrip cost on hot path** — every outbound
  tool call now hits stdio JSON-RPC instead of an in-process
  function call. Latency budget: ~1ms per call (existing stdio
  bridge benchmark). Acceptable for outbound flows (network +
  remote API is the bottleneck).
- **In-tree mock plugin** — `crates/core/tests/*` use direct trait
  impls. The default `register_outbound_tools` no-op keeps them
  working; tests gain a `MockPluginWithTools` variant for parity
  coverage.
- **Backwards-compat window** — `register_whatsapp_tools` /
  `register_telegram_tools` etc. stay exported (deprecated) for
  one minor cycle so external embedders can switch.
- **Manifest version bump** — `nexo-plugin-manifest` crate goes
  from v1.5.0 to v1.6.0 (additive only — no breaking changes for
  existing fields). Old plugins (no outbound_tools section)
  continue to work; daemon registers zero outbound tools for them.

## Next: /forge spec

After this brainstorm lands, `/forge spec
runtime-plugin-self-discovery-81.33.a` finalizes the
manifest schema + GenericRpcToolHandler signature + boot wiring
+ telegram PoC migration steps.
