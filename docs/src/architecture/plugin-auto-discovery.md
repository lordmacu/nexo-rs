# Plugin Auto-Discovery — Design Memo

Status: **design memo** (no code change). Produced 2026-05-15
to anchor the next 2-4 sessions of work toward the goal:

> Adding a new plugin to nexo should be a drop-in operation.
> The operator places the plugin binary and its manifest in
> `plugins.discovery.search_paths` and the daemon picks up
> EVERY capability the plugin declares — outbound tools,
> credentials, HTTP routes, pairing flows, dashboard surface,
> per-instance orchestration — without any daemon-side code
> change.

## Reference mining

OpenClaw (`/home/familia/chat/research/`):

- `src/channels/plugins/types.plugin.ts:47-96` — `ChannelPlugin`
  declarative top: `id`, `meta`, `capabilities`, `gatewayMethods`,
  `configSchema`, `reload`.
- `src/channels/plugins/types.adapters.ts:76-858` — imperative
  handler split (`gateway.startAccount` / `pairing` /
  `auth.login` / `outbound.send*` / `messaging.normalizeTarget`
  / `directory.self` / `lifecycle.onAccountConfigChanged`).
- `src/channels/plugins/types.core.ts:100` — `webhookPath` as
  per-account declarative HTTP mount point.
- `src/gateway/server-channels.ts:285-449` — daemon-managed
  per-account lifecycle loop (`AbortController`, exponential
  backoff 5s→5min, ≤10 retries, status snapshot).
- `src/plugins/inspect-shape.ts:36-127` — runtime introspection
  classifies plugins as
  `plain-capability / hybrid-capability / hook-only / non-capability`
  by counting `channelIds`, `providerIds`, `gatewayMethodCount`,
  `httpRouteCount`.
- `docs/channels/pairing.md:41-49` — pairing state lives in
  `~/.openclaw/credentials/<channel>-pairing.json` +
  `<channel>-allowFrom.json`; `pairing` adapter is the only
  per-channel custom logic surface.

`claude-code-leak/` ausente en `/home/familia/chat/`. Mining
absence declared explicitly.

Current Rust shape (`crates/core/src/agent/plugin_host.rs`):

- L66-199 — `NexoPlugin` trait. Already has the auto-discovery
  shape: `manifest()`, `init(&ctx)`, `shutdown()`,
  `build_pairing_adapter(broker)`, `register_outbound_tools(&reg)`,
  `configure(&yaml)`, `credential_store()`, `as_any()`. Defaults
  let new plugins opt in.
- `PluginInitContext` (L204-300+) — hands plugins `tool_registry`,
  `advisor_registry`, `hook_registry`, `broker`, `llm_registry`,
  `reload_coord`, `sessions`, `long_term_memory`, `shutdown`,
  `channel_adapter_registry`, `plugin_config`. Plenty of
  extension points already.

The trait + context is **already mostly self-describing**. What's
missing is daemon-side **dispatch** — code in `src/main.rs` that
iterates `plugin_handles` instead of hardcoding per-plugin
blocks.

## Inventory of 12 capability layers

| Layer | Auto-discoverable today? | What blocks it |
|---|---|---|
| Config schema | ✅ done (Phase 93.1-93.4) | — |
| Manifest discovery | ✅ done | — |
| Subprocess lifecycle | ✅ done | — |
| Broker RPC integration | ✅ done | — |
| Credential store | ✅ done (Phase 93.6-93.9) | — |
| Outbound tools | ✅ partial | Phase 81.32.c7.c — daemon-side hardcoded fallbacks (`register_whatsapp_tools` etc.) coexist with trait method. |
| **Pairing adapter** | ❌ | Phase 81.33.b.real — trait method exists but no daemon dispatch; subprocess plugins can't supply Rust trait obj across process boundary. |
| **HTTP routes** | ❌ | Daemon hardcodes `/whatsapp/pair`. No trait method for plugins to declare routes. |
| **Admin RPC commands** | ❌ partial | Daemon hardcodes `with_wa_bot_handle`. No generic admin-RPC registration. |
| **Channel dashboard** | ✅ partial (Phase 93.10) | `ChannelDashboardSource` lives in `nexo-setup`, NOT exposed via `NexoPlugin` trait. Plugins can't auto-register a dashboard surface. |
| **Metrics / health endpoints** | ❌ partial | Daemon hardcodes `/email/health`, `/metrics` whatsapp-instances JSON. |
| **Orchestration** | ❌ | Phase 93.5.d — daemon hardcodes whatsapp instance loop, tunnel auto-open, pairing-state map. |

Seven layers need work to reach "drop plugin → daemon discovers
everything".

## Architectural principles (non-negotiable)

1. **Manifest is the single source of truth.** Anything the daemon
   needs to know about a plugin is in `nexo-plugin.toml`. Daemon
   never inspects plugin Cargo features, plugin source code, or
   plugin runtime state to discover capabilities.
2. **Subprocess boundary is honoured.** Rust trait objects do not
   cross process boundaries. Anywhere the daemon would need to
   call into the plugin per-message, the dispatch goes through
   **broker JSON-RPC** (with caches at hot paths).
3. **In-tree plugins use trait dispatch; subprocess plugins use
   broker dispatch.** `NexoPlugin` trait methods stay valid for
   in-tree plugins (Phase 81.20 candidates). For subprocess
   plugins, `SubprocessNexoPlugin` translates trait calls into
   broker RPCs against a generic adapter constructed from
   manifest data.
4. **Per-channel custom logic stays in the plugin process.**
   `normalize_sender`, `auth_check`, instance-discovery — every
   per-channel rule executes inside the subprocess, never in the
   daemon. Daemon stays generic.
5. **Hardcoded canonical-plugin paths are deprecation-tracked,
   not deleted opportunistically.** Out-of-tree plugin crates
   ship on their own release cadence; daemon ships fallbacks
   until canonical plugins opt into the generic path via their
   own next manifest revision.

## Patterns

Two patterns repeat across all 7 remaining layers. Pin them once
in the framework; reuse for each layer.

### Pattern A: broker-RPC dispatch with cache

For per-event hot paths that need plugin-side logic.

Manifest declares the broker topic shape:

```toml
[plugin.pairing.adapter]
channel_id = "whatsapp"
broker_topic_prefix = "plugin.whatsapp"
# daemon will call: <broker_topic_prefix>.pairing.normalize_sender
#                   <broker_topic_prefix>.pairing.send_reply
#                   <broker_topic_prefix>.pairing.send_qr_image
```

Daemon-side adapter:

```rust
pub struct GenericBrokerPairingAdapter {
    channel_id: &'static str,
    broker: AnyBroker,
    topic_prefix: String,
    // Cache: raw sender → normalized form. Pairing volume is
    // low; cache grows bounded by unique senders.
    normalize_cache: Arc<RwLock<HashMap<String, Option<String>>>>,
}
```

- `normalize_sender(raw)` checks cache, on miss does
  `broker.request("<prefix>.pairing.normalize_sender", raw)` with
  a short timeout, then caches result.
- `send_reply`/`send_qr_image` are already async — direct broker
  RPC.

**Trade-off.** First-sighting of every sender pays a broker
round-trip (~1-5ms local). Subsequent lookups are O(1) cache. For
pairing flows, this is acceptable because handshakes are rare.
For high-throughput hot paths (every inbound message), upfront
broadcast-of-known-normalizations would be required — design
that into the manifest as a separate batch RPC if a layer needs
it.

**Sync trait → async broker.** `PairingChannelAdapter::normalize_sender`
is `fn` sync. The generic adapter uses `tokio::runtime::Handle::block_on`
inside an inherent async-block-on-cache-miss helper, OR the trait
gets migrated to `async fn` first (preferred if downstream
callers are already in async contexts).

### Pattern B: declarative interpreter

For boot-time setup that needs plugin-side logic but only fires
once per startup or per config-reload.

Manifest declares the data; daemon interprets:

```toml
[plugin.orchestration]
per_instance_state = true            # daemon allocates a state-map keyed by instance
public_tunnel.enabled = true         # daemon offers an auto-tunnel knob
public_tunnel.route = "/whatsapp/pair"  # daemon mounts the tunneled prefix
inbound_state_topic = "plugin.inbound.whatsapp"  # daemon subscribes here for state events

[plugin.http]
mount_prefix = "/whatsapp"            # daemon mounts a proxy under this prefix
# requests get forwarded via broker as:
#   plugin.<id>.http.<method>.<path-encoded>
```

Daemon iterates `plugin_handles.iter().filter_map(|h| h.manifest().http.as_ref())`
and mounts proxies generically.

**Trade-off.** The manifest schema enumerates known orchestration
shapes — adding a NEW shape (e.g. "websocket pairing" vs current
"HTTP-poll-for-QR") requires extending the schema. That's a
breaking change to the manifest contract, NOT to daemon code.
Plugin authors get a compile-time deserialization error pointing
at the missing field. Schema evolution is centralized + versioned.

## Per-layer design

### Layer 6 — outbound tools (already partially generic)

**Today.** `NexoPlugin::register_outbound_tools(&registry)` trait
method exists with default no-op. Plugins like
`nexo-plugin-whatsapp` override to call `register_whatsapp_tools(&tools)`.
Daemon ALSO has hardcoded fallbacks (`src/main.rs:5128, 6917`)
gated on `cfg.plugins.iter().any(|p| p == "whatsapp")` — these
fire IN ADDITION TO the trait method, scoped by feature gate.

**To close (Phase 81.32.c7.c).** Remove hardcoded fallbacks once
canonical plugins ship a manifest declaring
`[[plugin.tools.outbound]]` per tool. Daemon iterates
`plugin_handles[..].register_outbound_tools(&registry)` only;
delete the fallback `if plugin == "whatsapp"` blocks.

**Effort.** ~3-4h. Touches `main.rs` boot loop + hot-spawn loop.
Plugin crates must publish a release with the manifest section
first — coordinate via release notes.

### Layer 7 — pairing adapter (Phase 81.33.b.real)

**Today.** `NexoPlugin::build_pairing_adapter(broker)` trait method
exists with default `None`. Daemon hardcodes
`build_known_pairing_registry()` (`src/main.rs:1651-1660`) that
constructs whatsapp + telegram adapters by Rust type, both
cfg-gated.

**To close.** Pattern A. New manifest section
`[plugin.pairing.adapter]` (channel_id, broker_topic_prefix).
`GenericBrokerPairingAdapter` in `nexo-pairing` reads manifest
+ owns cache. `SubprocessNexoPlugin::build_pairing_adapter()`
returns `Some(Arc::new(GenericBrokerPairingAdapter::from_manifest(self.manifest(), broker)))`
when manifest declares the section, else `None`.

Daemon `build_known_pairing_registry` becomes a loop:

```rust
for handle in &plugin_handles {
    if let Some(adapter) = handle.build_pairing_adapter(broker.clone()) {
        registry.register(adapter);
    }
}
```

Canonical plugins (whatsapp, telegram) ship next manifest
revision adding the section + handle the broker RPCs in their
subprocess. Until then daemon falls back to legacy hardcoded
registrations (already cfg-gated).

**Trade-off accepted.** `normalize_sender` cache miss = one
broker round-trip per unique sender. Pairing flows are low
volume; cost is invisible in practice.

**Effort.** ~5h: manifest schema + adapter impl + subprocess
plugin RPC handler stubs + integration test.

### Layer 8 — HTTP routes

**Today.** Daemon `run_health_server` (`src/main.rs:~15140+`)
hardcodes `/whatsapp/*` route handler using
`nexo_plugin_whatsapp::pairing::dispatch_route`. Email and other
channels with HTTP needs would each add hardcoded blocks.

**To close.** Pattern B. New manifest section:

```toml
[plugin.http]
mount_prefix = "/whatsapp"
# daemon forwards every request under this prefix via broker
```

Daemon-side proxy: a single generic `handle_plugin_http_route`
function that matches `request.path` against registered
prefixes, then issues a broker RPC
`plugin.<id>.http.request` with serialized request bundle.
Plugin subprocess implements its own internal router under that
prefix.

**Trade-off.** Every HTTP request to a plugin pays a broker
round-trip (~1-2ms local) + serialization. For human-facing
pages (pairing QR, OAuth callbacks) this is invisible. For
machine-to-machine high-throughput webhooks, consider whether
the plugin should listen on its own port directly (avoid the
proxy entirely) and only register a "I have a port" descriptor
for the dashboard. Add a `mount_kind: "proxy" | "direct"` knob
in the manifest section if needed.

**Effort.** ~6h: manifest schema + daemon proxy handler +
broker RPC contract + subprocess router scaffolding +
integration test (round-trip a pairing GET through the proxy).

### Layer 9 — admin RPC commands

**Today.** Setup wizard's admin RPC dispatcher
(`crates/setup/src/admin_bootstrap.rs:712`) hardcodes
`.with_wa_bot_handle(Arc::new(WhatsappBotHandle))`. Only
whatsapp currently has plugin-specific admin commands but the
pattern extrapolates poorly.

**To close.** Pattern A (broker-RPC) for admin command dispatch.
Manifest section:

```toml
[[plugin.admin.command]]
namespace = "whatsapp"     # admin RPC method prefix
methods = ["pair_start", "pair_status", "pair_revoke", "bot_status"]
```

Daemon's admin dispatcher iterates registered plugin admin
namespaces; on `admin.<namespace>.<method>` call, forwards via
broker to plugin subprocess.

Removes `WhatsappBotHandle` typed integration entirely. Other
plugins (telegram bot-info, email account-info) auto-declare
their own admin namespaces.

**Effort.** ~5h: manifest schema + admin dispatcher generic
routing + broker RPC contract + remove `with_wa_bot_handle` +
integration test.

### Layer 10 — channel dashboard (Phase 93.10 polish)

**Today.** Phase 93.10 shipped `ChannelDashboardSource` trait
in `nexo-setup` with 3 hardcoded canonical impls. New canonical
channel = new impl in `nexo-setup` = framework code change.

**To close.** Pattern B. Move `ChannelDashboardSource` data
into manifest:

```toml
[plugin.dashboard]
auth_check_kind = "file_presence"     # | "session_dir_with_files" | "broker_probe"
auth_check_args = { path = "telegram_bot_token.txt" }
multi_instance_layout = "single"      # | "workspace_walk" | "broker_list"
```

Daemon-side generic interpreter reads the section + dispatches
to the matching auth-check / instance-discovery handler. For
shapes the interpreter doesn't recognise (rare), fall back to a
broker RPC `plugin.<id>.dashboard.discover` that the subprocess
implements.

**Trade-off.** Schema enumerates known auth-check + layout
shapes. A 5th channel with a wholly new auth shape (e.g.
OAuth-token-presence-with-refresh-due-check) requires extending
the enumeration. This is the SAME trade-off as Pattern B
elsewhere: schema evolution > framework code change.

Move the 3 canonical sources from `nexo-setup` to manifest
data on the canonical plugin crates (next release each).

**Effort.** ~4h: interpreter + manifest schema + migrate
3 canonical impls + integration test.

### Layer 11 — metrics / health endpoints

**Today.** Daemon hardcodes `/email/health`, `/metrics`
whatsapp-instances JSON output, etc.

**To close.** Pattern B + Pattern A combined. Manifest declares
which metrics surfaces a plugin owns:

```toml
[plugin.metrics]
prometheus = true                  # daemon scrapes plugin's broker RPC
health_endpoint = "/email/health"  # exposed as proxy
```

`/metrics` aggregator on daemon already collects from registered
sources. Add a generic `BrokerScrapeSource` that issues
`plugin.<id>.metrics.scrape` per scrape interval, parses
Prometheus text response, merges into aggregate.

**Trade-off.** Per-scrape broker RPC cost (~1ms × number of
plugins, ≤10ms total at typical scale). Cache-with-TTL if scrape
is high-frequency.

**Effort.** ~4h.

### Layer 12 — orchestration (Phase 93.5.d)

**Today.** Daemon hardcodes whatsapp orchestration in
`src/main.rs` (instance loop L3219+, tunnel auto-open L3833+,
pairing-state subscriber spawn L3608+).

**To close.** Pattern B with the orchestration schema:

```toml
[plugin.orchestration]
per_instance_state = true
inbound_state_topic = "plugin.inbound.whatsapp"
inbound_state_events = ["connected", "disconnected", "reconnecting", "qr"]

[plugin.orchestration.public_tunnel]
offer = true
mount_route = "/whatsapp/pair"
only_until_paired = true
```

Daemon iterates `plugin_handles[..].manifest().orchestration` and
runs the orchestration loop generically:

- Allocates per-instance state map (opaque `Value` indexed by
  instance label).
- Subscribes the broker bridge that mirrors `inbound_state_events`
  into the state map.
- Auto-opens public tunnel via `nexo-tunnel-quick` if `offer = true`
  and config allows.

State map is opaque from daemon's POV — it just stores JSON
payloads keyed by instance. Plugin subprocess writes events with
its own internal schema. HTTP layer (Layer 8) proxies queries
into the state map.

**Trade-off.** State payloads are opaque JSON daemon-side. No
typed access; daemon can't enforce schema. Plugin contract is
"whatever you publish on `inbound_state_topic` is what callers
get back from `/whatsapp/<inst>/status`". Plugin authors test
the round-trip themselves.

This is the LARGEST single piece. Probably split:
- 12a — opaque state map + subscriber bridge (~5h)
- 12b — public tunnel auto-open generalised (~3h)
- 12c — remove whatsapp-specific blocks from daemon (~2h, after
  whatsapp ships orchestration manifest section)

## Migration plan

Execution order matters because layers depend on each other:

1. **Stage 1 — Layer 7 (pairing adapter)** — closes
   Phase 81.33.b.real. Smallest deliverable. Validates Pattern A
   end-to-end with a real subprocess. ~5h. **First.**
2. **Stage 2 — Layer 8 (HTTP routes)** — unblocks the
   orchestration-tunnel work. The orchestration tunnel needs to
   know how plugins expose pairing pages; once HTTP-via-proxy is
   the contract, the tunnel just mounts the proxy prefix. ~6h.
3. **Stage 3 — Layer 12a + 12b (orchestration core + tunnel)** —
   closes Phase 93.5.d main mass. Depends on Layer 8. ~8h.
4. **Stage 4 — Layer 9 (admin RPC)** — orthogonal; can interleave
   with Stage 3. Removes `with_wa_bot_handle` typed path. ~5h.
5. **Stage 5 — Layer 11 (metrics)** — small, independent. Can
   ship anywhere. ~4h.
6. **Stage 6 — Layer 10 (dashboard polish)** — move sources from
   `nexo-setup` to manifest data. Last because plugin crates need
   2 prior releases first (Pattern B precedent + Stage 1's
   manifest format). ~4h.
7. **Stage 7 — Layer 6 cleanup + Layer 12c** — remove all
   remaining hardcoded plugin-name fallbacks from daemon
   (`register_whatsapp_tools` fallbacks, whatsapp orchestration
   block). Only after canonical plugin crates have shipped the
   manifest revisions for layers 1-6. ~3h.

**Total**: ~35h (~7 sessions of 5h each, more realistic than the
optimistic earlier estimates).

**Critical dependency.** Each stage that needs a new manifest
section blocks on a coordinated release of the 3 canonical
plugin crates (whatsapp, telegram, email). The daemon ships
fallbacks until the plugin manifest revisions are out. Plan
plugin releases AHEAD of removing the daemon fallback.

## Trade-offs we are explicitly accepting

| Layer | Trade-off |
|---|---|
| 7 — pairing | Broker RPC per unique sender. Cache after first sighting. ≤5ms one-time per pairing handshake. |
| 8 — HTTP | Broker round-trip per request. ≤2ms. Unacceptable for high-throughput webhooks — those keep direct ports. |
| 9 — admin RPC | Broker round-trip per admin command. ≤3ms. Admin commands are human-initiated, latency invisible. |
| 10 — dashboard | Schema enumerates auth-check + layout shapes. New shapes = schema extension, not framework code change. |
| 11 — metrics | Broker scrape per plugin per scrape interval. Cache-with-TTL if frequency is sub-second. |
| 12 — orchestration | State map daemon-side is opaque `serde_json::Value`. Plugin owns schema entirely. |

## Open questions

1. **Trait async migration.** Several trait methods are sync today
   (`PairingChannelAdapter::normalize_sender`,
   `ChannelDashboardSource::discover`). Generic broker-RPC dispatch
   needs async. Migrate trait to async or wrap with sync→async
   bridges? Lean: migrate to async, callers are already in async
   contexts.

2. **Plugin manifest schema version.** Each new manifest section
   bumps an implicit schema version. Should we add an explicit
   `nexo_manifest_version` field that the daemon checks for
   forward compatibility? Lean: yes, add `nexo_manifest_version = 2`
   in this design wave, daemon refuses to load `v1` plugins after
   transition window.

3. **In-tree plugin migration.** Email is still in-process (Phase
   93.11 bucket D). Does it adopt the same manifest sections, or
   does in-process keep using direct trait dispatch? Lean: same
   manifest sections, but `EmailPlugin` overrides each
   `build_pairing_adapter / mount_http / ...` to return Rust
   impls directly. Subprocess plugins return generic adapters.
   Trait method is the unifying API.

4. **Hot-reload.** OpenClaw supports plugin config hot-reload
   (`reload.configPrefixes`). Rust's static linking + subprocess
   model makes this harder. Lean: defer — each section's reload
   semantics get spec'd when the section ships. For now,
   config reload triggers subprocess restart of affected plugins.

5. **Plugin permission model.** Once plugins can declare HTTP
   routes + admin commands + metrics endpoints, the daemon needs
   to enforce per-plugin permissions (a malicious plugin shouldn't
   register `/admin/dangerous-thing`). Lean: prefix every plugin's
   declared routes with `/plugins/<plugin_id>/` mandatory. No
   plugin can mount at `/admin` or `/health` directly. Add the
   namespace constraint in this design wave.

## Validation strategy

Each stage gets:

1. **Unit tests** in the affected crate for the new types +
   interpreters.
2. **Integration test** spinning up a real subprocess plugin
   declaring the new manifest section, exercising the round-trip
   via broker.
3. **Build matrix preservation** — every stage keeps
   `cargo build --no-default-features` clean. Slim daemon does
   not need any plugin manifest section to compile.
4. **Documentation** — `docs/src/plugins/<section>.md` per
   manifest section the operator-writing plugin author needs to
   know.

## Non-goals

- **Hot-reload of compiled plugin binaries.** Subprocess restart
  is the reload story.
- **Wasm plugin runtime.** Out of scope. If/when added, this
  manifest-driven contract is what Wasm modules speak.
- **3rd-party plugin distribution (registry, signing).** Out of
  scope. Operator-managed paths only.
- **Web UI auto-generation from manifest.** Phase 83 microapp
  consumes the manifest for its own UI but the auto-discovery
  contract is daemon-side only.

## Next session: brainstorm + spec + plan for Stage 1

Per the project's `/forge` flow, the actual execution begins
with `/forge brainstorm 81.33.b.real` → spec → plan → ejecutar.
This memo is the architectural anchor that every brainstorm
must reference.
