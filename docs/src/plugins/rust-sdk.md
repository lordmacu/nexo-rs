# Rust plugin SDK

Phase 31.9. Author plugins in Rust that the daemon spawns as
subprocesses, talking the same JSON-RPC 2.0 wire format used by
the Python / TypeScript / PHP SDKs.

The SDK lives in
[`crates/microapp-sdk/`](https://github.com/lordmacu/nexo-rs/tree/main/crates/microapp-sdk)
behind the `plugin` Cargo feature; the reference plugin template
is at
[`extensions/template-plugin-rust/`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/template-plugin-rust).
Use `nexo plugin new <id> --lang rust` to scaffold a fresh
out-of-tree project from that template.

## Read this when

- You picked Rust from the language picker in
  [Plugin authoring overview](./authoring.md) and want the
  SDK reference.
- You are porting an in-tree plugin (`crates/plugins/<id>`) into
  an out-of-tree subprocess and need the wire-API mapping.
- You want the canonical Rust handler signature for
  `broker.event` notifications.

## Why subprocess + Rust

Running Rust plugins as separate processes — instead of crates
linked into the daemon — gives you:

- **Isolation** — a panic in your plugin terminates one process,
  not the daemon.
- **One contract, every language** — the daemon treats your
  binary the same way it treats Python or TypeScript plugins.
  Switching languages later is an SDK choice, not a daemon
  recompile.
- **No link-time coupling** — your plugin can use any Rust
  toolchain or `tokio` version that compiles; the daemon does
  not care.
- **Single static binary** — `cargo build --release` produces
  one file the publish workflow uploads as the per-target
  tarball.

Daemon-side spawn code in
[`crates/core/src/agent/nexo_plugin_registry/subprocess.rs`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core/src/agent/nexo_plugin_registry)
treats the plugin as an opaque executable; Rust plugins re-use
that path without modification.

## Architecture

```
Operator host                              Plugin process
┌──────────────────┐    stdin   ┌─────────────────────────────┐
│ daemon (Rust)    │──JSON-RPC──▶│ target/release/<id>         │
│ subprocess host  │             │   tokio::main async runtime │
│                  │◀──JSON-RPC──│   PluginAdapter.run_stdio() │
└──────────────────┘    stdout   └─────────────────────────────┘
```

The daemon writes newline-delimited JSON-RPC requests to your
binary's stdin; you write replies + outbound `broker.publish`
notifications back on stdout. `stderr` is collected by the
operator's tracing pipeline (Phase 81.23 fold pending) — use it
freely for plugin-side logs.

## Public API

```rust
use nexo_broker::Event;
use nexo_microapp_sdk::plugin::{BrokerSender, PluginAdapter};
```

`PluginAdapter` builder methods:

| Method | Required | Description |
|--------|----------|-------------|
| `PluginAdapter::new(manifest_toml: &str)` | ✅ | Body of `nexo-plugin.toml`. Read once at startup; the SDK validates `plugin.id` + `plugin.version` and surfaces `ManifestError` on parse failure. |
| `.on_broker_event(handler)` | ⬜ | `async fn(topic: String, event: Event, broker: BrokerSender)`. Invoked for every `broker.event` notification. Each handler call is spawned on the runtime; the dispatch loop continues reading stdin without blocking. |
| `.on_shutdown(handler)` | ⬜ | `async fn() -> Result<(), Box<dyn Error + Send + Sync>>`. Awaited before the SDK replies `{ok: true}` to the host's `shutdown` request. In-flight `on_broker_event` tasks are awaited too. |
| `.run_stdio().await` | ✅ | Single-shot — calling it twice returns `PluginError::AlreadyRunning`. Drives the JSON-RPC loop until stdin closes or the host sends `shutdown`. |

`Event` (re-exported from `nexo-broker`) carries `topic`,
`source`, `payload: serde_json::Value`, optional `correlation_id`
+ `metadata`. Construct with `Event::new(topic, source, payload)`
which stamps a fresh UUID + RFC3339 timestamp.

`BrokerSender::publish(topic: &str, event: Event) -> Result<(),
WireError>` serializes a `broker.publish` notification to stdout
under an internal write lock. The daemon's bridge re-checks the
topic against the manifest's `[[plugin.channels.register]]`
allowlist before forwarding to the broker.

## Manifest example

```toml
[plugin]
id = "my_plugin"
version = "0.1.0"
name = "My Plugin"
description = "Forwards inbound events to a third-party API."
min_nexo_version = ">=0.1.0"

[plugin.requires]
nexo_capabilities = ["broker"]

[[plugin.channels.register]]
kind = "my_plugin_inbound"
description = "Inbound events the plugin emits onto the broker."
```

`plugin.id` MUST match `^[a-z][a-z0-9_]{0,31}$`. Cargo's
`[[bin]] name` MUST equal `plugin.id` so the publish workflow's
`pack-tarball.sh` finds the artifact at
`target/<target>/release/<id>`.

See [Plugin contract](./contract.md) for the full manifest
schema and the JSON-RPC envelope every method exchanges.

## Quickstart

Scaffold + build + run, copy-paste:

```bash
nexo plugin new my_plugin --lang rust --owner alice
cd my_plugin
cargo build
nexo plugin run .
```

`nexo plugin run` boots the daemon with your plugin injected at
the head of `plugins.discovery.search_paths`, bypassing the
install pipeline. See [Local dev loop](./authoring.md) for the
inner-loop conventions and `--no-daemon-config`.

The handler in the scaffolded `src/main.rs` echoes every
inbound event back on `plugin.inbound.<id>_echo`:

```rust
use nexo_broker::Event;
use nexo_microapp_sdk::plugin::{BrokerSender, PluginAdapter};

const MANIFEST: &str = include_str!("../nexo-plugin.toml");

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    PluginAdapter::new(MANIFEST)?
        .on_broker_event(handle_event)
        .on_shutdown(|| async {
            tracing::info!("plugin shutdown handler invoked");
            Ok(())
        })
        .run_stdio()
        .await?;
    Ok(())
}

async fn handle_event(topic: String, event: Event, broker: BrokerSender) {
    let echo = Event::new(
        "plugin.inbound.my_plugin_echo",
        "my_plugin",
        serde_json::json!({
            "echoed_from": topic,
            "echoed_payload": event.payload,
        }),
    );
    let _ = broker
        .publish("plugin.inbound.my_plugin_echo", echo)
        .await;
}
```

Replace the body of `handle_event` with your channel's real
outbound logic (forward to a third-party API, persist to disk,
trigger a downstream agent, etc.) and re-publish the API's
reply back through `broker` so agents can observe it.

## Smoke test

Hand-run the binary against a synthetic JSON-RPC frame to
confirm the handshake is well-formed:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
    | ./target/debug/my_plugin
```

The plugin should print one JSON-RPC response containing your
manifest's `id`, `version`, `name`, and the SDK's
`server_version`. If you see anything other than a single line
of valid JSON on stdout, check that you have not added stray
`println!`s in the handler — every byte on stdout must be a
JSON-RPC frame. Use `eprintln!` / `tracing::*` for logs.

## Per-target tarball convention

Operators install Rust plugins via the same `nexo plugin
install <owner>/<repo>[@<tag>]` CLI. The resolver expects
per-target tarballs:

```
<id>-<version>-<target>.tar.gz
├── nexo-plugin.toml
└── bin/<id>           # static binary, mode 0755
```

Targets follow Rust's standard target triples
(`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`,
`x86_64-unknown-linux-musl`, etc.). The shipped CI workflow
in
[`extensions/template-plugin-rust/.github/workflows/release.yml`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/template-plugin-rust/.github/workflows/release.yml)
covers Linux musl + macOS by default; add additional matrix
entries to support more.

## CI publish workflow

The shipped workflow has 4 jobs: `validate-tag` →
`build` (matrix) → optional `sign` (cosign keyless,
gated by repo variable `COSIGN_ENABLED`) → `release` (uploads
all tarballs + sha256 sidecars + signing material + a copy of
`nexo-plugin.toml`). See
[Publishing a plugin](./publishing.md) for the full asset
naming convention and
[Signing & publishing](./signing-and-publishing.md) for the
end-to-end signed-release tutorial.

## Local validation

Before pushing a tag, dry-run the pack step:

```bash
cargo build --release --target x86_64-unknown-linux-gnu
bash scripts/pack-tarball.sh x86_64-unknown-linux-gnu
ls dist/
# my_plugin-0.1.0-x86_64-unknown-linux-gnu.tar.gz
# my_plugin-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

The Rust integration test
[`extensions/template-plugin-rust/tests/pack_tarball.rs`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/template-plugin-rust/tests/pack_tarball.rs)
covers this end-to-end against a synthetic binary; copy it when
you fork the template to keep the convention regression-tested.

## SDK tests

```bash
cargo test -p nexo-microapp-sdk --features plugin
```

Covers handshake, manifest validation, dispatch (including
non-blocking reader proof), shutdown lifecycle, unknown-method
handling, oversized-frame rejection.

## See also

- [Plugin authoring overview](./authoring.md) — start here if
  you have not picked a language yet.
- [Plugin contract](./contract.md) — full wire spec every SDK
  implements.
- [Patterns (8 common shapes)](./patterns.md) — channel /
  poller / hybrid plugin shapes.
- [Publishing a plugin](./publishing.md) — CI workflow shape
  and asset naming convention.
- [Signing & publishing](./signing-and-publishing.md) — cosign
  keyless tutorial.
- [Plugin trust (`trusted_keys.toml`)](../ops/plugin-trust.md)
  — operator-side verification policy.
