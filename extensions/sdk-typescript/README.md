# nexo-plugin-sdk (TypeScript)

Phase 31.5 — child-side SDK for nexo subprocess plugins written
in TypeScript or plain JavaScript. Mirrors the Rust counterpart
in [`crates/microapp-sdk/`](../../crates/microapp-sdk/) and the
Python counterpart in [`extensions/sdk-python/`](../sdk-python/).
Same wire format ([`nexo-plugin-contract.md`](../../nexo-plugin-contract.md)),
different language.

The reference plugin template lives at
[`extensions/template-plugin-typescript/`](../template-plugin-typescript/);
copy that directory to start a new plugin.

## Public API

```typescript
import {
  PluginAdapter,        // async dispatch loop
  BrokerSender,         // write-only handle to publish events back
  Event,                // value object mirror of the host's broker event
  PluginError,          // base exception
  ManifestError,        // raised when nexo-plugin.toml is malformed
  WireError,            // raised on malformed JSON-RPC frames or oversized lines
  installStdoutGuard,   // defensive guard installable independently
  parseManifest,        // standalone manifest TOML parser
  STDOUT_GUARD_MARKER,  // sentinel that prefixes diverted stderr lines
} from "nexo-plugin-sdk";
```

## Minimal example

```typescript
import { readFileSync } from "node:fs";
import { PluginAdapter, Event } from "nexo-plugin-sdk";

const MANIFEST = readFileSync("nexo-plugin.toml", "utf-8");

const adapter = new PluginAdapter({
  manifestToml: MANIFEST,
  onEvent: async (topic, event, broker) => {
    const out = Event.new(
      "plugin.inbound.my_kind",
      "my_plugin",
      { echoed: event.payload },
    );
    await broker.publish("plugin.inbound.my_kind", out);
  },
});

await adapter.run();
```

## Robustness defaults

The constructor defaults are picked to make the most common
plugin-author mistakes recoverable rather than fatal:

| Default | What it gives you |
|---------|-------------------|
| `enableStdoutGuard: true` | Stray `console.log("hi")` from your handler (or a chatty transitive dep) is diverted to stderr tagged with `[stdout-guard]` rather than corrupting the JSON-RPC frame stream the host parses. |
| `maxFrameBytes: 1 << 20` | Inbound JSON-RPC frames larger than 1 MiB are rejected with a `WireError` log; dispatch continues. Adversarial host cannot OOM the plugin via a single huge line. |
| `handleProcessSignals: true` | Ctrl-C / SIGTERM trigger a graceful shutdown — in-flight handler tasks are awaited (no mid-publish cancellation), then the process exits 0. |
| In-flight task drain on `shutdown` | Handlers spawned for `broker.event` are awaited via `Promise.allSettled([...inflight])` before the SDK replies `{ok: true}` to a host's `shutdown` request. Same idiom as the Python SDK's `_drain_inflight`. |

## What the daemon expects

| Method | Direction | Reply |
|--------|-----------|-------|
| `initialize` | host → child | `{ manifest, server_version }` automatically — the SDK reads + caches your manifest TOML at construction time. |
| `broker.event` (notification) | host → child | No JSON reply. Your `onEvent` handler runs in a detached task so the dispatch loop continues reading stdin while the handler awaits broker round-trips. |
| `shutdown` | host → child | `{ ok: true }` after draining in-flight tasks + invoking your `onShutdown` (if set). |

Full spec: [`nexo-plugin-contract.md`](../../nexo-plugin-contract.md).

## Tests

```bash
cd extensions/sdk-typescript
npm install
npm run build
npm test
```

13 tests covering:
- Handshake: initialize reply, unknown method `-32601`, unknown
  notification silently ignored.
- Manifest validation: missing id, invalid TOML, id regex
  violation.
- Dispatch: handler invocation, non-blocking reader, in-flight
  drain on shutdown.
- Stdout guard: idempotent install, console.log diverted to
  stderr.
- Wire: oversized frame rejected with continued dispatch.
- Lifecycle: double `run()` rejects with PluginError.

## Phase tracking

- 31.5 (shipped, this package) — child-side SDK + 13 tests +
  default-on stdout guard.
- 31.5.b (deferred) — per-target TypeScript tarballs
  (`<id>-<version>-node20-x86_64-linux.tar.gz` etc.) for
  plugins that need native node addons.
- 31.5.c (deferred) — PHP SDK + template.
- npm publish deferred — once the API stabilizes after 31.5.c
  this package ships to npm as `nexo-plugin-sdk`. Until then
  plugin authors vendor it via `pack-tarball-typescript.sh`.
