//! Cross-platform Cloudflare Tunnel launcher.
//!
//! Gives the agent a public HTTPS URL over `trycloudflare.com` without
//! requiring the operator to install or invoke `cloudflared` manually.
//! The manager:
//!
//! 1. Locates the `cloudflared` binary. First probes `$PATH`; if the
//!    binary is missing, downloads the release asset appropriate for
//!    the current platform (Linux amd64/arm64, macOS amd64/arm64,
//!    Windows amd64) and caches it under the user's platform data
//!    directory so subsequent runs skip the download.
//! 2. Spawns `cloudflared tunnel --url http://127.0.0.1:<port>` as a
//!    managed subprocess.
//! 3. Scrapes stderr looking for the `https://…trycloudflare.com` URL
//!    Cloudflare prints once the tunnel is live.
//! 4. Exposes that URL through a small [`TunnelHandle`] whose `Drop`
//!    + explicit [`TunnelHandle::shutdown`] reliably terminate the
//!      child.
//!
//! The crate is tunnel-framework agnostic within its contract — nothing
//! here knows about WhatsApp. Integrators call `TunnelManager::start`
//! during boot, stash the returned handle, and print the resulting URL
//! (plus the `/whatsapp/pair` suffix if that's their use case).

pub mod binary;

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};

pub use binary::ensure_cloudflared;

/// How long we wait for `cloudflared` to print its tunnel URL before
/// treating the spawn as a failure.
pub const DEFAULT_URL_TIMEOUT: Duration = Duration::from_secs(30);

// ── Sidecar URL file ──────────────────────────────────────────────────────────
//
// FOLLOWUPS PR-3 — in-process URL accessor across daemon ↔ CLI
// boundaries.
//
// `nexo pair start` is a separate process from the daemon, so a
// real in-process accessor needs IPC. The cheapest IPC that
// works under systemd, Docker, and ad-hoc shells is a sidecar
// file: the daemon writes the active URL on tunnel-up, removes
// it on shutdown, and the CLI reads it directly. No daemon
// connection, no broker round-trip, no shared library state.
//
// The file lives at `$NEXO_HOME/state/tunnel.url` (or
// `~/.nexo/state/tunnel.url` when `NEXO_HOME` is unset). One
// canonical location so deployments don't have to coordinate
// path conventions.

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

/// Write the active tunnel URL to the sidecar file. Used by the
/// daemon after `TunnelManager::start()` succeeds. Atomic — write
/// to `<path>.tmp` + rename so a CLI reading mid-write never sees
/// a torn URL.
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
/// `None` when the file is absent / empty / unreadable. Used by
/// `nexo pair start` and other CLI subcommands that need the URL
/// without a daemon connection.
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

/// Public handle to a live tunnel. Drop or [`shutdown`] kills the
/// `cloudflared` subprocess.
pub struct TunnelHandle {
    /// The `https://*.trycloudflare.com` URL Cloudflare assigned.
    pub url: String,
    child: Arc<Mutex<Option<Child>>>,
}

impl TunnelHandle {
    pub async fn shutdown(&self) {
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

impl Drop for TunnelHandle {
    fn drop(&mut self) {
        // Best-effort synchronous kill. Callers should prefer
        // `shutdown().await` during graceful shutdown to also join.
        if let Ok(mut guard) = self.child.try_lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
            }
        }
    }
}

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

    /// Ensure cloudflared is available, spawn the tunnel, and wait
    /// until we've seen the public URL on stderr.
    pub async fn start(&self) -> Result<TunnelHandle> {
        let bin = binary::ensure_cloudflared().await?;
        tracing::info!(binary = %bin.display(), port = self.port, "launching cloudflared tunnel");

        let mut cmd = Command::new(&bin);
        cmd.arg("tunnel")
            .arg("--url")
            .arg(format!("http://127.0.0.1:{}", self.port))
            .arg("--no-autoupdate")
            // Cloudflared's default logger splits `info` vs `err`; the
            // URL lands on both but keeping stderr lets us avoid stdout
            // buffering weirdness on Windows.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn {}", bin.display()))?;

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("cloudflared stderr not captured"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("cloudflared stdout not captured"))?;

        let (tx, rx) = oneshot::channel::<String>();
        let tx = Arc::new(Mutex::new(Some(tx)));

        // stderr reader — this is the channel cloudflared usually
        // prints the `https://…trycloudflare.com` line to.
        {
            let tx = tx.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "cloudflared.stderr", "{line}");
                    if let Some(url) = extract_trycloudflare_url(&line) {
                        if let Some(sender) = tx.lock().await.take() {
                            let _ = sender.send(url);
                        }
                    }
                }
            });
        }
        // stdout reader — mirrors to our tracing for debugging and
        // doubles as a URL source when cloudflared changes log routing
        // between versions.
        {
            let tx = tx.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "cloudflared.stdout", "{line}");
                    if let Some(url) = extract_trycloudflare_url(&line) {
                        if let Some(sender) = tx.lock().await.take() {
                            let _ = sender.send(url);
                        }
                    }
                }
            });
        }

        let url = match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(url)) => url,
            Ok(Err(_)) => {
                let _ = child.start_kill();
                bail!("cloudflared exited before printing a tunnel URL");
            }
            Err(_) => {
                let _ = child.start_kill();
                bail!(
                    "timed out after {}s waiting for cloudflared to open a tunnel",
                    self.timeout.as_secs()
                );
            }
        };

        tracing::info!(url = %url, "cloudflared tunnel up");
        Ok(TunnelHandle {
            url,
            child: Arc::new(Mutex::new(Some(child))),
        })
    }
}

/// Pull the first `https://*.trycloudflare.com` URL off a log line.
/// Cloudflared's line format has shifted across versions (ANSI boxes,
/// plain "INFO …", JSON) — a single regex covers them all.
fn extract_trycloudflare_url(line: &str) -> Option<String> {
    use regex::Regex;
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"https://[a-zA-Z0-9._-]+\.trycloudflare\.com").unwrap());
    re.find(line).map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_boxed_url() {
        let s = "|  https://metro-fifteen-prince-solo.trycloudflare.com                       |";
        assert_eq!(
            extract_trycloudflare_url(s).as_deref(),
            Some("https://metro-fifteen-prince-solo.trycloudflare.com")
        );
    }

    #[test]
    fn parses_info_line() {
        let s = "2026-04-23T22:00:00Z INF +--- https://ab-cd.trycloudflare.com ---+";
        assert_eq!(
            extract_trycloudflare_url(s).as_deref(),
            Some("https://ab-cd.trycloudflare.com")
        );
    }

    #[test]
    fn no_match_returns_none() {
        assert!(extract_trycloudflare_url("INF booting quick tunnel").is_none());
    }

    /// Supply-chain defense: the tarball extractor refuses `..` or
    /// absolute-path entries. `tar::Builder` blocks us from writing
    /// such entries in a unit test, so we exercise the underlying
    /// predicate directly — the extract function consults it on every
    /// entry, so covering the predicate covers the behaviour.
    #[test]
    fn path_is_safe_rejects_traversal_and_absolute() {
        use crate::binary::path_is_safe;
        use std::path::Path;
        assert!(path_is_safe(Path::new("cloudflared")));
        assert!(path_is_safe(Path::new("bin/cloudflared")));
        assert!(path_is_safe(Path::new("./cloudflared")));

        assert!(!path_is_safe(Path::new("../evil")));
        assert!(!path_is_safe(Path::new("foo/../../etc/passwd")));
        assert!(!path_is_safe(Path::new("/etc/passwd")));
    }

    // FOLLOWUPS PR-3 — sidecar URL accessor round-trip. Use a
    // throwaway NEXO_HOME so the test never touches the operator's
    // real `~/.nexo/state/tunnel.url`.
    #[test]
    fn sidecar_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Pin NEXO_HOME for the duration of this test.
        let prev = std::env::var_os("NEXO_HOME");
        std::env::set_var("NEXO_HOME", dir.path());

        // Sidecar starts empty.
        assert_eq!(read_url_file(), None);

        write_url_file("https://abc-123.trycloudflare.com").expect("write");
        assert_eq!(
            read_url_file().as_deref(),
            Some("https://abc-123.trycloudflare.com")
        );

        // Whitespace gets trimmed on read.
        write_url_file("\n  https://x.trycloudflare.com  \n").expect("write");
        assert_eq!(
            read_url_file().as_deref(),
            Some("https://x.trycloudflare.com")
        );

        clear_url_file().expect("clear");
        assert_eq!(read_url_file(), None);

        // Idempotent clear.
        clear_url_file().expect("clear-twice");

        match prev {
            Some(v) => std::env::set_var("NEXO_HOME", v),
            None => std::env::remove_var("NEXO_HOME"),
        }
    }
}
