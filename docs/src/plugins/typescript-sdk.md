# TypeScript plugin SDK

Phase 31.5. Author plugins in TypeScript (or plain JavaScript)
that the daemon spawns as subprocesses, talking the same
JSON-RPC 2.0 wire format used by the Rust SDK in
[`crates/microapp-sdk/`](https://github.com/lordmacu/nexo-rs/tree/main/crates/microapp-sdk)
and the Python SDK in
[`extensions/sdk-python/`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/sdk-python).

Reference template:
[`extensions/template-plugin-typescript/`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/template-plugin-typescript).
The SDK package itself lives at
[`extensions/sdk-typescript/`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/sdk-typescript).

## Why subprocess + Node instead of an embedded runtime

Running TypeScript plugins as separate Node processes:

- Keeps the daemon language-agnostic; one wire contract, three
  shipped SDK languages (Rust, Python, TypeScript).
- Isolates plugin failures (a runaway plugin cannot crash the
  daemon).
- Sidesteps V8 embedding complexity.

Daemon-side spawn code in
[`crates/core/src/agent/nexo_plugin_registry/subprocess.rs`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core/src/agent/nexo_plugin_registry)
treats the plugin as an opaque executable; TypeScript plugins
re-use it without modification.

## Architecture summary

```
Operator host                         Plugin process
┌──────────────────┐    stdin   ┌──────────────────────────┐
│ daemon (Rust)    │──JSON-RPC──▶│ bin/<id> (bash launcher) │
│ subprocess host  │             │   exec node main.js      │
│                  │◀──JSON-RPC──│   PluginAdapter.run()    │
└──────────────────┘    stdout   └──────────────────────────┘
```

The bash launcher in `bin/<id>` sets
`NODE_PATH=lib/node_modules` and exec's the vendored Node
runtime so the plugin's deps come from `lib/` only — no global
`node_modules` interference.

## Public API

```typescript
import {
  PluginAdapter,
  BrokerSender,
  Event,
  PluginError, ManifestError, WireError,
  installStdoutGuard, parseManifest,
  STDOUT_GUARD_MARKER,
} from "nexo-plugin-sdk";
```

`PluginAdapter` constructor options:

| Option | Required | Description |
|--------|----------|-------------|
| `manifestToml: string` | ✅ | Body of `nexo-plugin.toml`. Read once at startup; the SDK validates `plugin.id` (regex `/^[a-z][a-z0-9_]{0,31}$/`), `plugin.version`, `plugin.name`, `plugin.description`. |
| `serverVersion?: string` | ⬜ | Returned in the `initialize` reply. Default `"0.1.0"`. |
| `onEvent?: EventHandler` | ⬜ | `async (topic, Event, BrokerSender) => Promise<void>`. Invoked for every `broker.event` notification. Handler runs in a detached task; the dispatch loop continues reading stdin without blocking. |
| `onShutdown?: ShutdownHandler` | ⬜ | `async () => Promise<void>`. Awaited before `{ok: true}` reply to the host's `shutdown` request. In-flight `onEvent` tasks are also awaited before returning. |
| `enableStdoutGuard?: boolean` | ⬜ default `true` | Patches `process.stdout.write` so any stray `console.log` from your handler (or a chatty transitive dep) is diverted to stderr tagged with `STDOUT_GUARD_MARKER` instead of corrupting the JSON-RPC frame stream. |
| `maxFrameBytes?: number` | ⬜ default 1 MiB | Reject inbound frames larger than this with a `WireError` log; dispatch continues. |
| `handleProcessSignals?: boolean` | ⬜ default `true` | Listen for SIGTERM + SIGINT and trigger graceful shutdown (drain in-flight, exit 0). |

`Event` is a value object with `topic`, `source`, `payload`,
optional `correlation_id` + `metadata`.
`BrokerSender.publish(topic, event)` serializes a JSON-RPC
notification to stdout under a Promise-chain write lock so
concurrent handler tasks never interleave half-written frames.

## Tarball convention (`noarch`)

Operators install TypeScript plugins via the same
`nexo plugin install <owner>/<repo>[@<tag>]` CLI. The resolver
in `nexo-ext-installer` falls back to `noarch` when no
per-target tarball matches the daemon's host triple (Phase
31.4):

```
<id>-<version>-noarch.tar.gz
├── nexo-plugin.toml
├── bin/<id>           # bash launcher, mode 0755
└── lib/
    ├── plugin/main.js   # compiled from src/main.ts via tsc
    └── node_modules/
        ├── nexo-plugin-sdk/dist/...
        └── ...           # pure-JS production deps
```

The launcher (~5 LOC) reads:

```bash
#!/usr/bin/env bash
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec env NODE_PATH="$DIR/lib/node_modules" node "$DIR/lib/plugin/main.js" "$@"
```

## Pure-JS deps constraint

`noarch` requires that vendored deps work on every operator's
CPU. Native node addons (`*.node`, `*.so`, `*.dylib`, `*.dll`)
invalidate the claim. The publish workflow's audit step runs
`scripts/verify-pure-js.sh` post-vendor and rejects any tree
containing those suffixes.

If your plugin needs a native dep, per-target TypeScript
tarballs (`<id>-<version>-node20-x86_64-linux.tar.gz` etc.) are
tracked as Phase 31.5.b and not yet shipped.

## Stdout guard — the robustness multiplier

Plugin authors invariably `console.log("debug")` at some point,
or import a chatty dep (`dotenv` banners, transitive logging
libs). Without protection, the very first non-JSON line on
stdout corrupts the daemon's JSON-RPC parser mid-stream — no
recovery path, the host disconnects.

The default-on stdout guard wraps `process.stdout.write` and:

1. Buffers writes until a newline arrives.
2. Each complete line is `JSON.parse`-tested.
3. Lines that parse → forwarded to the real stdout.
4. Lines that don't parse → diverted to stderr tagged with
   `[stdout-guard] <line>`.

The blessed write path (`BrokerSender` and the SDK's own
response helpers) always emits valid JSON so frames pass through
unchanged. Operator log scraping picks up the `[stdout-guard]`
marker so debug output stays visible without breaking the wire
format.

Set `enableStdoutGuard: false` only if you have another guard
layer (e.g. process-level isolation) — it is the single
strongest recommendation in the SDK.

## CI publish workflow

The shipped workflow in
[`extensions/template-plugin-typescript/.github/workflows/release.yml`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/template-plugin-typescript/.github/workflows/release.yml)
has the same 4-job shape as the Rust + Python templates but:

- Build matrix has a single `noarch` entry.
- Build step uses `actions/setup-node@v4` + `npm ci` + `npm run typecheck` + `npm run build` (`tsc` to `dist/`).
- Pre-vendor: `npm prune --omit=dev` strips dev deps so only
  runtime deps land in the tarball.
- Vendor audit step calls `scripts/verify-pure-js.sh
  .audit/lib/node_modules` to enforce pure-JS.

Sign + release jobs are identical to the Rust + Python templates;
cosign keyless OIDC ships `.sig` + `.pem` + `.bundle` per asset
when the `COSIGN_ENABLED` repo variable is `"true"`.

## Operator install flow (no changes for TypeScript)

```bash
nexo plugin install your-handle/your-plugin@v0.2.0
```

Identical pipeline to the Rust + Python install paths:

1. Resolve release JSON.
2. Try `<id>-0.2.0-<host-triple>.tar.gz` (miss for noarch
   plugins).
3. **Fall back to `<id>-0.2.0-noarch.tar.gz`** (Phase 31.4
   addition).
4. Verify sha256.
5. Cosign verify per `trusted_keys.toml` (Phase 31.3).
6. Extract under `<dest_root>/<id>-0.2.0/`.
7. Daemon picks it up at next boot or hot-reload; spawns
   `bin/<id>` which exec's `node lib/plugin/main.js` with
   `NODE_PATH=lib/node_modules`.

## Local smoke test

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize"}' \
    | node dist/main.js
```

Should print one JSON-RPC response with your manifest +
`server_version`.

End-to-end test for the pack pipeline:

```bash
node --test tests/pack-tarball.test.mjs
```

## SDK tests

```bash
cd extensions/sdk-typescript
npm install
npm run build
npm test
```

13 tests across handshake, manifest validation, dispatch,
stdout-guard, wire, lifecycle. All run via stdlib `node:test`
so there is zero install friction beyond the SDK's runtime
dep on `smol-toml`.

## See also

- [Publishing a plugin (CI workflow)](./publishing.md) — Rust
  counterpart of the publish workflow this template is modeled
  after.
- [Python plugin SDK](./python-sdk.md) — sibling SDK in Python.
- [Plugin trust (`trusted_keys.toml`)](../ops/plugin-trust.md)
  — operator-side cosign verification policy that applies to
  TypeScript plugins too.
- [Plugin contract](./contract.md) — wire format all SDKs
  implement.
