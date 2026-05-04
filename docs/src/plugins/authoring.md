# Plugin authoring overview

Phase 31.9. Entry point for authors building anything that
extends nexo-rs from the outside. This page gets you to the
right deeper guide in 60 seconds.

## Read this when

- You want to add capability to nexo-rs and have not yet picked
  between a plugin, an extension, or a microapp.
- You have picked "plugin" and need to know which language SDK
  to start with.
- You want a 5-minute end-to-end smoke test before committing
  to a language choice.

## Plugin vs Extension vs Microapp

nexo-rs ships three extension surfaces. They differ in who
owns the runtime, who owns the UI, and how operators install
them.

| You're building | Use | Owns UI? | Owns auth/billing? | Common languages |
|-----------------|-----|----------|--------------------|------------------|
| New channel (Slack, Discord, IRC) or poller | **Plugin** | No (daemon owns I/O) | No (operator config) | Rust, Python, TypeScript, PHP |
| Bundle of skills, advisors, prompts, or YAML config that operators `nexo ext install` | **Extension** | No | No | YAML + small Rust stubs |
| End-product on top of nexo-rs (multi-tenant SaaS, internal tool, white-label deploy) | **Microapp** | ✅ yes | ✅ yes | Any language with a NATS client |

If you are still unsure:

- **Plugin** if your code is reactive (`broker.event` fires →
  you do something) and ships as a binary the daemon spawns.
- **Extension** if your code is declarative (skills + agents +
  prompts) and ships as a tarball operators install with
  `nexo ext install`.
- **Microapp** if your code is the product. End users see your
  UI, your domain, your billing — nexo-rs is invisible
  infrastructure.

This page covers **plugins**. For extensions, jump to
[Manifest reference](../extensions/manifest.md). For
microapps, jump to
[Microapps · getting started](../microapps/getting-started.md).

## Pick a language

All four SDKs implement the same wire contract — your choice
is purely about ergonomics. Operators don't care which SDK you
picked; they just run `nexo plugin install <owner>/<repo>`.

| Language | Best for | Runtime deps | Per-target binaries? | SDK reference |
|----------|----------|--------------|----------------------|---------------|
| **Rust** | Performance, single static binary, zero runtime deps. | None — `cargo build` produces a static ELF/Mach-O. | ✅ yes (one tarball per Rust target) | [Rust SDK](./rust-sdk.md) |
| **Python** | Existing scripts, ML ecosystem, fast iteration. | `python3.11+` on operator host. | No (`noarch` — single tarball) | [Python SDK](./python-sdk.md) |
| **TypeScript** | Existing Node servers, npm ecosystem, frontend devs. | `node 20+` on operator host. | No (`noarch`) | [TypeScript SDK](./typescript-sdk.md) |
| **PHP** | Existing Composer / Symfony / Laravel codebase. | `php 8.1+` (Fibers required) on operator host. | No (`noarch`) | [PHP SDK](./php-sdk.md) |

Cross-cutting reference: [Plugin contract](./contract.md) is
the wire spec all four SDKs implement. Read it once and you
understand every SDK.

## 5-min quickstart

The shortest path from zero to a running plugin uses Rust
because the toolchain ships with cargo. Adapt the
`nexo plugin new --lang <other>` step for Python / TypeScript
/ PHP — the rest is identical.

```bash
# 1. Scaffold from the bundled template (Phase 31.6).
nexo plugin new my_plugin --lang rust --owner alice
cd my_plugin

# 2. Build (under a second on a warm cache).
cargo build

# 3. Boot the daemon with this directory injected at the head
#    of plugins.discovery.search_paths. No install, no verify,
#    no GitHub round-trip — pure inner-loop dev.
nexo plugin run .
```

Expected stderr trace from step 3:

```
INFO local plugin override applied (plugin_id=my_plugin)
INFO subprocess plugin spawned (id=my_plugin, pid=...)
INFO my_plugin starting
INFO subprocess plugin handshake ok (id=my_plugin, version=0.1.0)
```

The plugin is now live. Publishing any event on a topic the
plugin's manifest registers (default
`plugin.inbound.my_plugin_echo`) reaches the handler in
`src/main.rs::handle_event`.

To exit, send `Ctrl+C` — the daemon issues a `shutdown`
request, the plugin's `on_shutdown` runs, and both processes
return cleanly.

## Plugin config dir

Phase 81.4 — operators place per-plugin YAML config under
`<config_dir>/plugins/<plugin_id>/`. The daemon reads every
`*.yaml` / `*.yml` file in that directory at boot, deep-merges
them alphabetically, resolves `${ENV_VAR}` placeholders, and
(when your manifest declares a `schema_path`) validates the
merged tree against your JSONSchema before calling
`init()`. Validation failure aborts plugin load with
`InitOutcome::Failed`; the daemon continues without the plugin.

Multi-file sharding lets operators split sensitive settings
from declarative ones:

```text
<config_dir>/plugins/slack/
  01-credentials.yaml   # api_token: "${SLACK_BOT_TOKEN}"
  02-channels.yaml      # channels: [...]
  03-allowlist.yaml     # rate limits per channel
```

Mappings deep-merge across files (later wins per-key).
**Arrays full-replace** — they don't concat — so an operator
override file completely substitutes the array from earlier
files. Comment-only and non-`.yaml` files are ignored.

Declare your config schema in `nexo-plugin.toml`:

```toml
[plugin.config]
schema_path = "config.schema.json"   # relative to plugin root
hot_reload = true                    # parsed; wiring lands in 81.4.b
```

The schema validator currently supports the JSONSchema subset
`type` / `required` / `properties` / `additionalProperties` /
`enum`. Plugins needing `oneOf` / `$ref` / `pattern` will get
richer validation in a future 81.4.c slice — for now, those
keywords pass through silently.

Inside your plugin, consume `ctx.plugin_config` (an
`Arc<serde_yaml::Value>`):

```rust
let api_token = ctx
    .plugin_config
    .get("api_token")
    .and_then(serde_yaml::Value::as_str)
    .ok_or_else(|| anyhow::anyhow!("api_token missing"))?;
```

When the operator hasn't placed any config files, the value is
an empty mapping — your plugin sees `Value::Mapping(empty)`,
not `Null`. Plugins with all-optional fields boot cleanly
without operator action.

## Contributing channel kinds

Phase 81.24 — subprocess plugins that declare
`[plugin.extends].channels = [...]` automatically get a
host-side `RemoteChannelAdapter` registered for each kind. The
daemon's `ChannelAdapterRegistry` routes outbound dispatches to
your subprocess via three JSON-RPC methods:

- `channel.start { kind, instance }` — subscribe outbound
  topics + begin publishing inbound (default 30 s timeout)
- `channel.stop { kind }` — release resources (30 s)
- `channel.send_outbound { kind, msg }` — send one outbound
  message; reply with `{ message_id, sent_at_unix }` (60 s)

Wire spec + error codes:
[Plugin contract §5.x](./contract.md#5x-channel-methods-phase-8124).

Sketch (Rust subprocess plugin) — handle each request from your
adapter's reader loop:

```rust
match method {
    "channel.start" => reply_ok(id, serde_json::json!({ "ok": true })),
    "channel.stop"  => reply_ok(id, serde_json::json!({ "ok": true })),
    "channel.send_outbound" => {
        let msg = params.get("msg").cloned().unwrap_or_default();
        // Forward `msg` to your provider's API; map the API's
        // response into OutboundAck.
        let ack = send_to_slack(msg).await?;
        reply_ok(id, serde_json::json!({
            "message_id": ack.id,
            "sent_at_unix": ack.ts,
        }));
    }
    _ => reply_method_not_found(id, method),
}
```

For typed errors (rate-limit, recipient invalid, etc.), reply
with the channel-specific error codes from the contract table —
the host's adapter maps them to `ChannelAdapterError` variants
the agent runtime understands.

The matching SDK helpers (`handle_channel_start` /
`handle_channel_send_outbound` etc.) ship in Phase 81.24.b.
Until then, hand-handle the JSON-RPC frames using the SDK's
existing primitives.

## Contributing LLM providers

Phase 81.25 — subprocess plugins that declare
`[plugin.extends].llm_providers = [...]` get one host-side
`RemoteLlmFactory` registered into `LlmRegistry` per provider
name. When the agent runtime resolves
`model.provider = "<name>"`, the factory builds a
`RemoteLlmClient` that translates trait calls into `llm.chat`
JSON-RPC requests over your subprocess plugin's stdio pipe.

```toml
[plugin.extends]
llm_providers = ["cohere", "mistral"]
```

Two modes supported on the wire:

- **Sync** — `params.stream = false`; reply once with
  `WireChatResponse` (default 60 s timeout).
- **Streaming** — `params.stream = true`; emit zero or more
  `llm.chat.delta { request_id, chunk }` notifications + one
  final response carrying `usage` / `finish_reason` (default
  300 s timeout).

Wire spec + error codes:
[Plugin contract §5.y](./contract.md#5y-llm-provider-methods-phase-8125).

Sketch (Rust subprocess plugin) — handle `llm.chat` from your
adapter's reader loop:

```rust
match method {
    "llm.chat" => {
        let provider = params["provider"].as_str().unwrap_or("");
        let stream = params["stream"].as_bool().unwrap_or(false);
        let request = serde_json::from_value::<WireChatRequest>(
            params["request"].clone()
        )?;
        if stream {
            // Emit zero or more deltas
            send_notification("llm.chat.delta", json!({
                "request_id": id,
                "chunk": { "type": "text_delta", "delta": "Hello" },
            }));
            // ...then the final response.
            reply_ok(id, /* WireChatResponse with usage + finish_reason */);
        } else {
            // Sync: call your provider's API, build WireChatResponse.
            let resp = call_my_provider(provider, &request).await?;
            reply_ok(id, resp);
        }
    }
    _ => reply_method_not_found(id, method),
}
```

For typed errors (rate-limit, auth failed, model not found),
reply with the LLM-specific error codes from the contract table
— the host's `RemoteLlmClient` surfaces them as `anyhow::Error`
with operator-greppable messages.

The matching SDK helpers (`PluginAdapter::handle_llm_chat`,
streaming sender, etc.) ship in Phase 81.25.b. Until then,
hand-handle the JSON-RPC frames.

## Contributing hook handlers

Phase 81.27 — subprocess plugins that declare
`[plugin.extends].hooks = [...]` get one host-side
`RemoteHookHandler` registered into `HookRegistry` per hook
name. When the daemon fires that hook, the handler translates
the call into a `hook.on_hook` JSON-RPC request over your
subprocess plugin's stdio pipe.

```toml
[plugin.extends]
hooks = ["before_message", "after_message"]
```

Wire spec + error semantic:
[Plugin contract §5.z](./contract.md#5z-hook-methods-phase-8127).

The reply shape is the existing `HookResponse` struct:

```rust
HookResponse {
    abort: bool,                     // legacy block signal
    reason: Option<String>,          // operator-readable
    override: Option<Value>,         // key-by-key mutation
    decision: Option<String>,        // "allow" | "block" | "transform"
    transformed_body: Option<String>,// for "transform"
    do_not_reply_again: bool,        // anti-loop signal
}
```

Sketch (Rust subprocess plugin) — handle each hook by name:

```rust
match method {
    "hook.on_hook" => {
        let hook_name = params["hook_name"].as_str().unwrap_or("");
        let event = params["event"].clone();
        let response = match hook_name {
            "before_message" => check_pii(&event)?,    // your logic
            "after_message"  => log_audit(&event)?,
            _ => HookResponse::default(),               // Continue
        };
        reply_ok(id, serde_json::to_value(&response)?);
    }
    _ => reply_method_not_found(id, method),
}
```

**Continue-on-error semantic** — the host swallows every
dispatch failure (timeout, malformed reply, JSON-RPC error)
and returns `HookResponse::default()` so the registry's fire
loop keeps iterating. Failures land in `tracing::warn!` for
operator debugging but never break the agent flow. This means:

- Returning `-32601 method_not_found` for an unknown hook is
  fine — host logs + continues.
- A hung subprocess hook eventually times out (5s default;
  `NEXO_PLUGIN_HOOK_TIMEOUT_MS` env override) and the agent
  proceeds.
- Returning a malformed `HookResponse` still continues; only
  well-formed responses with `abort: true` or `decision:
  "block"` actually block.

Hooks fire on the message hot path — keep handler latency
low (<50 ms typical). Use the `decision: "transform"` path
sparingly: every transform rewrites the event payload for
subsequent handlers.

## Contributing memory backends

Phase 81.26 — subprocess plugins that declare
`[plugin.extends].memory_backends = [...]` get one
host-side `RemoteVectorBackend` registered into the daemon's
`VectorBackendRegistry` per backend name. v1 covers VECTOR
storage only — short/long-term memory keep their SQLite
implementation; plugins replace only the vector index. Primary
use case: Pinecone / Qdrant / Weaviate / pgvector.

```toml
[plugin.extends]
memory_backends = ["pinecone"]
```

Three wire methods (default timeouts 30s upsert/delete, 10s
search):

- `memory.vector_upsert { backend, collection, records }` →
  `{ count }`
- `memory.vector_search { backend, collection, query }` →
  `{ matches: [...] }`
- `memory.vector_delete { backend, collection, ids }` →
  `{ count }`

`NEXO_PLUGIN_MEMORY_TIMEOUT_MS` env overrides all three.

Wire spec + error codes:
[Plugin contract §5.w](./contract.md#5w-memory-backend-methods-phase-8126).

Sketch (Rust subprocess plugin) — handle each method by name:

```rust
match method {
    "memory.vector_upsert" => {
        let collection = params["collection"].as_str().unwrap_or("");
        let records: Vec<VectorRecord> = serde_json::from_value(
            params["records"].clone()
        )?;
        let count = my_pinecone_client.upsert(collection, records).await?;
        reply_ok(id, serde_json::json!({"count": count}));
    }
    "memory.vector_search" => {
        let collection = params["collection"].as_str().unwrap_or("");
        let query: VectorQuery = serde_json::from_value(
            params["query"].clone()
        )?;
        let matches = my_pinecone_client.search(collection, query).await?;
        reply_ok(id, serde_json::json!({"matches": matches}));
    }
    "memory.vector_delete" => {
        let collection = params["collection"].as_str().unwrap_or("");
        let ids: Vec<String> = serde_json::from_value(
            params["ids"].clone()
        )?;
        let count = my_pinecone_client.delete(collection, ids).await?;
        reply_ok(id, serde_json::json!({"count": count}));
    }
    _ => reply_method_not_found(id, method),
}
```

For typed errors (collection-not-found, dimension-mismatch,
rate-limited, write-failed), reply with the memory-specific
error codes from the contract table — the host's
`RemoteVectorBackend` surfaces them as `anyhow::Error` with
operator-greppable messages.

**v1 limitation**: registered backends are NOT yet consumed at
runtime — `LongTermMemory.recall_vector` still uses sqlite-vec.
Operators audit registered backends today via
`wire.vector_backend_registry.names()`. Consumer-side dispatch
(`agents.yaml.<id>.vector_backend = "pinecone"`) lands in
Phase 81.26.b.

## Contributing tools

Phase 81.29 — subprocess plugins can expose **tools** that the
daemon's LLM picks via function-calling. Each tool name lives
in `[plugin.extends].tools = [...]` plus the subprocess
advertises the matching schema at handshake.

```toml
[plugin]
id = "browser"
# ... other manifest fields ...

[plugin.extends]
tools = ["browser_navigate", "browser_click"]
```

The subprocess MUST advertise these tools in the
initialize-reply's `tools` array:

```jsonc
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "manifest": { "plugin": { "id": "browser", ... } },
    "server_version": "browser-0.1.1",
    "tools": [
      {
        "name": "browser_navigate",
        "description": "Navigate to a URL",
        "input_schema": {
          "type": "object",
          "properties": { "url": { "type": "string" } },
          "required": ["url"]
        }
      }
    ]
  }
}
```

When the agent's LLM picks a tool the daemon issues a
`tool.invoke` JSON-RPC request to the subprocess:

```jsonc
{
  "jsonrpc": "2.0",
  "id": 80,
  "method": "tool.invoke",
  "params": {
    "plugin_id": "browser",
    "tool_name": "browser_navigate",
    "args": { "url": "https://example.com" },
    "agent_id": "shopper"
  }
}
```

The plugin replies with the tool's result (typically a
`ToolResponse`-shaped object). Errors use the
`-33401..=-33405` band documented in
[Plugin contract §5.t](./contract.md#5t-tool-methods-phase-8129).

**Validation rules** (host-side):

- Tool names MUST satisfy the per-plugin namespace policy from
  Phase 81.3 (`<plugin_id>_*` or `ext_<plugin_id>_*`).
- The advertised `tools` array MUST be a subset of
  `extends.tools` — drift in this direction is a hard failure
  at handshake.
- Manifest entries WITHOUT an advertised counterpart are
  tolerated but logged at warn; runtime calls yield
  `-33401 ToolNotFound`.

**Plugin-side responsibilities**:

- Validate args against the published `input_schema` before
  executing (defense in depth — host already validates host-
  side, but plugins should re-check).
- Return `-33402 ToolArgumentInvalid` with `details: <Value>`
  pointing to the offending field if validation fails.
- Return `-33404 ToolUnavailable` with `data: { retry_after_ms: <u64> }`
  for transient failures (rate-limits, locked resources).

**Default timeout**: 60 s (matches the LLM band — tools span
fast `browser_click` to slow `browser_navigate`). Operator
override via `NEXO_PLUGIN_TOOL_TIMEOUT_MS`.

**v1 limitations** — see follow-ups in `FOLLOWUPS.md`:
streaming tools (chunked outputs via `tool.invoke.delta`),
per-tool timeout knobs in manifest, SDK helper
`PluginAdapter::on_tool(name, handler)`.

## Sandboxing your plugin

Phase 81.22 — Linux subprocess plugins can opt into bubblewrap
isolation by declaring `[plugin.sandbox]` in `nexo-plugin.toml`.
Default is disabled, so existing plugins keep today's behavior;
opt in when you want defense-in-depth.

```toml
[plugin.sandbox]
enabled = true
network = "deny"                       # "deny" | "host"
fs_read_paths = ["/etc/ssl/certs"]    # absolute paths only
fs_write_paths = ["${state_dir}"]     # ${state_dir} = per-plugin
                                       # state root, the only safe
                                       # writable place by default
drop_user = true                       # nobody:nogroup uid mapping
```

**Linux prereq**: install `bubblewrap` (`apt install bubblewrap`
on Debian/Ubuntu, available on Arch + Alpine + Fedora). The
daemon discovers `bwrap` once at boot via PATH lookup. Without
bwrap, sandbox-enabled plugins log a warning and run unsandboxed
unless `NEXO_PLUGIN_SANDBOX_REQUIRE=1` is set (in which case
boot fails).

**macOS**: not yet enforced. The daemon logs a `tracing::warn!`
at every spawn and runs the plugin without sandbox. Native
sandbox-exec integration is deferred to follow-up 81.22.macos
(deprecated-API risk noted).

**Network policy**:
- `network = "deny"` → plugin runs in an isolated network
  namespace with only loopback. Use the host's daemon-mediated
  RPCs (`llm.complete`, `memory.recall`, `broker.publish`) for
  any external IO. Recommended default.
- `network = "host"` → plugin shares the daemon's network.
  Operator must opt in via
  `NEXO_PLUGIN_SANDBOX_HOST_NET_ALLOW=1`; manifest validation
  rejects this otherwise. Use only when daemon-mediated RPCs
  cannot satisfy your plugin's IO needs.

**Filesystem allowlist**:
- `fs_read_paths` = host paths bound read-only into the sandbox
  (`bwrap --ro-bind`). Common: `/etc/ssl/certs` for outbound
  TLS verification.
- `fs_write_paths` = host paths bound read-write
  (`bwrap --bind`). The literal `${state_dir}` token expands at
  spawn time to `<state_root>/plugins/<plugin_id>` — that is
  your plugin's per-instance owned scratch space. Only token
  recognized; only valid in `fs_write_paths`.

**Hard denylist** (compile-time, not configurable):
allowlist entries that equal or include any of these paths are
rejected at manifest validation:

```
/etc/shadow, /etc/sudoers, /etc/sudoers.d
/proc/sys, /proc/kcore, /proc/kallsyms
/sys/firmware, /sys/kernel
/dev/mem, /dev/kmem, /dev/port
/var/run/docker.sock, /run/docker.sock,
  /private/var/run/docker.sock
/root, /boot
```

**Operator capability env knobs**:

| Env var | Effect |
|---------|--------|
| `NEXO_PLUGIN_SANDBOX_REQUIRE=1` | Refuse to spawn any plugin without `sandbox.enabled = true`. |
| `NEXO_PLUGIN_SANDBOX_HOST_NET_ALLOW=1` | Permit `network = "host"` manifests. Default off. |

**Recommended pattern**: `enabled = true, network = "deny",
fs_write_paths = ["${state_dir}"]`, no `fs_read_paths` unless
your plugin truly needs to read host config (e.g. CA bundles
for TLS). Use daemon-mediated RPCs for everything else.

**v1 out of scope** — see follow-ups in `FOLLOWUPS.md`:
granular network egress allowlist (81.22.b), per-syscall
seccomp filters (81.22.c), `nexo agent doctor plugins` sandbox
section (81.22.d), native macOS sandbox-exec (81.22.macos).

## Future capability extensions

Phase 81.28 — subprocess plugins that contribute new
**channel kinds**, **LLM providers**, **memory backends**, or
**HookInterceptor IDs** declare them via an additive
`[plugin.extends]` manifest section:

```toml
[plugin.extends]
channels         = ["slack"]              # paired with Phase 81.24 wrapper
llm_providers    = ["cohere"]             # paired with Phase 81.25
memory_backends  = ["pinecone"]           # paired with Phase 81.26
hooks            = ["pii_redact"]         # paired with Phase 81.27
```

Each list names the IDs the plugin contributes. Validation
rules + the canonical schema live in
[Plugin contract §2.1](./contract.md#21-extends-section-phase-8128).
Daemon dispatch wiring (actually populating the matching
registry slots) ships per-registry across Phase 81.24-27 — the
schema is shipped today so subprocess plugin authors can
declare intent ahead of those wrappers landing.

## Local dev loop conventions

- **`nexo plugin run <path>`** — boots the daemon with one
  local plugin overriding discovery; the rest of the system
  (broker, agents, channels) runs as configured.
- **`nexo plugin run <path> --no-daemon-config`** — same, but
  clears `cfg.agents.agents` so the plugin runs in isolation
  for contract debugging.
- **Rebuild → respawn** — Phase 81.10 hot-reload re-walks
  `search_paths` periodically, so a fresh `cargo build` triggers
  the daemon to respawn the subprocess automatically. No
  `--watch` flag yet (Phase 31.7.b deferred).

## Next steps

- [Rust SDK](./rust-sdk.md) — full Rust API + manifest example.
- [Python SDK](./python-sdk.md), [TypeScript SDK](./typescript-sdk.md), [PHP SDK](./php-sdk.md) — language-specific
  references with the same shape.
- [Plugin contract](./contract.md) — wire spec; read this once
  and you can debug any SDK.
- [Patterns (8 common shapes)](./patterns.md) — pre-baked
  designs for channel plugins, pollers, hybrid bridges.
- [Publishing a plugin](./publishing.md) — asset naming
  convention + 4-job CI workflow shape.
- [Signing & publishing](./signing-and-publishing.md) — cosign
  keyless tutorial that operators on `--require-signature`
  need.
- [Plugin trust (`trusted_keys.toml`)](../ops/plugin-trust.md)
  — operator-side verification policy your readers will
  configure to trust your releases.
