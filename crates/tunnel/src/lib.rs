//! Public-HTTPS tunnel for Nexo agents.
//!
//! Exposes a local TCP port at a `https://*.trycloudflare.com`
//! URL so an agent can be reached from outside the LAN without
//! port-forwarding or static IPs. Used during WhatsApp pairing
//! (where the QR / link must be hit from the operator's phone)
//! and as a generic dev-time webhook receiver.
//!
//! ## v0.2 — pure-Rust backbone
//!
//! Internally this crate now wraps
//! [`cloudflare-quick-tunnel`](https://crates.io/crates/cloudflare-quick-tunnel),
//! which speaks QUIC + Cap'n Proto-RPC against Cloudflare's
//! `argotunnel` edge natively. The legacy `cloudflared` Go
//! subprocess strategy (auto-download, stderr scraping, ~30 MB
//! binary in the data dir) is gone — `cargo build` produces a
//! self-contained Rust binary, Android NDK / Termux cross-compile
//! works without any extra toolchain, and the tunnel surfaces
//! native typed errors, `bytes_in/out` counters, reconnect-on-
//! edge-drop, and a `unregisterConnection` RPC on graceful
//! shutdown.
//!
//! The crate's public types — [`TunnelManager`], [`TunnelHandle`],
//! plus the sidecar URL helpers — keep their pre-v0.2 shape, so
//! daemons + CLI tools that already use this crate keep building
//! unchanged.
//!
//! ## Sidecar URL accessor
//!
//! `nexo pair start` is a separate process from the daemon, so
//! the active URL is published to a file at
//! `$NEXO_HOME/state/tunnel.url` (or `~/.nexo/state/tunnel.url`
//! when `NEXO_HOME` is unset). The daemon writes the URL atomically
//! on tunnel-up and removes it on shutdown; the CLI reads it
//! directly. No daemon connection, no broker round-trip, no
//! shared library state.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use cloudflare_quick_tunnel::{
    QuickTunnelHandle as InnerHandle, QuickTunnelManager as InnerManager,
};
use tokio::sync::Mutex;

/// How long we wait for the edge to register before treating
/// `start()` as failed. Mirrors the legacy timeout so call sites
/// don't see a behavioural change.
pub const DEFAULT_URL_TIMEOUT: Duration = Duration::from_secs(30);

// ── Sidecar URL file ─────────────────────────────────────────────────────────

/// Canonical sidecar path. Honours `$NEXO_HOME` when set, falls
/// back to `~/.nexo/`. Directory is created on first write.
pub fn url_state_path() -> std::path::PathBuf {
    let home = std::env::var_os("NEXO_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".nexo")
        });
    home.join("state").join("tunnel.url")
}

/// Write the active tunnel URL to the sidecar file. Atomic —
/// write to `<path>.tmp` + rename so a CLI reading mid-write
/// never sees a torn URL.
pub fn write_url_file(url: &str) -> std::io::Result<()> {
    let path = url_state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("url.tmp");
    std::fs::write(&tmp, url.trim().as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Read the active tunnel URL from the sidecar file. Returns
/// `None` when the file is absent / empty / unreadable.
pub fn read_url_file() -> Option<String> {
    let path = url_state_path();
    let body = std::fs::read_to_string(&path).ok()?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Remove the sidecar file. Called on graceful daemon shutdown
/// so a stale URL doesn't outlive the tunnel that owns it.
pub fn clear_url_file() -> std::io::Result<()> {
    let path = url_state_path();
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ── TunnelHandle ─────────────────────────────────────────────────────────────

/// Public handle to a live tunnel. Holds the underlying pure-Rust
/// `cloudflare-quick-tunnel` handle behind a Mutex so we can keep
/// the legacy `shutdown(&self)` signature — the inner handle's
/// `shutdown` consumes `self`.
pub struct TunnelHandle {
    /// The `https://*.trycloudflare.com` URL Cloudflare assigned.
    pub url: String,
    inner: Arc<Mutex<Option<InnerHandle>>>,
}

impl TunnelHandle {
    /// Graceful shutdown — fires `unregisterConnection` on the
    /// control stream, waits for the reactor to drain, closes the
    /// QUIC connection. Best-effort; idempotent; safe to call
    /// multiple times.
    pub async fn shutdown(&self) {
        let mut guard = self.inner.lock().await;
        if let Some(handle) = guard.take() {
            if let Err(e) = handle.shutdown().await {
                tracing::warn!(error = %e, "tunnel shutdown reported an error");
            }
        }
    }
}

impl Drop for TunnelHandle {
    fn drop(&mut self) {
        // Best-effort synchronous teardown — drops the inner
        // handle, whose own Drop fires a non-blocking shutdown
        // signal to its reactor task. Callers should prefer
        // `shutdown().await` for the full graceful path.
        if let Ok(mut guard) = self.inner.try_lock() {
            let _ = guard.take();
        }
    }
}

// ── TunnelManager ────────────────────────────────────────────────────────────

pub struct TunnelManager {
    port: u16,
    timeout: Duration,
}

impl TunnelManager {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            timeout: DEFAULT_URL_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    /// Provision a fresh quick tunnel pointed at `127.0.0.1:<port>`
    /// and wait until the edge has registered the connection.
    /// Returns once the public URL is ready to serve traffic.
    pub async fn start(&self) -> Result<TunnelHandle> {
        tracing::info!(port = self.port, "starting pure-Rust quick tunnel");
        let inner = InnerManager::new(self.port)
            .with_timeout(self.timeout)
            .start()
            .await
            .context("cloudflare-quick-tunnel start failed")?;
        let url = inner.url.clone();
        tracing::info!(%url, location = %inner.location, "tunnel registered");
        Ok(TunnelHandle {
            url,
            inner: Arc::new(Mutex::new(Some(inner))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_state_path_honours_nexo_home() {
        // Sequential — std::env is process-global.
        let prev = std::env::var_os("NEXO_HOME");
        std::env::set_var("NEXO_HOME", "/tmp/test-nexo-home");
        let p = url_state_path();
        assert_eq!(p, std::path::PathBuf::from("/tmp/test-nexo-home/state/tunnel.url"));
        if let Some(v) = prev {
            std::env::set_var("NEXO_HOME", v);
        } else {
            std::env::remove_var("NEXO_HOME");
        }
    }

    #[test]
    fn read_returns_none_when_missing() {
        std::env::set_var("NEXO_HOME", "/tmp/test-nexo-home-missing");
        let _ = clear_url_file();
        assert!(read_url_file().is_none());
    }
}
