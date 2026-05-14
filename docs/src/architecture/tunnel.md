# Pure-Rust quick tunnel

`nexo-tunnel-quick` exposes a local TCP port over
`https://*.trycloudflare.com` without the `cloudflared` Go
subprocess. It is the public-HTTPS plumbing the daemon needs for
WhatsApp QR pairing and dev-time webhook receivers.

Phase 92 lineage:

- `cloudflare-quick-tunnel` (crates.io, upstream) — QUIC + Cap'n
  Proto-RPC client against Cloudflare's `argotunnel` edge.
- `nexo-tunnel-quick` (this crate) — workspace wrapper that adds
  the sidecar URL file accessor, the lifecycle metrics module,
  and surfaces the supervisor knobs through a stable API.
- `nexo-tunnel` (legacy alias, v0.3.x) — re-exports
  `nexo-tunnel-quick::*` verbatim until Phase 92.11 retires it.

## Public API

```rust
use std::time::Duration;
use nexo_tunnel_quick::{TunnelManager, DEFAULT_GRACE_PERIOD};

let handle = TunnelManager::new(8080)
    .with_timeout(Duration::from_secs(30))
    .start()
    .await?;

println!("public URL: {}", handle.url);
println!("edge POP : {}", handle.location);
println!("tunnel id: {}", handle.tunnel_id);

if let Some(m) = handle.metrics().await {
    println!(
        "streams={} in={} out={} reconnects={}",
        m.streams_total, m.bytes_in, m.bytes_out, m.reconnects,
    );
}

handle.shutdown_with(DEFAULT_GRACE_PERIOD).await;
```

## Supervisor

Heartbeat, reconnect-with-backoff and graceful `unregisterConnection`
all run inside the upstream supervisor (a Tokio task owned by
`QuickTunnelHandle`). Visible knobs:

| Constant                       | Value | Role                                         |
|--------------------------------|-------|----------------------------------------------|
| `DEFAULT_HANDSHAKE_TIMEOUT`    | 30 s  | wait for edge to register before failing     |
| `DEFAULT_GRACE_PERIOD`         | 30 s  | drain in-flight streams during shutdown      |
| `MAX_RECONNECT_ATTEMPTS`       | 10    | consecutive supervisor reconnect failures    |

`MAX_RECONNECT_ATTEMPTS` exhaustion surfaces as
`TunnelError::PermanentFailure(attempts)` on the next `metrics()`
poll (the supervisor task closes itself; the handle still answers
`shutdown()` cleanly).

## Telemetry (Phase 92.10)

Process-wide lifecycle counters live in `nexo_tunnel_quick::metrics`
and follow the same lock-free pattern as Phase 86
(`nexo-memory::metrics`): `LazyLock<AtomicU64>` / `LazyLock<DashMap>`
storage, hand-rolled Prometheus text rendering, no `prometheus`
crate dep, no metrics server here. Operators stitch the renderer
output into the runtime's `/metrics` aggregator.

| Counter                              | Labels      | Meaning                                                 |
|--------------------------------------|-------------|---------------------------------------------------------|
| `tunnel_starts_total`                | —           | successful `TunnelManager::start` calls                 |
| `tunnel_starts_failed_total`         | `reason`    | `api` / `discovery` / `quic_dial` / `register` / …      |
| `tunnel_shutdowns_total`             | —           | graceful `TunnelHandle::shutdown_with` invocations      |
| `tunnel_streams_total`               | `tunnel_id` | streams proxied (per tunnel, supervisor counter)        |
| `tunnel_bytes_in_total`              | `tunnel_id` | bytes edge → local                                      |
| `tunnel_bytes_out_total`             | `tunnel_id` | bytes local → edge                                      |
| `tunnel_reconnects_total`            | `tunnel_id` | supervisor reconnect cycles                             |

Two render entry points:

```rust
// Lifecycle counters only — no per-handle snapshot.
let text = nexo_tunnel_quick::metrics::render_prometheus();

// Lifecycle + per-tunnel supervisor counters from live handles.
let text = nexo_tunnel_quick::metrics::render_prometheus_for(&[&h1, &h2]).await;
```

`reason` cardinality is capped at 16 distinct labels — extra ones
collapse to `"other"` so a misbehaving error path can't blow up
Prometheus storage.

## Sidecar URL file

`nexo pair start` is a separate process from the daemon, so the
active URL is published to a file at:

- `$NEXO_HOME/state/tunnel.url` when `NEXO_HOME` is set, otherwise
- `~/.nexo/state/tunnel.url`.

The daemon writes atomically (`<path>.tmp` + rename) on tunnel-up
and removes on shutdown:

```rust
use nexo_tunnel_quick::{write_url_file, read_url_file, clear_url_file};

write_url_file(&handle.url)?;
let active = read_url_file();
clear_url_file()?;
```

No daemon connection, no broker round-trip, no shared library
state — the CLI reads the file directly.
