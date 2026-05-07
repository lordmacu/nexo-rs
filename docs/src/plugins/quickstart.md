# Plugin quickstart — zero to installed in 10 minutes

Phase 31.9. Linear, copy-paste path from empty directory to a
plugin running inside an operator's daemon. Take this page once
end-to-end before reading
[Plugin authoring overview](./authoring.md), the
[Rust SDK reference](./rust-sdk.md), or the
[Plugin contract](./contract.md) — those documents make sense
faster after you have shipped one toy plugin.

Single-language path (Rust). Python / TypeScript / PHP
quickstarts share the exact same shell commands; only the
`--lang` flag and the in-repo source tree differ. Pointers to the
sister SDKs appear at the bottom.

## What you build

A plugin called `hello_plugin` that echoes every event arriving
on its inbound topic onto `plugin.inbound.hello_plugin_echo`. By
the end of this page:

- A new GitHub repository under your account holds the plugin's
  source.
- A signed (optional) GitHub release ships per-target tarballs.
- An operator on a separate host runs
  `nexo plugin install <you>/<repo>` and the daemon spawns your
  plugin inside the next 10 seconds, then logs the handshake.

This is the same pipeline that ships the in-house plugins — see
`github.com/lordmacu/nexo-plugin-browser` for a real-world
output of the same quickstart, scaled up to 12 tools.

## Prerequisites

| Tool | Version | Why |
|------|---------|-----|
| Rust toolchain | 1.74+ (`rustup` recommended) | Build the plugin binary. |
| `nexo` CLI | 0.1.5+ on PATH | `plugin new` (Phase 31.6), `plugin run` (31.7), `plugin install` (31.1.c). |
| `git` | any | Push to GitHub. |
| GitHub account + a repo you can push to | — | Releases host the install artifacts. |
| `cosign` (optional) | 2.x | Sign releases for operators on `--require-signature`. Skip until step 9. |

Verify each:

```bash
cargo --version          # cargo 1.74+
nexo --version           # 0.1.5+
git --version
gh auth status           # if using `gh` CLI for the repo create
```

If `nexo` is not yet on PATH, build it from the daemon workspace
once with `cargo install --path . --bin nexo` from your
`nexo-rs` checkout and make sure `~/.cargo/bin` is on PATH.

## 1. Scaffold

The CLI bundles the same Rust template that produces the
in-tree `template-plugin-rust` — one command lands a fresh
project on disk:

```bash
nexo plugin new hello_plugin --lang rust --owner alice
cd hello_plugin
```

Flags that matter:

- `<id>` — the plugin's globally unique id. Must satisfy
  `^[a-z][a-z0-9_]{0,31}$`. It becomes the prefix for any tool
  name, channel kind, or config namespace your plugin
  contributes.
- `--lang rust` — switch to `python`, `typescript`, or `php` for
  the matching template. The remaining steps are identical.
- `--owner alice` — your GitHub username. Used in the generated
  README + CI workflow's release URL.
- `--description "..."` — optional one-liner; flows into the
  manifest + README + Cargo description.
- `--git-init` — runs `git init` for you and stages the initial
  commit.
- `--dest /custom/path` — emit elsewhere (default is
  `./<id>/`).

Re-running the command on a non-empty directory aborts; pass
`--force` only if you mean it.

## 2. Inspect what landed

```text
hello_plugin/
├── Cargo.toml               # name = "hello_plugin", bin name matches plugin.id
├── nexo-plugin.toml         # manifest — read by the daemon at handshake
├── README.md                # operator-facing docs (edit these later)
├── scripts/
│   └── pack-tarball.sh      # the per-target tarball packer the CI uses
├── src/
│   └── main.rs              # tokio::main + PluginAdapter
└── tests/
    └── pack_tarball.rs      # regression test for the asset shape
```

Two files you must know intimately. Open both:

**`nexo-plugin.toml`**:

```toml
[plugin]
id = "hello_plugin"
version = "0.1.0"
name = "Hello Plugin"
description = "Echoes inbound events back onto the broker."
min_nexo_version = ">=0.1.0"

[plugin.requires]
nexo_capabilities = ["broker"]

[[plugin.channels.register]]
kind = "hello_plugin_inbound"
description = "Inbound events the plugin emits onto the broker."

[plugin.entrypoint]
command = "./bin/hello_plugin"   # resolved relative to this file
```

`plugin.entrypoint.command = "./bin/hello_plugin"` is the
[Phase 31.1.c](./publishing.md) install convention. The daemon's
discovery walker reads the manifest, then spawns whatever
`command` resolves to, relative to the manifest's containing
directory. The pack-tarball step (step 8) copies your release
binary into `bin/hello_plugin` so this entrypoint resolves on
the operator host.

**`src/main.rs`** (truncated):

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
        .on_shutdown(|| async { Ok(()) })
        .run_stdio()
        .await?;
    Ok(())
}

async fn handle_event(topic: String, event: Event, broker: BrokerSender) {
    let echo = Event::new(
        "plugin.inbound.hello_plugin_echo",
        "hello_plugin",
        serde_json::json!({
            "echoed_from": topic,
            "echoed_payload": event.payload,
        }),
    );
    let _ = broker.publish("plugin.inbound.hello_plugin_echo", echo).await;
}
```

The contract is small: build a `PluginAdapter` from your manifest
text, register handlers, call `run_stdio().await`. The SDK owns
the JSON-RPC envelope — you only see decoded `Event`s.

> **Stdout discipline** — every byte on stdout must be a JSON-RPC
> frame. Use `eprintln!` / `tracing::*` for plugin-side logs; a
> stray `println!` will corrupt the wire and the daemon will tear
> the subprocess down at handshake.

## 3. Build

```bash
cargo build
```

A debug binary lands at `target/debug/hello_plugin` in well
under a second on a warm cache (mold + sccache, configured
machine-wide on the dev box, are not required — vanilla cargo
works too).

## 4. Smoke-test the handshake

Hand-feed a JSON-RPC `initialize` frame to the binary and verify
it replies with a well-formed manifest:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
    | ./target/debug/hello_plugin
```

Expected output (one line of JSON, formatted here for
readability):

```jsonc
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "manifest": { "plugin": { "id": "hello_plugin", ... } },
    "server_version": "hello_plugin-0.1.0",
    "tools": []
  }
}
```

If you see anything else — extra blank lines, panic backtrace,
malformed JSON — fix that before moving on. The daemon will
reject the same frame your terminal saw.

## 5. Local dev loop

Boot a daemon with this directory injected at the head of
`plugins.discovery.search_paths`. No install, no GitHub round
trip, no signature verification — pure inner-loop dev:

```bash
nexo plugin run .
```

Expected stderr trace:

```
INFO local plugin override applied (plugin_id=hello_plugin)
INFO subprocess plugin spawned (id=hello_plugin, pid=...)
INFO hello_plugin starting
INFO subprocess plugin handshake ok (id=hello_plugin, version=0.1.0)
```

The plugin is now live inside the daemon. Ctrl+C tears both
processes down cleanly.

Edit `src/main.rs`, re-run `cargo build`, and the daemon's
hot-reload walker (Phase 81.10) re-spawns the subprocess
automatically — no daemon restart.

For isolated contract debugging without your real
`agents.yaml`, add `--no-daemon-config`. See
[Local dev loop conventions](./authoring.md#local-dev-loop-conventions)
for the inner-loop reference.

## 6. Customize the handler

Replace the body of `handle_event` with whatever your plugin
should do — call a third-party API, persist to disk, trigger a
downstream agent. Re-publish the API's reply back through
`broker.publish` so agents can observe it.

Eight common shapes (channel plugin, poller, hybrid bridge,
etc.) are pre-baked in [Patterns](./patterns.md). Browse those
once before designing — most plugins fit one of those shapes
exactly.

## 7. Push to GitHub

```bash
gh repo create alice/hello_plugin --public --source=. --push
```

Or, with plain git:

```bash
git init && git add . && git commit -m "initial plugin"
git remote add origin git@github.com:alice/hello_plugin.git
git push -u origin main
```

The scaffolded repo already contains
`.github/workflows/release.yml` (the Phase 31.2 template). The
workflow fires on tag push (`v*`).

## 8. Cut a release

Bump the version in `Cargo.toml` and `nexo-plugin.toml` (must
match), then tag and push:

```bash
# bump both files first — keep them in lock-step
git add Cargo.toml nexo-plugin.toml
git commit -m "v0.1.0"
git tag v0.1.0
git push origin main v0.1.0
```

Watch the workflow in the GitHub Actions tab. The four jobs
shipped by the template:

1. **`validate-tag`** — refuses to release if the manifest
   version does not match the tag.
2. **`build`** — compiles the plugin for `linux-x64` and
   `macos-arm64` (extend the matrix yourself for more targets),
   runs `pack-tarball.sh` to produce the
   `bin/hello_plugin` + `nexo-plugin.toml` archive layout
   operators expect, and uploads `<id>-<version>-<target>.tar.gz`
   plus `.sha256` sidecars.
3. **`sign`** (optional, gated by repo variable
   `COSIGN_ENABLED`) — cosign keyless signature against the
   GitHub Actions OIDC identity. See
   [Signing & publishing](./signing-and-publishing.md) for the
   full keyless tutorial.
4. **`release`** — creates the GitHub Release, attaches every
   tarball, every `.sha256`, the bare `nexo-plugin.toml` (so
   `nexo plugin install` can resolve manifest URL early), and
   the optional cosign material.

When the workflow turns green, the release page contains the
exact asset shape `nexo plugin install` resolves against. See
[Publishing a plugin](./publishing.md) for the full asset naming
convention.

## 9. Install on an operator host

On any host running the daemon (could be the same machine — set
`<state_root>/plugins/` writable), run:

```bash
nexo plugin install alice/hello_plugin@v0.1.0
```

The installer (Phase 31.1.c):

1. Hits `https://api.github.com/repos/alice/hello_plugin/releases/tags/v0.1.0`.
2. Picks the tarball matching the host's target triple.
3. Downloads tarball + `.sha256`, verifies the digest.
4. Optionally verifies cosign signature (default off; flip on
   with `--require-signature` after configuring trusted keys —
   see [Plugin trust](../ops/plugin-trust.md)).
5. Extracts to
   `<state_root>/plugins/hello_plugin-0.1.0/{nexo-plugin.toml, bin/hello_plugin, .nexo-install.json}`.
6. Records the install in the per-host install ledger.

If your daemon already has
`plugins.discovery.search_paths` pointing at the
`<state_root>/plugins/` directory (the standard layout in the
shipped scaffold), the next discovery walk picks the new plugin
up — no restart required when Phase 81.10 hot-reload is on. On
a stock daemon, restart it to load the plugin.

## 10. Verify

```bash
nexo plugin list
```

Expected output:

```
ID            VERSION  TARGET                       CHANNEL  INSTALLED
hello_plugin  0.1.0    x86_64-unknown-linux-gnu     latest   2026-05-07T18:20:42+00:00
```

Daemon stderr should show the handshake within ~5 seconds:

```
INFO subprocess plugin spawned (id=hello_plugin, pid=...)
INFO subprocess plugin handshake ok (id=hello_plugin, version=0.1.0)
```

Publish anything onto an inbound topic the plugin subscribed to
(e.g. `agent broker publish`-equivalent inside your microapp)
and the echo lands on `plugin.inbound.hello_plugin_echo`.

## Iterate

```bash
# bump in source, push tag
git tag v0.1.1 && git push origin v0.1.1

# operator-side
nexo plugin upgrade hello_plugin            # pulls latest tag
# or pin: nexo plugin upgrade hello_plugin --version v0.1.1
```

`nexo plugin upgrade` (Phase 31.8) atomically swaps the on-disk
copy + restarts the subprocess inside the daemon. To roll back,
re-run `install` against the older tag.

To remove:

```bash
nexo plugin remove hello_plugin
```

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| `nexo plugin install` errors `no asset matching target <triple>`. | CI matrix did not build for the operator's host. | Add the missing triple to the workflow matrix, re-tag. |
| Daemon stderr shows `subprocess exited at handshake (status=...)`. | Plugin wrote non-JSON-RPC bytes to stdout (most likely a stray `println!`) or panicked before the handshake. | Re-run `./target/debug/<id>` against a synthetic frame from step 4 — the panic is reproducible there. |
| `nexo plugin list` does not show your plugin after install. | Daemon's `plugins.discovery.search_paths` does not include `<state_root>/plugins/`. | Add it to `config/plugins/discovery.yaml` and restart. |
| `nexo plugin install` errors `signature required`. | Operator runs with `--require-signature` and your release was unsigned. | Sign with cosign — see [Signing & publishing](./signing-and-publishing.md). |
| Plugin runs locally with `nexo plugin run` but the published binary panics on the operator host. | Per-target build skipped a runtime dep (e.g. linked OpenSSL on Linux but not on the operator's distro). | Switch to `vendored-openssl` or static-link in `Cargo.toml`; rebuild. |
| Operator on `--require-signature` rejects your release with `cosign verify failed`. | Trusted-keys file does not include your identity issuer. | Operator adds your GitHub identity to `config/extensions/trusted_keys.toml`. See [Plugin trust](../ops/plugin-trust.md). |

## Going deeper

You shipped one plugin. From here:

- [Plugin authoring overview](./authoring.md) — the full picture
  (plugin vs extension vs microapp, plugin config dir,
  sandboxing, contributing tools / channel kinds / LLM
  providers / memory backends / hooks).
- [Rust SDK reference](./rust-sdk.md) — full `PluginAdapter`
  surface, manifest schema, per-target tarball convention.
- [Plugin contract](./contract.md) — the wire spec every SDK
  implements. Read this once and you can debug any plugin in
  any language.
- [Patterns (8 common shapes)](./patterns.md) — pre-baked
  designs for channels, pollers, hybrid bridges.
- [Publishing a plugin](./publishing.md) — full asset naming
  convention and the 4-job CI workflow shape.
- [Signing & publishing](./signing-and-publishing.md) — cosign
  keyless tutorial.

## Other languages

Same flow, swap step 1's `--lang`:

| SDK | Scaffold | Reference |
|-----|----------|-----------|
| Python | `nexo plugin new hello --lang python --owner alice` | [Python SDK](./python-sdk.md) |
| TypeScript | `nexo plugin new hello --lang typescript --owner alice` | [TypeScript SDK](./typescript-sdk.md) |
| PHP | `nexo plugin new hello --lang php --owner alice` | [PHP SDK](./php-sdk.md) |

Steps 2–10 read identically; the source tree differs (no
`Cargo.toml`, language-appropriate runtime). The wire contract
is the same.
