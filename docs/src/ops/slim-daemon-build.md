# Slim daemon builds (Cargo feature-gates)

Phase 93.12.a (2026-05-15) introduced Cargo feature-gates
for canonical plugin crates so operators targeting embedded
or mobile (Android Flutter FFI, slim Docker images) can ship
a daemon binary without the optional plugin crates in its
compile graph.

## Available features

| Feature | Default | Drops crate |
|---|---|---|
| `plugin-telegram` | ✅ on | `nexo-plugin-telegram` |
| `plugin-whatsapp` | ✅ on | `nexo-plugin-whatsapp` (setup-crate scaffolding shipped 93.12.c.1; daemon main.rs gates pending 93.12.c.2) |
| `plugin-browser` | off | (no-op placeholder; browser already has no Cargo dep) |

`email` is NOT feature-gated — structurally in-process by
design (Phase 93.11 audit, bucket D). Autonomous worker +
EmailToolContext + `/metrics` rendering all hold
`Arc<EmailPlugin>` in-process. No subprocess driver today.

### whatsapp gate status

Phase 93.12.c.1 shipped the cross-crate scaffolding:

- `nexo-plugin-whatsapp` moved to `optional = true` in
  workspace deps + daemon deps.
- `crates/setup/Cargo.toml` gains a `plugin-whatsapp` feature.
- The daemon's `plugin-whatsapp` feature forwards through
  `nexo-setup/plugin-whatsapp` so setup compiles whatsapp-less.
- 3 setup-side import sites cfg-gated:
  `writer.rs` (`session::pair_once` pairing flow),
  `admin_bootstrap.rs` (`with_wa_bot_handle` admin RPC),
  `admin_adapters.rs` (`WhatsAppTranslator` + outbound
  topic constant).

Validated: `cargo check -p nexo-setup --no-default-features`
green AND `cargo check -p nexo-setup --features plugin-whatsapp`
green. Setup-crate tests 317/317 with default features.

**Phase 93.12.c.2 (pending)** — daemon main.rs sites. The
remaining ~10 blocks include `RuntimeHealth.wa_pairing`
typed field (`BTreeMap<String, SharedPairingState>`), the
instance-loop population (~25 LOC), subscriber spawn
(~30 LOC), HTTP `/whatsapp/*` route dispatcher (~30 LOC),
pairing trigger registration, pairing adapter constructor,
register_whatsapp_tools fallbacks (boot + hot-spawn). Estimated
~6-8h in a dedicated session. Until 93.12.c.2 lands,
`cargo build --bin nexo --no-default-features` will fail
fast on the whatsapp typed-import sites; the daemon ships
with `plugin-whatsapp` enabled by default.

## Building a telegram-less daemon

```bash
cargo build --release --bin nexo --no-default-features
```

Verify the crate dropped from the dep graph:

```bash
cargo tree --no-default-features -i nexo-plugin-telegram
# expected: error: package ID specification `nexo-plugin-telegram` did not match any packages
```

`cargo tree -i nexo-plugin-telegram` (without
`--no-default-features`) prints the canonical `nexo-rs v0.1.x`
parent — proving the gate is the *only* thing keeping
telegram in.

## Runtime behaviour

A feature-gated build still runs telegram as a **discovered
subprocess** if its manifest sits in
`plugins.discovery.search_paths` and the
`nexo-plugin-telegram` binary is installed (via
`cargo install nexo-plugin-telegram` or release tarball).
The gate removes only the daemon's compile-time imports
(pairing adapter constructor + outbound-tool fallback
registration). The subprocess path uses broker JSON-RPC,
not direct Rust imports, so it is unaffected.

Tradeoff: the feature-disabled daemon loses the daemon-side
fallback that registers `telegram_*` outbound tools into
the agent's `ToolRegistry` if the plugin manifest does not
yet declare `[[plugin.tools.outbound]]`. Standalone telegram
v0.3.0+ ships the manifest section, so the fallback is dead
weight for any operator running a current plugin binary.

## CI matrix

The release workflow validates both shapes:

```bash
cargo build --bin nexo                        # default (telegram in)
cargo build --bin nexo --no-default-features  # slim (telegram out)
```

Both targets must compile clean for release-fast and
release profiles before the binary ships.

## When to add a new feature-gate

Add `plugin-<id> = ["dep:nexo-plugin-<id>"]` if:

1. The plugin has a non-trivial Cargo dep with transitive
   cost (binary size, link time, native dep like OpenSSL).
2. The plugin is genuinely optional for the target audience
   (Android, embedded, slim Docker).
3. The compile-time integration points are localised — no
   cross-crate admin-RPC entanglement that would force the
   gate to bubble through `crates/setup` or `crates/core`.

If any of (1)-(3) fail, prefer subprocess discovery over a
feature-gate — manifest-driven runtime decoupling avoids
the conditional-compilation noise.
