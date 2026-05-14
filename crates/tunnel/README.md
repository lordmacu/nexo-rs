# nexo-tunnel

> Public-HTTPS tunnel + sidecar URL accessor for Nexo agents — exposes
> a local agent over `https://*.trycloudflare.com` with **no
> `cloudflared` subprocess and no Go binary**.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** —
a multi-agent Rust framework with a NATS event bus, pluggable LLM
providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek),
per-agent credentials, MCP support, and channel plugins for
WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What changed in v0.2

The crate is now a thin façade over
[`cloudflare-quick-tunnel`](https://crates.io/crates/cloudflare-quick-tunnel),
a pure-Rust QUIC + Cap'n Proto-RPC client we wrote to speak the
trycloudflare `argotunnel` protocol natively. The legacy strategy
— shelling out to a downloaded `cloudflared` Go binary and scraping
its stderr — is gone.

Net effect:

- **Zero runtime deps.** No 30 MB binary in the user data dir. No
  first-launch download from GitHub Releases. No sha256 cache.
- **Android NDK + Termux + WASM** cross-compile without an extra
  toolchain. The whole tunnel is Rust.
- **Typed errors + telemetry**: `bytes_in/out`, `streams_total`,
  `reconnects` instead of log scraping.
- **Reconnect on edge drop** with exponential backoff.
- **Graceful `unregisterConnection`** RPC on shutdown.

The public API (`TunnelManager` / `TunnelHandle` / sidecar URL
helpers) is unchanged, so callers from v0.1.x keep building.

## What this crate does

- **Provisions a free `https://<sub>.trycloudflare.com` URL** and
  routes inbound HTTP/1.1 + HTTP/2 requests to the local TCP
  listener you point it at.
- **Owns a reactor task** that keeps the QUIC + capnp-RPC control
  stream alive, accepts inbound streams, and reconnects if the
  edge POP drops the connection.
- **Sidecar URL accessor** — `write_url_file`, `read_url_file`,
  `clear_url_file` over `$NEXO_HOME/state/tunnel.url`. Bridges
  the daemon ↔ CLI process boundary so a separately-launched
  `nexo pair start` picks up the active URL without env-var
  coordination. Atomic writes via `<path>.tmp + rename`.
- **Graceful shutdown** — `TunnelHandle::shutdown().await` fires
  `unregisterConnection` with a 30s grace and joins the reactor
  task. Drop falls back to fire-and-forget signal.

## Architecture

```
   nexo daemon (process A)               nexo pair start (process B)
   ─────────────────────────             ─────────────────────────
   TunnelManager::new(8080)              read_url_file()
        ↓
   start()                                  ─→ Some("https://abc.tr…")
   │                                            ↓
   ├─ POST api.trycloudflare.com/tunnel       opens WS pairing URL
   ├─ SRV discover argotunnel edges
   ├─ QUIC dial (rustls + 3 CF roots)
   ├─ capnp-RPC RegisterConnection
   └─ spawn reactor (accept_bi loop)
        ↓
   TunnelHandle { url, inner: QuickTunnelHandle }
        ↓
   write_url_file(&url)
   $NEXO_HOME/state/tunnel.url ◄────── read by process B
```

## Public API

| Item | Purpose |
|---|---|
| `TunnelManager::new(port)` | Build a manager bound to a local port |
| `TunnelManager::with_timeout(d)` | Override the start-up budget (default 30s) |
| `TunnelManager::start() -> TunnelHandle` | Provision + register + spawn reactor |
| `TunnelHandle::url` | The `https://*.trycloudflare.com` URL |
| `TunnelHandle::shutdown().await` | Graceful unregister + reactor join |
| `url_state_path() -> PathBuf` | Canonical sidecar path |
| `write_url_file(url)` | Daemon-side write (atomic) |
| `read_url_file() -> Option<String>` | CLI-side read |
| `clear_url_file()` | Idempotent removal on shutdown |

## Quick start

```rust
use nexo_tunnel::TunnelManager;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let handle = TunnelManager::new(8080).start().await?;
    println!("Public URL: {}", handle.url);
    // … keep handle alive while the agent runs …
    handle.shutdown().await;
    Ok(())
}
```

## Install

```toml
[dependencies]
nexo-tunnel = "0.2"
```

## When to use this crate vs not

- ✅ Personal-agent on Termux that needs inbound WhatsApp / webhook
  callbacks without a public IP.
- ✅ Local development — exposes your dev agent so a teammate can
  hit it from their phone for testing.
- ❌ Production deployments with a real domain — use a proper
  reverse proxy (nginx + Let's Encrypt, or a load balancer in front
  of the agent). See [Hetzner deploy recipe](https://lordmacu.github.io/nexo-rs/recipes/deploy-hetzner.html).
- ❌ Anything that needs a stable URL across restarts — Cloudflare
  rotates the `*.trycloudflare.com` subdomain on every launch.

## Documentation for this crate

- [Termux install](https://lordmacu.github.io/nexo-rs/getting-started/install-termux.html)
- [Pairing protocol](https://lordmacu.github.io/nexo-rs/ops/pairing.html)
- Upstream: [`cloudflare-quick-tunnel`](https://crates.io/crates/cloudflare-quick-tunnel)
  for the wire-level details + protocol notes (ALPN, SNI, CF
  internal CAs, capnp framing).

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
