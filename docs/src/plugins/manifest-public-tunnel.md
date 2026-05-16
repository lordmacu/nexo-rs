# `[plugin.public_tunnel]` — manifest-driven Cloudflare quick tunnel

> Phase 81.20.x Stage 7 Phase 2. Status: introduced 2026-05-16.

Plugins that expose HTTP routes the operator might want to reach
from outside the LAN (e.g. WhatsApp pairing while the daemon runs
on a desktop and the QR is scanned on a phone) declare this section.
The daemon spawns a [Cloudflare quick
tunnel](https://crates.io/crates/cloudflare-quick-tunnel) pointed
at its HTTP port; plugin routes exposed via
[`[plugin.http]`](./manifest-http.md) become reachable at
`https://*.trycloudflare.com/<plugin-mount-prefix>/...`.

## Two-key opt-in

The daemon opens a public tunnel only when **both** are true:

1. **Plugin manifest:** `[plugin.public_tunnel] enabled = true`.
2. **Operator capability env:**
   `NEXO_PLUGIN_PUBLIC_TUNNEL_ALLOW=1`.

A manifest declaration alone is not enough — the daemon still
honours the operator's hardening choice. Declaring the section
with `enabled = false` (or omitting it) keeps the plugin
forever-private even if the operator flips the env on for another
plugin.

## Manifest section

```toml
[plugin.public_tunnel]
enabled = true
# Optional — when set, the daemon subscribes to this exact broker
# subject and closes the tunnel on the first published message.
# Wildcards (`*`, `>`) are rejected at manifest validation.
close_on_event = "plugin.lifecycle.whatsapp.tunnel_done"
```

### Fields

- `enabled` *(bool, default `false`)* — plugin-side opt-in. When
  `false`, the section behaves identically to "no section
  declared".
- `close_on_event` *(string, optional)* — literal broker subject.
  When set, the daemon subscribes once and tears the tunnel down
  on the first inbound message. Typical use: a pairing-channel
  plugin publishes `<plugin>.tunnel_done` after the operator
  completes pairing so the public URL stops responding
  immediately. When `None`, the tunnel stays up for the daemon's
  lifetime.

## Validation

Rejected at boot:

- `close_on_event` is the empty string or whitespace
  (`PublicTunnelCloseEventEmpty`).
- `close_on_event` contains a NATS wildcard (`*` or `>`). The
  daemon refuses wildcards so a stray plugin event can't
  race-close a healthy tunnel
  (`PublicTunnelCloseEventWildcard`).

## URL sidecar file

When a tunnel comes up, the daemon writes the public URL to
`$NEXO_HOME/state/tunnel.url` (or `~/.nexo/state/tunnel.url`).
The `nexo pair start` CLI reads this file directly so the operator
doesn't have to copy/paste from logs.

## Migration from `wa_tunnel_cfg`

Earlier daemon revisions extracted `public_tunnel` from each
WhatsApp YAML entry under `config/plugins/whatsapp.yaml` and
spawned a Cloudflare tunnel inline. That orchestration was
removed (Phase 81.20.x Stage 7 Phase 2) — plugins now declare the
intent in their manifest, the daemon iterates uniformly, and the
operator's env flag stays authoritative.

## Daemon-side wiring

`main.rs` iterates `wire.plugin_handles` after
`wire_plugin_registry` returns. For every plugin with
`[plugin.public_tunnel] enabled = true`, the daemon:

1. Spawns `nexo_tunnel_quick::TunnelManager::new(8080).start()`.
2. Logs the URL + writes it to the sidecar file.
3. If `close_on_event` is set, spawns a broker subscriber that
   awaits one message then calls `tunnel.shutdown().await`.

The capability gate is checked once at the top of the iterator
block. When OFF, the iterator logs a single informational line
("declared by at least one plugin but env is not set") and skips
every tunnel spawn — useful for hardened deployments that want a
visible audit trail.

## Threat model

`https://*.trycloudflare.com/<plugin-prefix>/...` is reachable
from anywhere the URL is shared. Cloudflare provides DDoS
protection + edge TLS but does NOT enforce authentication. The
plugin's HTTP handler is responsible for any access control on
the exposed paths.

Pairing pages are time-limited (the QR rotates ~every 20s, the
challenge expires in 5 minutes). When `close_on_event` is set,
the tunnel is teardown-immediate post-pairing — the URL becomes
404 the moment the plugin signals completion.

## Validation commands

- `cargo nextest run -p nexo-plugin-manifest` — manifest schema
  + validator tests.
- Manual smoke: `NEXO_PLUGIN_PUBLIC_TUNNEL_ALLOW=1 cargo run --bin nexo`
  with a manifest declaring the section. Daemon log should show
  `"public tunnel up"` with the assigned URL.
