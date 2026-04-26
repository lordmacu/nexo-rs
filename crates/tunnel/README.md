# nexo-tunnel

> Cloudflare Tunnel manager + sidecar URL accessor for Nexo agents — exposes a local agent over HTTPS without opening firewall ports.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **Spawns `cloudflared` as a managed subprocess** that opens a free
  `https://*.trycloudflare.com` tunnel pointing at the local agent's
  admin port (default 8080).
- **Auto-downloads `cloudflared`** on first launch (Termux + Linux
  + macOS) with sha256 verification + supply-chain-safe tarball
  extraction (rejects `..` / absolute-path entries).
- **Parses the public URL** off `cloudflared` stderr and surfaces
  it via `TunnelHandle::url`. The runtime then prints a hard-to-
  miss banner so operators can paste the URL into WhatsApp pairing
  / webhook forms without scraping logs.
- **Sidecar URL accessor** — `write_url_file`, `read_url_file`,
  `clear_url_file` over `$NEXO_HOME/state/tunnel.url`. Bridges the
  daemon ↔ CLI process boundary so a separately-launched
  `nexo pair start` picks up the active URL without env-var
  coordination. Atomic writes via `<path>.tmp + rename`.
- **Graceful shutdown** — `TunnelHandle::shutdown().await` kills
  the subprocess and joins. Drop fallback handles SIGTERM-on-
  parent-death so a forgotten tunnel doesn't leak after a panic.

## Architecture

```
   nexo daemon (process A)               nexo pair start (process B)
   ─────────────────────────             ─────────────────────────
   TunnelManager::new(8080)              read_url_file()
        ↓
   start() → spawn cloudflared              ─→ Some("https://abc.tr…")
        ↓                                       ↓
   TunnelHandle { url, child }            opens WS pairing URL
        ↓
   write_url_file(&url)
   $NEXO_HOME/state/tunnel.url ◄────── read by process B
```

## Public API

| Item | Purpose |
|---|---|
| `TunnelManager::new(port)` | Build a manager bound to a local port |
| `TunnelManager::with_timeout(d)` | Override the URL-discovery timeout (default 30s) |
| `TunnelManager::start() -> TunnelHandle` | Launch + wait for the public URL |
| `TunnelHandle::url` | The `https://*.trycloudflare.com` URL |
| `TunnelHandle::shutdown().await` | Graceful kill + join |
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
nexo-tunnel = "0.1"
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

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
