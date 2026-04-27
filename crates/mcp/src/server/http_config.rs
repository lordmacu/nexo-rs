//! Phase 76.1 — HTTP transport configuration.
//!
//! Defaults are deliberately conservative; any insecure
//! combination is refused at boot via [`HttpTransportConfig::validate`].

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Hard upper bound on body size, regardless of operator config.
/// 16 MiB is enough for any reasonable JSON-RPC payload while
/// preventing trivial memory-pressure attacks.
pub const MAX_BODY_BYTES_HARD_CAP: usize = 16 * 1024 * 1024;

/// Hard upper bound on idle-timeout (24 h). Sessions older than
/// this are almost certainly leaked.
pub const MAX_IDLE_TIMEOUT_SECS: u64 = 86_400;

/// Hard upper bound on per-request handler timeout (10 min).
/// Beyond this is almost certainly a stuck handler.
pub const MAX_REQUEST_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpTransportConfig {
    /// Whether to start the HTTP transport at all. Stdio is
    /// independent and shipped from `McpServerConfig` already.
    #[serde(default)]
    pub enabled: bool,

    /// Bind address. Defaults to `127.0.0.1:7575`. Any non-loopback
    /// bind requires authentication AND a non-empty, non-`*`
    /// `allow_origins` allowlist or boot fails.
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,

    /// Phase 76.3 — preferred. Pluggable authenticator
    /// configuration (`none` / `static_token` / `bearer_jwt` /
    /// `mutual_tls`). Mutually exclusive with the legacy
    /// `auth_token` field below; setting both refuses to boot.
    #[serde(default)]
    pub auth: Option<crate::server::auth::AuthConfig>,

    /// DEPRECATED (Phase 76.3): legacy bearer-token field. Kept
    /// for backward compat; auto-promoted to
    /// `AuthConfig::StaticToken` at boot with a `warn` log.
    #[serde(default)]
    pub auth_token: Option<String>,

    /// CORS / origin allowlist for browser clients (mandatory when
    /// bind is not loopback). Exact-match comparison — substring
    /// match is intentionally NOT supported.
    #[serde(default = "default_allow_origins")]
    pub allow_origins: Vec<String>,

    /// Body size cap. Default 1 MiB; hard cap [`MAX_BODY_BYTES_HARD_CAP`].
    #[serde(default = "default_body_max_bytes")]
    pub body_max_bytes: usize,

    /// Global in-flight request cap (tower ConcurrencyLimit).
    #[serde(default = "default_max_in_flight")]
    pub max_in_flight: usize,

    /// Per-IP rate limit applied to mutating routes.
    #[serde(default)]
    pub per_ip_rate_limit: PerIpRateLimit,

    /// Per-request handler timeout. Default 30 s; hard cap
    /// [`MAX_REQUEST_TIMEOUT_SECS`].
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,

    /// Idle TTL for sessions. Default 300 s.
    #[serde(default = "default_session_idle_timeout_secs")]
    pub session_idle_timeout_secs: u64,

    /// Absolute upper bound on session age. Default 86400 s.
    #[serde(default = "default_session_max_lifetime_secs")]
    pub session_max_lifetime_secs: u64,

    /// Cap on concurrent sessions. Over-cap returns 503 with
    /// `Retry-After`.
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,

    /// SSE keep-alive interval.
    #[serde(default = "default_sse_keepalive_secs")]
    pub sse_keepalive_secs: u64,

    /// SSE max stream duration before server-initiated close.
    #[serde(default = "default_sse_max_age_secs")]
    pub sse_max_age_secs: u64,

    /// SSE per-session broadcast buffer. Drop-oldest on overflow.
    #[serde(default = "default_sse_buffer_size")]
    pub sse_buffer_size: usize,

    /// Enable legacy SSE alias (`GET /sse` + `POST /messages`).
    #[serde(default)]
    pub enable_legacy_sse: bool,

    /// Phase 76.5 — per-(tenant, tool) token-bucket rate limit.
    /// `None` disables enforcement entirely (zero overhead in the
    /// dispatcher hot path). When `Some`, the limiter sits inside
    /// `Dispatcher::dispatch` for `tools/call` only; `initialize`,
    /// `tools/list`, `shutdown`, etc. bypass.
    #[serde(default)]
    pub per_principal_rate_limit:
        Option<crate::server::per_principal_rate_limit::PerPrincipalRateLimiterConfig>,

    /// Phase 76.6 — per-(tenant, tool) in-flight concurrency cap +
    /// per-call timeout. `None` disables; otherwise enforced at
    /// the dispatcher for `tools/call` only.
    #[serde(default)]
    pub per_principal_concurrency:
        Option<crate::server::per_principal_concurrency::PerPrincipalConcurrencyConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PerIpRateLimit {
    #[serde(default = "default_rate_rps")]
    pub rps: u32,
    #[serde(default = "default_rate_burst")]
    pub burst: u32,
}

impl Default for PerIpRateLimit {
    fn default() -> Self {
        Self {
            rps: default_rate_rps(),
            burst: default_rate_burst(),
        }
    }
}

impl Default for HttpTransportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_bind(),
            auth: None,
            auth_token: None,
            allow_origins: default_allow_origins(),
            body_max_bytes: default_body_max_bytes(),
            max_in_flight: default_max_in_flight(),
            per_ip_rate_limit: PerIpRateLimit::default(),
            request_timeout_secs: default_request_timeout_secs(),
            session_idle_timeout_secs: default_session_idle_timeout_secs(),
            session_max_lifetime_secs: default_session_max_lifetime_secs(),
            max_sessions: default_max_sessions(),
            sse_keepalive_secs: default_sse_keepalive_secs(),
            sse_max_age_secs: default_sse_max_age_secs(),
            sse_buffer_size: default_sse_buffer_size(),
            enable_legacy_sse: false,
            per_principal_rate_limit: None,
            per_principal_concurrency: None,
        }
    }
}

impl HttpTransportConfig {
    /// Validate at boot. Returns the first refusal reason as a
    /// human-readable error.
    pub fn validate(&self) -> Result<(), String> {
        let loopback = is_loopback(&self.bind.ip());

        if !loopback && self.auth_token.is_none() {
            return Err(format!(
                "bind {} is not loopback; auth_token (or auth_token_env) is required",
                self.bind
            ));
        }

        if !loopback {
            if self.allow_origins.is_empty() {
                return Err(format!(
                    "bind {} is not loopback; allow_origins must not be empty",
                    self.bind
                ));
            }
            if self.allow_origins.iter().any(|o| o == "*") {
                return Err(format!(
                    "bind {} is not loopback; allow_origins '*' is forbidden",
                    self.bind
                ));
            }
        }

        if self.body_max_bytes == 0 {
            return Err("body_max_bytes must be > 0".into());
        }
        if self.body_max_bytes > MAX_BODY_BYTES_HARD_CAP {
            return Err(format!(
                "body_max_bytes {} exceeds hard cap {}",
                self.body_max_bytes, MAX_BODY_BYTES_HARD_CAP
            ));
        }
        if self.max_in_flight == 0 {
            return Err("max_in_flight must be > 0".into());
        }
        if self.max_sessions == 0 {
            return Err("max_sessions must be > 0".into());
        }
        if self.session_idle_timeout_secs == 0 {
            return Err("session_idle_timeout_secs must be > 0".into());
        }
        if self.session_idle_timeout_secs > MAX_IDLE_TIMEOUT_SECS {
            return Err(format!(
                "session_idle_timeout_secs {} exceeds hard cap {}",
                self.session_idle_timeout_secs, MAX_IDLE_TIMEOUT_SECS
            ));
        }
        if self.session_max_lifetime_secs < self.session_idle_timeout_secs {
            return Err("session_max_lifetime_secs must be >= session_idle_timeout_secs".into());
        }
        if self.request_timeout_secs == 0 {
            return Err("request_timeout_secs must be > 0".into());
        }
        if self.request_timeout_secs > MAX_REQUEST_TIMEOUT_SECS {
            return Err(format!(
                "request_timeout_secs {} exceeds hard cap {}",
                self.request_timeout_secs, MAX_REQUEST_TIMEOUT_SECS
            ));
        }
        if self.sse_max_age_secs == 0 {
            return Err("sse_max_age_secs must be > 0".into());
        }
        if self.sse_buffer_size == 0 {
            return Err("sse_buffer_size must be > 0".into());
        }
        if self.per_ip_rate_limit.rps == 0 {
            return Err("per_ip_rate_limit.rps must be > 0".into());
        }
        if self.per_ip_rate_limit.burst == 0 {
            return Err("per_ip_rate_limit.burst must be > 0".into());
        }
        Ok(())
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }

    pub fn sse_keepalive(&self) -> Duration {
        Duration::from_secs(self.sse_keepalive_secs)
    }

    pub fn sse_max_age(&self) -> Duration {
        Duration::from_secs(self.sse_max_age_secs)
    }

    pub fn session_idle_timeout(&self) -> Duration {
        Duration::from_secs(self.session_idle_timeout_secs)
    }

    pub fn session_max_lifetime(&self) -> Duration {
        Duration::from_secs(self.session_max_lifetime_secs)
    }
}

/// Loopback covers `127.0.0.0/8` (IPv4) and `::1` (IPv6). The
/// unspecified addresses (`0.0.0.0`, `::`) are NOT loopback and
/// require auth + allowlist.
pub(crate) fn is_loopback(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

fn default_bind() -> SocketAddr {
    "127.0.0.1:7575".parse().unwrap()
}
fn default_allow_origins() -> Vec<String> {
    vec!["http://localhost".into(), "http://127.0.0.1".into()]
}
fn default_body_max_bytes() -> usize {
    1024 * 1024
}
fn default_max_in_flight() -> usize {
    500
}
fn default_request_timeout_secs() -> u64 {
    30
}
fn default_session_idle_timeout_secs() -> u64 {
    300
}
fn default_session_max_lifetime_secs() -> u64 {
    86_400
}
fn default_max_sessions() -> usize {
    1_000
}
fn default_sse_keepalive_secs() -> u64 {
    15
}
fn default_sse_max_age_secs() -> u64 {
    600
}
fn default_sse_buffer_size() -> usize {
    256
}
fn default_rate_rps() -> u32 {
    60
}
fn default_rate_burst() -> u32 {
    120
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_loopback_validates_ok() {
        HttpTransportConfig::default().validate().unwrap();
    }

    #[test]
    fn default_with_token_validates_ok() {
        let mut cfg = HttpTransportConfig::default();
        cfg.auth_token = Some("t".into());
        cfg.validate().unwrap();
    }

    #[test]
    fn public_bind_without_token_refuses() {
        let mut cfg = HttpTransportConfig::default();
        cfg.bind = "0.0.0.0:7575".parse().unwrap();
        cfg.auth_token = None;
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("auth_token"));
    }

    #[test]
    fn public_bind_with_empty_allowlist_refuses() {
        let mut cfg = HttpTransportConfig::default();
        cfg.bind = "0.0.0.0:7575".parse().unwrap();
        cfg.auth_token = Some("t".into());
        cfg.allow_origins.clear();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("allow_origins"));
    }

    #[test]
    fn public_bind_with_star_allowlist_refuses() {
        let mut cfg = HttpTransportConfig::default();
        cfg.bind = "0.0.0.0:7575".parse().unwrap();
        cfg.auth_token = Some("t".into());
        cfg.allow_origins = vec!["*".into()];
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("'*'"));
    }

    #[test]
    fn body_above_hard_cap_refuses() {
        let mut cfg = HttpTransportConfig::default();
        cfg.body_max_bytes = MAX_BODY_BYTES_HARD_CAP + 1;
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("body_max_bytes"));
    }

    #[test]
    fn idle_above_hard_cap_refuses() {
        let mut cfg = HttpTransportConfig::default();
        cfg.session_idle_timeout_secs = MAX_IDLE_TIMEOUT_SECS + 1;
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("session_idle_timeout_secs"));
    }

    #[test]
    fn lifetime_below_idle_refuses() {
        let mut cfg = HttpTransportConfig::default();
        cfg.session_idle_timeout_secs = 1000;
        cfg.session_max_lifetime_secs = 500;
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("session_max_lifetime_secs"));
    }

    #[test]
    fn request_timeout_above_hard_cap_refuses() {
        let mut cfg = HttpTransportConfig::default();
        cfg.request_timeout_secs = MAX_REQUEST_TIMEOUT_SECS + 1;
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("request_timeout_secs"));
    }

    #[test]
    fn valid_public_deployment_validates_ok() {
        let mut cfg = HttpTransportConfig::default();
        cfg.bind = "0.0.0.0:7575".parse().unwrap();
        cfg.auth_token = Some("strong-token".into());
        cfg.allow_origins = vec!["https://app.example.com".into()];
        cfg.validate().unwrap();
    }

    #[test]
    fn parses_full_yaml() {
        let yaml = r#"
enabled: true
bind: "0.0.0.0:7575"
auth_token: secret
allow_origins:
  - "https://app.example.com"
body_max_bytes: 1048576
max_in_flight: 500
per_ip_rate_limit:
  rps: 60
  burst: 120
request_timeout_secs: 30
session_idle_timeout_secs: 300
session_max_lifetime_secs: 86400
max_sessions: 1000
sse_keepalive_secs: 15
sse_max_age_secs: 600
sse_buffer_size: 256
enable_legacy_sse: false
"#;
        let cfg: HttpTransportConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.enabled);
        cfg.validate().unwrap();
    }

    #[test]
    fn ipv6_loopback_is_loopback() {
        let mut cfg = HttpTransportConfig::default();
        cfg.bind = "[::1]:7575".parse().unwrap();
        cfg.auth_token = None;
        cfg.validate().unwrap();
    }
}
