//! Phase 82.2 — top-level HTTP server schema for the webhook
//! receiver. Wraps the per-source schema shipped in
//! `nexo-webhook-receiver` (Phase 80.12) with transport knobs and
//! per-source overrides for rate-limit + concurrency cap.
//!
//! Provider-agnostic by construction: the operator declares any
//! number of sources, each one of `signature.algorithm ∈
//! { hmac-sha256, hmac-sha1, raw-token }` plus an event-kind
//! origin (header or JSON body path). No provider-specific Rust
//! ships in core.

use std::net::SocketAddr;

use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::channels::ChannelRateLimit;

/// Default body cap honoured when neither global nor per-source
/// override is set. 1 MiB matches the Phase 80.12 default and is
/// generous for typical provider payloads.
pub const DEFAULT_BODY_CAP_BYTES: usize = 1024 * 1024;

/// Default request timeout — abort body buffering / handler work
/// after this many milliseconds. Mirror OpenClaw's 15 000 ms.
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 15_000;

/// Default per-source concurrency cap. 32 is the same number we
/// pick for MCP HTTP transport; protects DB / dispatcher from a
/// single noisy webhook source saturating the runtime.
pub const DEFAULT_CONCURRENCY_CAP: u32 = 32;

fn default_bind() -> SocketAddr {
    "0.0.0.0:8081".parse().expect("static literal")
}

fn default_body_cap() -> usize {
    DEFAULT_BODY_CAP_BYTES
}

fn default_request_timeout() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_MS
}

fn default_concurrency() -> u32 {
    DEFAULT_CONCURRENCY_CAP
}

/// Top-level webhook server config. Operator drops this in
/// `agents.yaml` (or wherever the runtime config lives) under
/// `webhook_receiver`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebhookServerConfig {
    /// Master killswitch. `false` (default) means the boot
    /// supervisor never spawns the listener — operators can ship
    /// the YAML disabled and flip via Phase 18 hot-reload.
    #[serde(default)]
    pub enabled: bool,

    /// Bind address. Default `0.0.0.0:8081` (one port above the
    /// existing health server). Operator typically fronts this
    /// with a reverse proxy; see `trusted_proxies` below.
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,

    /// Global default body cap. Per-source `body_cap_bytes` on
    /// `WebhookSourceConfig` overrides. `0` is invalid.
    #[serde(default = "default_body_cap")]
    pub body_cap_bytes: usize,

    /// Global request timeout in milliseconds. Applies to body
    /// buffering AND handler execution. `0` is invalid.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_ms: u64,

    /// Default rate limit applied per `(source_id, client_ip)` to
    /// every source that doesn't ship its own. `None` keeps the
    /// listener unthrottled (not recommended in production —
    /// document explicitly).
    #[serde(default)]
    pub default_rate_limit: Option<ChannelRateLimit>,

    /// Default per-source concurrency cap (max in-flight requests
    /// for any single source). `0` means unbounded — matches
    /// "no semaphore" semantics.
    #[serde(default = "default_concurrency")]
    pub default_concurrency_cap: u32,

    /// CIDR networks the operator considers trusted reverse
    /// proxies. When the inbound socket peer falls inside any of
    /// these, the handler honours `X-Forwarded-For` to extract the
    /// real client IP. Default empty — no header trust, peer is
    /// authoritative.
    #[serde(default)]
    pub trusted_proxies: Vec<IpNetwork>,

    /// When `true` AND `trusted_proxies` is non-empty AND no
    /// `X-Forwarded-For` chain matches, fall back to honouring
    /// `X-Real-IP`. Defensive default `false`.
    #[serde(default)]
    pub allow_realip_fallback: bool,

    /// Source declarations. May be empty even when `enabled` so an
    /// operator can stage the listener before declaring sources.
    #[serde(default)]
    pub sources: Vec<WebhookSourceWithLimits>,
}

impl Default for WebhookServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_bind(),
            body_cap_bytes: default_body_cap(),
            request_timeout_ms: default_request_timeout(),
            default_rate_limit: None,
            default_concurrency_cap: default_concurrency(),
            trusted_proxies: Vec::new(),
            allow_realip_fallback: false,
            sources: Vec::new(),
        }
    }
}

/// Per-source config. Wraps the Phase 80.12 `WebhookSourceConfig`
/// (already shipped in `nexo-webhook-receiver`) and layers two
/// override knobs on top.
///
/// Using `#[serde(flatten)]` keeps the operator-facing YAML shape
/// flat (one block per source), so existing fixtures don't need
/// nesting.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebhookSourceWithLimits {
    #[serde(flatten)]
    pub source: nexo_webhook_receiver::WebhookSourceConfig,

    /// Per-source rate limit override. Falls back to
    /// `WebhookServerConfig.default_rate_limit` when absent.
    #[serde(default)]
    pub rate_limit: Option<ChannelRateLimit>,

    /// Per-source concurrency cap override. `Some(0)` is rejected
    /// at validate time. `None` falls back to
    /// `WebhookServerConfig.default_concurrency_cap`.
    #[serde(default)]
    pub concurrency_cap: Option<u32>,
}

/// Boot-time validation errors. Caller maps to `tracing::error!` +
/// skip-server-spawn (daemon continues without webhook surface).
#[derive(Debug, Error, PartialEq)]
pub enum WebhookConfigError {
    #[error("body_cap_bytes must be > 0 (got 0)")]
    BodyCapZero,
    #[error("request_timeout_ms must be > 0 (got 0)")]
    RequestTimeoutZero,
    #[error("duplicate webhook source id `{0}`")]
    DuplicateId(String),
    #[error(
        "duplicate webhook source path `{path}` (sources `{first}` and `{second}` both bind it)"
    )]
    DuplicatePath {
        path: String,
        first: String,
        second: String,
    },
    #[error("source `{id}` invalid: {detail}")]
    Source { id: String, detail: String },
    #[error("default_rate_limit invalid: {0}")]
    DefaultRateLimit(String),
    #[error("source `{id}` rate_limit invalid: {detail}")]
    SourceRateLimit { id: String, detail: String },
    #[error("source `{id}` concurrency_cap must be > 0 when set (got 0)")]
    ConcurrencyCapZero { id: String },
    #[error(
        "bind address `{0}` collides with reserved port (health=8080, admin=9091); change bind"
    )]
    ReservedBind(SocketAddr),
}

impl WebhookServerConfig {
    /// Run all boot-time checks. Caller invokes from the boot
    /// supervisor; on `Err`, log + skip server spawn. Killswitch
    /// off short-circuits to `Ok(())` even when `sources` contain
    /// invalid entries — operator can edit the YAML, fix issues,
    /// then flip `enabled: true` via hot-reload.
    pub fn validate(&self) -> Result<(), WebhookConfigError> {
        if !self.enabled {
            return Ok(());
        }
        if self.body_cap_bytes == 0 {
            return Err(WebhookConfigError::BodyCapZero);
        }
        if self.request_timeout_ms == 0 {
            return Err(WebhookConfigError::RequestTimeoutZero);
        }
        if let Some(rl) = &self.default_rate_limit {
            rl.validate("webhook_receiver.default_rate_limit")
                .map_err(WebhookConfigError::DefaultRateLimit)?;
        }
        let port = self.bind.port();
        if port == 8080 || port == 9091 {
            return Err(WebhookConfigError::ReservedBind(self.bind));
        }
        let mut seen_ids: std::collections::BTreeMap<String, ()> =
            std::collections::BTreeMap::new();
        let mut seen_paths: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for s in &self.sources {
            nexo_webhook_receiver::WebhookHandler::validate(&s.source).map_err(|detail| {
                WebhookConfigError::Source {
                    id: s.source.id.clone(),
                    detail,
                }
            })?;
            if seen_ids.insert(s.source.id.clone(), ()).is_some() {
                return Err(WebhookConfigError::DuplicateId(s.source.id.clone()));
            }
            if let Some(prev_id) = seen_paths.get(&s.source.path) {
                return Err(WebhookConfigError::DuplicatePath {
                    path: s.source.path.clone(),
                    first: prev_id.clone(),
                    second: s.source.id.clone(),
                });
            }
            seen_paths.insert(s.source.path.clone(), s.source.id.clone());
            if let Some(rl) = &s.rate_limit {
                rl.validate(&format!("webhook_receiver.sources[{}].rate_limit", s.source.id))
                    .map_err(|detail| WebhookConfigError::SourceRateLimit {
                        id: s.source.id.clone(),
                        detail,
                    })?;
            }
            if let Some(cap) = s.concurrency_cap {
                if cap == 0 {
                    return Err(WebhookConfigError::ConcurrencyCapZero {
                        id: s.source.id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Resolve the rate limit applied to `source_id`. Per-source
    /// override wins; falls back to global default. `None` means
    /// no rate limiting for this source.
    pub fn resolve_rate_limit(&self, source_id: &str) -> Option<ChannelRateLimit> {
        for s in &self.sources {
            if s.source.id == source_id {
                if s.rate_limit.is_some() {
                    return s.rate_limit;
                }
                break;
            }
        }
        self.default_rate_limit
    }

    /// Resolve the concurrency cap applied to `source_id`. `0`
    /// signals "unbounded" — caller skips semaphore acquisition.
    pub fn resolve_concurrency_cap(&self, source_id: &str) -> u32 {
        for s in &self.sources {
            if s.source.id == source_id {
                if let Some(cap) = s.concurrency_cap {
                    return cap;
                }
                break;
            }
        }
        self.default_concurrency_cap
    }

    /// Resolve the body cap applied to `source_id`. Per-source
    /// override wins; falls back to global default.
    pub fn resolve_body_cap(&self, source_id: &str) -> usize {
        for s in &self.sources {
            if s.source.id == source_id {
                if let Some(cap) = s.source.body_cap_bytes {
                    return cap;
                }
                break;
            }
        }
        self.body_cap_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_webhook_receiver::{EventKindSource, SignatureAlgorithm, SignatureSpec};

    fn mk_source(id: &str, path: &str) -> WebhookSourceWithLimits {
        WebhookSourceWithLimits {
            source: nexo_webhook_receiver::WebhookSourceConfig {
                id: id.into(),
                path: path.into(),
                signature: SignatureSpec {
                    algorithm: SignatureAlgorithm::HmacSha256,
                    header: "X-Sig".into(),
                    prefix: "sha256=".into(),
                    secret_env: format!("WEBHOOK_{}_SECRET", id.to_uppercase()),
                },
                publish_to: format!("webhook.{id}.${{event_kind}}"),
                event_kind_from: EventKindSource::Header {
                    name: "X-Event".into(),
                },
                body_cap_bytes: None,
            },
            rate_limit: None,
            concurrency_cap: None,
        }
    }

    #[test]
    fn defaults_pass_validate_when_disabled() {
        let cfg = WebhookServerConfig::default();
        // disabled short-circuits — body_cap_bytes default is fine
        // here, but the killswitch path is the gate.
        assert_eq!(cfg.enabled, false);
        cfg.validate().expect("disabled validates");
    }

    #[test]
    fn enabled_with_zero_body_cap_rejects() {
        let cfg = WebhookServerConfig {
            enabled: true,
            body_cap_bytes: 0,
            ..Default::default()
        };
        assert_eq!(cfg.validate(), Err(WebhookConfigError::BodyCapZero));
    }

    #[test]
    fn enabled_with_zero_request_timeout_rejects() {
        let cfg = WebhookServerConfig {
            enabled: true,
            request_timeout_ms: 0,
            ..Default::default()
        };
        assert_eq!(cfg.validate(), Err(WebhookConfigError::RequestTimeoutZero));
    }

    #[test]
    fn duplicate_id_rejected() {
        let cfg = WebhookServerConfig {
            enabled: true,
            sources: vec![mk_source("github", "/a"), mk_source("github", "/b")],
            ..Default::default()
        };
        assert_eq!(
            cfg.validate(),
            Err(WebhookConfigError::DuplicateId("github".into()))
        );
    }

    #[test]
    fn duplicate_path_rejected() {
        let cfg = WebhookServerConfig {
            enabled: true,
            sources: vec![mk_source("a", "/x"), mk_source("b", "/x")],
            ..Default::default()
        };
        assert_eq!(
            cfg.validate(),
            Err(WebhookConfigError::DuplicatePath {
                path: "/x".into(),
                first: "a".into(),
                second: "b".into(),
            })
        );
    }

    #[test]
    fn reserved_health_port_rejected() {
        let cfg = WebhookServerConfig {
            enabled: true,
            bind: "0.0.0.0:8080".parse().unwrap(),
            ..Default::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(WebhookConfigError::ReservedBind(_))
        ));
    }

    #[test]
    fn reserved_admin_port_rejected() {
        let cfg = WebhookServerConfig {
            enabled: true,
            bind: "127.0.0.1:9091".parse().unwrap(),
            ..Default::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(WebhookConfigError::ReservedBind(_))
        ));
    }

    #[test]
    fn negative_default_rate_limit_rejected() {
        let cfg = WebhookServerConfig {
            enabled: true,
            default_rate_limit: Some(ChannelRateLimit {
                rps: -1.0,
                burst: 5,
            }),
            ..Default::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(WebhookConfigError::DefaultRateLimit(_))
        ));
    }

    #[test]
    fn per_source_zero_concurrency_rejected() {
        let mut s = mk_source("a", "/a");
        s.concurrency_cap = Some(0);
        let cfg = WebhookServerConfig {
            enabled: true,
            sources: vec![s],
            ..Default::default()
        };
        assert_eq!(
            cfg.validate(),
            Err(WebhookConfigError::ConcurrencyCapZero { id: "a".into() })
        );
    }

    #[test]
    fn per_source_invalid_inner_propagates() {
        let mut s = mk_source("a", "/a");
        s.source.id = String::new();
        let cfg = WebhookServerConfig {
            enabled: true,
            sources: vec![s],
            ..Default::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(WebhookConfigError::Source { .. })
        ));
    }

    #[test]
    fn happy_path_two_sources_validates() {
        let cfg = WebhookServerConfig {
            enabled: true,
            sources: vec![mk_source("a", "/a"), mk_source("b", "/b")],
            ..Default::default()
        };
        cfg.validate().expect("valid");
    }

    #[test]
    fn resolve_rate_limit_per_source_overrides_default() {
        let mut s = mk_source("a", "/a");
        s.rate_limit = Some(ChannelRateLimit {
            rps: 5.0,
            burst: 10,
        });
        let cfg = WebhookServerConfig {
            enabled: true,
            default_rate_limit: Some(ChannelRateLimit {
                rps: 1.0,
                burst: 2,
            }),
            sources: vec![s, mk_source("b", "/b")],
            ..Default::default()
        };
        let a = cfg.resolve_rate_limit("a").unwrap();
        assert_eq!(a.rps, 5.0);
        assert_eq!(a.burst, 10);
        let b = cfg.resolve_rate_limit("b").unwrap();
        assert_eq!(b.rps, 1.0);
        let unknown = cfg.resolve_rate_limit("unknown");
        assert_eq!(unknown.unwrap().rps, 1.0);
    }

    #[test]
    fn resolve_concurrency_cap_per_source_overrides_default() {
        let mut s = mk_source("a", "/a");
        s.concurrency_cap = Some(8);
        let cfg = WebhookServerConfig {
            enabled: true,
            default_concurrency_cap: 16,
            sources: vec![s, mk_source("b", "/b")],
            ..Default::default()
        };
        assert_eq!(cfg.resolve_concurrency_cap("a"), 8);
        assert_eq!(cfg.resolve_concurrency_cap("b"), 16);
        assert_eq!(cfg.resolve_concurrency_cap("unknown"), 16);
    }

    #[test]
    fn resolve_body_cap_per_source_overrides_default() {
        let mut s = mk_source("a", "/a");
        s.source.body_cap_bytes = Some(2048);
        let cfg = WebhookServerConfig {
            enabled: true,
            body_cap_bytes: 1024,
            sources: vec![s, mk_source("b", "/b")],
            ..Default::default()
        };
        assert_eq!(cfg.resolve_body_cap("a"), 2048);
        assert_eq!(cfg.resolve_body_cap("b"), 1024);
        assert_eq!(cfg.resolve_body_cap("unknown"), 1024);
    }

    #[test]
    fn yaml_round_trips() {
        let yaml = r#"
enabled: true
bind: "0.0.0.0:8081"
body_cap_bytes: 524288
request_timeout_ms: 10000
default_rate_limit:
  rps: 5.0
  burst: 10
default_concurrency_cap: 16
trusted_proxies:
  - "10.0.0.0/24"
allow_realip_fallback: true
sources:
  - id: "github_main"
    path: "/webhooks/github"
    signature:
      algorithm: "hmac-sha256"
      header: "X-Hub-Signature-256"
      prefix: "sha256="
      secret_env: "WEBHOOK_GITHUB_MAIN_SECRET"
    publish_to: "webhook.github_main.${event_kind}"
    event_kind_from:
      kind: "header"
      name: "X-GitHub-Event"
    rate_limit:
      rps: 20.0
      burst: 40
    concurrency_cap: 8
"#;
        let cfg: WebhookServerConfig = serde_yaml::from_str(yaml).expect("parse");
        cfg.validate().expect("valid");
        assert_eq!(cfg.sources.len(), 1);
        assert_eq!(cfg.sources[0].source.id, "github_main");
        assert_eq!(cfg.trusted_proxies.len(), 1);
        // Round-trip through serialize → deserialize.
        let yaml_back = serde_yaml::to_string(&cfg).unwrap();
        let cfg2: WebhookServerConfig = serde_yaml::from_str(&yaml_back).unwrap();
        assert_eq!(cfg, cfg2);
    }
}
