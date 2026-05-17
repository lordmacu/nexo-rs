//! Public-HTTPS tunnel for Nexo agents — Phase 92 first-party home.
//!
//! Exposes a local TCP port at a `https://*.trycloudflare.com` URL so
//! an agent can be reached from outside the LAN without
//! port-forwarding or static IPs. Used during WhatsApp pairing
//! (where the QR / link must be hit from the operator's phone) and
//! as a generic dev-time webhook receiver.
//!
//! ## Backbone
//!
//! Internally this crate wraps
//! [`cloudflare-quick-tunnel`](https://crates.io/crates/cloudflare-quick-tunnel),
//! which speaks QUIC + Cap'n Proto-RPC against Cloudflare's
//! `argotunnel` edge natively — no `cloudflared` Go subprocess, no
//! ~30 MB binary in the data dir, Android NDK / Termux cross-compile
//! works without any extra toolchain.
//!
//! ## Sidecar URL accessor
//!
//! `nexo pair start` is a separate process from the daemon, so the
//! active URL is published to a file at
//! `$NEXO_HOME/state/tunnel.url` (or `~/.nexo/state/tunnel.url` when
//! `NEXO_HOME` is unset). The daemon writes the URL atomically on
//! tunnel-up and removes it on shutdown; the CLI reads it directly.
//! No daemon connection, no broker round-trip, no shared library
//! state.
//!
//! ## Phase 92 lineage
//!
//! `nexo-tunnel` (legacy) re-exports this crate verbatim starting in
//! Phase 92.8. Phase 92.11 retires the legacy alias.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use cloudflare_quick_tunnel::{
    QuickTunnelHandle as InnerHandle, QuickTunnelManager as InnerManager,
};
use tokio::sync::Mutex;

pub use cloudflare_quick_tunnel::manager::{
    DEFAULT_GRACE_PERIOD, DEFAULT_HANDSHAKE_TIMEOUT, MAX_RECONNECT_ATTEMPTS,
};
pub use cloudflare_quick_tunnel::{
    QuickTunnelHandle, QuickTunnelManager, TunnelError, TunnelMetrics,
};

pub mod metrics;

/// Map a [`TunnelError`] to a bounded `&'static str` label for
/// `tunnel_starts_failed_total{reason=…}`. Variants the caller
/// didn't introduce (e.g. `Shutdown` on the start path) collapse
/// to `"other"`.
fn failure_reason_label(e: &TunnelError) -> &'static str {
    match e {
        TunnelError::Api(_) => "api",
        TunnelError::ApiBusiness(_) => "api_business",
        TunnelError::ApiNonJson { .. } => "api_non_json",
        TunnelError::Discovery(_) => "discovery",
        TunnelError::QuicDial { .. } => "quic_dial",
        TunnelError::Register(_) => "register",
        TunnelError::PermanentFailure(_) => "permanent",
        TunnelError::Internal(_) => "internal",
        TunnelError::Shutdown => "other",
    }
}

/// Crate version stamp — useful for tracing the runtime origin
/// after `nexo-tunnel` flips to re-exporting from here.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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
///
/// The supervisor (Phase 92.7) is owned by the upstream reactor —
/// it keeps a single QUIC connection alive, drives the heartbeat,
/// and reconnects with backoff on edge drop (up to
/// [`MAX_RECONNECT_ATTEMPTS`] consecutive failures). Reconnects
/// are visible in `metrics().reconnects`.
pub struct TunnelHandle {
    /// The `https://*.trycloudflare.com` URL Cloudflare assigned.
    pub url: String,
    /// Tunnel UUID minted by the Cloudflare API. Stable for the
    /// lifetime of this handle (reused across reconnects).
    pub tunnel_id: String,
    /// Account tag the edge advertised on register. Informational.
    pub account_tag: String,
    /// Edge POP that registered the connection (`"DFW"`, `"SJC"`,
    /// …). Updates only on full reconnect; surfaced for telemetry.
    pub location: String,
    inner: Arc<Mutex<Option<InnerHandle>>>,
}

impl TunnelHandle {
    /// Snapshot of the supervisor's counters — stream count,
    /// proxied byte totals, completed reconnect cycles. Cheap;
    /// safe to poll on a tracing/OTel cadence.
    pub async fn metrics(&self) -> Option<TunnelMetrics> {
        let guard = self.inner.lock().await;
        guard.as_ref().map(|h| h.metrics())
    }

    /// Graceful shutdown — fires `unregisterConnection` on the
    /// control stream, waits for the reactor to drain, closes the
    /// QUIC connection. Best-effort; idempotent; safe to call
    /// multiple times.
    pub async fn shutdown(&self) {
        self.shutdown_with(DEFAULT_GRACE_PERIOD).await;
    }

    /// Graceful shutdown with an explicit grace deadline for the
    /// supervisor to drain in-flight streams. Defaults to
    /// [`DEFAULT_GRACE_PERIOD`] when callers use [`shutdown`].
    pub async fn shutdown_with(&self, grace: Duration) {
        let mut guard = self.inner.lock().await;
        if let Some(handle) = guard.take() {
            metrics::record_shutdown();
            if let Err(e) = handle.shutdown_with(grace).await {
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
        let inner = match InnerManager::new(self.port)
            .with_timeout(self.timeout)
            .start()
            .await
        {
            Ok(h) => h,
            Err(e) => {
                metrics::record_start_failure(failure_reason_label(&e));
                return Err(anyhow::Error::new(e).context("cloudflare-quick-tunnel start failed"));
            }
        };
        metrics::record_start_success();
        let url = inner.url.clone();
        let tunnel_id = inner.tunnel_id.to_string();
        let account_tag = inner.account_tag.clone();
        let location = inner.location.clone();
        tracing::info!(%url, %tunnel_id, %location, "tunnel registered");
        Ok(TunnelHandle {
            url,
            tunnel_id,
            account_tag,
            location,
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
        assert_eq!(
            p,
            std::path::PathBuf::from("/tmp/test-nexo-home/state/tunnel.url")
        );
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
