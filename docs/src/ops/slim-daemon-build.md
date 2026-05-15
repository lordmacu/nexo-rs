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
| `plugin-whatsapp` | ✅ on | `nexo-plugin-whatsapp` |
| `plugin-browser` | off | (no-op placeholder; browser already has no Cargo dep) |

`email` is NOT feature-gated — structurally in-process by
design (Phase 93.11 audit, bucket D). Autonomous worker +
EmailToolContext + `/metrics` rendering all hold
`Arc<EmailPlugin>` in-process. No subprocess driver today.

### whatsapp gate (93.12.c.1 + 93.12.c.2, shipped)

Both halves shipped — slim daemon binary can be built
without `nexo-plugin-whatsapp` in its compile graph:

```bash
cargo build --release --bin nexo --no-default-features
cargo tree --no-default-features -i nexo-plugin-whatsapp
# expected: error: package ID specification ... did not match any packages
```

Gated sites:

| Crate | Site | Detail |
|---|---|---|
| `src/main.rs` | `RuntimeHealth.wa_pairing` | typed `BTreeMap<String, SharedPairingState>` field |
| `src/main.rs` | `spawn_whatsapp_pairing_state_subscriber` | broker subscriber fn |
| `src/main.rs` | `spawn_whatsapp_typing_presence_subscriber` | typing-presence broker bridge fn |
| `src/main.rs` | `build_known_pairing_registry` | `WhatsappPairingAdapter::new` |
| `src/main.rs` | admin pairing trigger map | `WhatsappPairingTrigger::from_configs` |
| `src/main.rs` | instance loop | `wa_pairing` + `wa_tunnel_cfg` population |
| `src/main.rs` | subscriber spawn block | `spawn_whatsapp_pairing_state_subscriber` call |
| `src/main.rs` | tunnel auto-open | `/whatsapp/pair` Cloudflare quick tunnel |
| `src/main.rs` | tool fallback (boot) | `register_whatsapp_tools` |
| `src/main.rs` | tool fallback (hot-spawn) | `register_whatsapp_tools` |
| `src/main.rs` | HTTP handler | `/whatsapp/*` route dispatcher |
| `crates/setup/src/writer.rs` | pairing flow | `session::pair_once` + helpers + dual-shape `wipe_channel_session` |
| `crates/setup/src/admin_bootstrap.rs` | admin RPC | `with_wa_bot_handle` + outbound translator |
| `crates/setup/src/admin_adapters.rs` | outbound translator | `WhatsAppTranslator` struct + impl + tests |
| `crates/setup/tests/channel_outbound_end_to_end.rs` | e2e test | file-level `#![cfg]` |

Runtime impact when `--no-default-features`:

- Admin RPC `/whatsapp/*` returns channel-unavailable.
- HTTP `/whatsapp/*` route returns 404 (handler block absent).
- Auto-open Cloudflare quick tunnel for pairing is skipped.
- Pairing trigger map has no whatsapp entry — `admin pairing/start`
  returns "channel not supported".
- Outbound dispatcher rejects whatsapp routes with typed
  `TranslationError::UnsupportedChannel`.

WhatsApp still runs as a discovered subprocess if its
manifest sits in `plugins.discovery.search_paths` and the
binary is installed — the gate removes only compile-time
imports. Subprocess broker path is unaffected.

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
