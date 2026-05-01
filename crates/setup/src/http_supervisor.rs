//! Phase 82.12 — HTTP server boot supervisor.
//!
//! Microapps that declare `[capabilities.http_server]` in
//! `plugin.toml` get a side-channel health check:
//! 1. Boot calls [`HttpServerSupervisor::probe`] after the
//!    extension's `initialize` handshake completes. The probe
//!    polls `GET <bind>:<port><health_path>` every 250 ms until
//!    it sees a `200 OK` or hits the [`READY_TIMEOUT`].
//! 2. Once the extension is `ready`, [`spawn_monitor_loop`]
//!    starts a 60 s background polling loop. Transient
//!    failures log at `warn`; the daemon does NOT restart the
//!    extension on its own.
//!
//! The supervisor is intentionally simple — boot wiring picks
//! the policy (fail-fast vs degrade) by checking
//! `probe().await.is_ok()`. Extensions that don't declare the
//! capability skip this whole subsystem.

use std::sync::Arc;
use std::time::{Duration, Instant};

use nexo_plugin_manifest::manifest::HttpServerCapability;
use reqwest::StatusCode;
use thiserror::Error;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Maximum wall time the boot probe waits for the microapp's
/// health endpoint to come up. Microapps with slower-than-30s
/// init either need to ship a stub server first or get marked
/// `unhealthy` and surface in `nexo extension status`.
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Interval between probe polls during the ready check.
pub const READY_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Interval between live-monitor polls after `ready`. Failures
/// surface as `warn` logs; runtime restart policy lives in the
/// extension supervisor, not here.
pub const MONITOR_INTERVAL: Duration = Duration::from_secs(60);

/// Per-request timeout. Health endpoints are expected to be
/// trivial; anything past this is a hung microapp.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Fail modes the boot probe surfaces. Wrapped so callers can
/// distinguish a hung microapp (timeout) from a misbehaving
/// one (non-200 response).
#[derive(Debug, Error)]
pub enum HealthProbeError {
    /// `READY_TIMEOUT` elapsed without seeing a 200.
    #[error("timed out waiting for {url} to return 200 within {0:?}", READY_TIMEOUT)]
    Timeout {
        /// Full URL the probe was hitting.
        url: String,
    },
    /// Server replied but with a non-200 status. Microapp will
    /// stay unhealthy until either it self-corrects or the
    /// operator restarts it.
    #[error("{url} returned {status} (expected 200)")]
    BadStatus {
        /// Full URL.
        url: String,
        /// Last observed HTTP status.
        status: u16,
    },
}

/// Build the URL the probe + monitor hit.
pub fn health_url(decl: &HttpServerCapability) -> String {
    let mut path = decl.health_path.clone();
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    format!("http://{}:{}{}", decl.bind, decl.port, path)
}

/// Boot-time supervisor surface. v0 is stateless — every
/// method takes the manifest declaration directly and returns
/// a fresh result. Tests inject a mock by swapping the
/// reqwest client through [`HttpServerSupervisor::with_client`].
#[derive(Clone)]
pub struct HttpServerSupervisor {
    client: reqwest::Client,
}

impl Default for HttpServerSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for HttpServerSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpServerSupervisor").finish_non_exhaustive()
    }
}

impl HttpServerSupervisor {
    /// Build a supervisor with a fresh reqwest client. Per-call
    /// timeout is set on the client so individual probes can't
    /// stall the whole loop.
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }

    /// Test/ops hook: provide a pre-built client.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// Block until the health endpoint returns `200 OK` or the
    /// `READY_TIMEOUT` elapses. Caller decides what to do with
    /// a `Timeout` (fail boot, mark unhealthy + continue, …).
    pub async fn probe(&self, decl: &HttpServerCapability) -> Result<(), HealthProbeError> {
        self.probe_with_deadline(decl, READY_TIMEOUT).await
    }

    /// Probe variant with a caller-supplied deadline. Tests
    /// pass a tight deadline; production goes through `probe`.
    pub async fn probe_with_deadline(
        &self,
        decl: &HttpServerCapability,
        deadline: Duration,
    ) -> Result<(), HealthProbeError> {
        let url = health_url(decl);
        let started = Instant::now();
        let mut last_status: Option<StatusCode> = None;
        loop {
            match self.client.get(&url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status == StatusCode::OK {
                        return Ok(());
                    }
                    last_status = Some(status);
                }
                Err(_) => {
                    // Transport error → microapp not up yet.
                    // Fall through to retry until the deadline.
                }
            }
            if started.elapsed() >= deadline {
                if let Some(status) = last_status {
                    return Err(HealthProbeError::BadStatus {
                        url,
                        status: status.as_u16(),
                    });
                }
                return Err(HealthProbeError::Timeout { url });
            }
            tokio::time::sleep(READY_POLL_INTERVAL).await;
        }
    }

    /// Spawn a 60 s background monitor loop. Returns
    /// `(JoinHandle, watch::Receiver<bool>)` — the receiver
    /// flips to `false` whenever a probe fails (so callers can
    /// surface "unhealthy" in `nexo extension status`) and
    /// back to `true` once the microapp recovers. Drop the
    /// handle to abort the loop; the receiver stays valid (its
    /// last value is the final health snapshot).
    pub fn spawn_monitor_loop(
        &self,
        decl: HttpServerCapability,
    ) -> (JoinHandle<()>, watch::Receiver<bool>) {
        let (tx, rx) = watch::channel(true);
        let client = self.client.clone();
        let handle = tokio::spawn(async move {
            let url = health_url(&decl);
            let mut interval = tokio::time::interval(MONITOR_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // First tick fires immediately; skip it so we don't
            // double-probe right after `probe()` returned ok.
            interval.tick().await;
            loop {
                interval.tick().await;
                let healthy = matches!(
                    client.get(&url).send().await.map(|r| r.status()),
                    Ok(StatusCode::OK)
                );
                if !healthy {
                    tracing::warn!(url=%url, "http_server health probe failed");
                }
                let _ = tx.send_replace(healthy);
            }
        });
        (handle, rx)
    }
}

/// Convenience: check that `bind` is loopback OR the operator
/// flipped `allow_external_bind = true`. Returns the offending
/// bind string when the policy is violated.
pub fn validate_bind_policy(
    decl: &HttpServerCapability,
    allow_external: bool,
) -> Result<(), String> {
    if !decl.is_loopback() && !allow_external {
        return Err(decl.bind.clone());
    }
    Ok(())
}

/// Convenience type alias used by boot wiring.
pub type SharedHttpSupervisor = Arc<HttpServerSupervisor>;

/// Phase 82.12 — sha256-hex digest of a bearer token,
/// truncated to the first 16 chars. Used in
/// `nexo/notify/token_rotated.old_hash` so microapps verify
/// the rotation message matches the token they hold without
/// the daemon putting the cleartext old token on the wire.
///
/// 16 hex chars = 8 bytes = 64-bit collision space — easily
/// strong enough to discriminate operator rotations (an
/// attacker forging a notification needs the daemon's stdio
/// channel anyway, which is more direct than birthday attacks
/// on a truncated hash).
pub fn token_hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(token.as_bytes());
    let mut s = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn cap(port: u16, path: &str) -> HttpServerCapability {
        HttpServerCapability {
            port,
            bind: "127.0.0.1".into(),
            token_env: "TEST_TOKEN".into(),
            health_path: path.into(),
        }
    }

    /// Spin a tiny axum-free hyper server that returns the
    /// requested status code on the configured path. Returns
    /// the bound port + a JoinHandle so the test can shut down.
    async fn spawn_health_server(
        path: &'static str,
        status: u16,
    ) -> (u16, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let request = String::from_utf8_lossy(&buf);
                    let body = "ok";
                    let response_status = if request.contains(path) {
                        status
                    } else {
                        404
                    };
                    let phrase = match response_status {
                        200 => "OK",
                        503 => "Service Unavailable",
                        _ => "Not Found",
                    };
                    let response = format!(
                        "HTTP/1.1 {response_status} {phrase}\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body,
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        (port, handle)
    }

    #[test]
    fn health_url_normalises_missing_slash() {
        let mut c = cap(1234, "healthz");
        assert_eq!(health_url(&c), "http://127.0.0.1:1234/healthz");
        c.health_path = "/custom".into();
        assert_eq!(health_url(&c), "http://127.0.0.1:1234/custom");
    }

    #[tokio::test]
    async fn probe_succeeds_on_200() {
        let (port, _h) = spawn_health_server("/healthz", 200).await;
        let sup = HttpServerSupervisor::new();
        let result = sup
            .probe_with_deadline(&cap(port, "/healthz"), Duration::from_secs(2))
            .await;
        assert!(result.is_ok(), "probe ok: {result:?}");
    }

    #[tokio::test]
    async fn probe_returns_bad_status_when_server_responds_503() {
        let (port, _h) = spawn_health_server("/healthz", 503).await;
        let sup = HttpServerSupervisor::new();
        let err = sup
            .probe_with_deadline(&cap(port, "/healthz"), Duration::from_millis(700))
            .await
            .unwrap_err();
        match err {
            HealthProbeError::BadStatus { status, .. } => assert_eq!(status, 503),
            other => panic!("expected BadStatus, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_times_out_when_no_server_listens() {
        // Bind a port and immediately drop it — no listener.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let sup = HttpServerSupervisor::new();
        let err = sup
            .probe_with_deadline(&cap(port, "/healthz"), Duration::from_millis(500))
            .await
            .unwrap_err();
        assert!(matches!(err, HealthProbeError::Timeout { .. }));
    }

    #[test]
    fn validate_bind_policy_loopback_always_ok() {
        let c = cap(8080, "/h");
        assert!(validate_bind_policy(&c, false).is_ok());
        assert!(validate_bind_policy(&c, true).is_ok());
    }

    #[test]
    fn validate_bind_policy_external_requires_opt_in() {
        let mut c = cap(8080, "/h");
        c.bind = "0.0.0.0".into();
        let err = validate_bind_policy(&c, false).unwrap_err();
        assert_eq!(err, "0.0.0.0");
        assert!(validate_bind_policy(&c, true).is_ok());
    }

    #[test]
    fn token_hash_is_stable_and_truncated() {
        let h = token_hash("hunter2");
        assert_eq!(h.len(), 16, "16 hex chars = 8 bytes");
        // Stable across calls.
        assert_eq!(h, token_hash("hunter2"));
        // Different inputs → different hashes.
        assert_ne!(token_hash("hunter2"), token_hash("hunter3"));
    }
}
