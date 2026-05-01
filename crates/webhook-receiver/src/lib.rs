//! Phase 80.12 — generic webhook receiver primitives.
//!
//! Provider-agnostic: operator configures sources in YAML with HTTP
//! path + signature spec + event-kind extraction + NATS publish
//! template. The crate ships pure-fn primitives (verify signature,
//! extract event kind, render publish topic) plus a [`WebhookHandler`]
//! that orchestrates the three.
//!
//! # Provider-agnostic
//!
//! No GitHub-specific (or any other provider-specific) code. The
//! decision table is data-driven via [`WebhookSourceConfig`]: any
//! provider that signs payloads with HMAC-SHA256 / HMAC-SHA1 / a
//! raw shared token AND exposes the event kind in a header or JSON
//! body field is supported. New providers add a YAML entry, no
//! Rust code change.
//!
//! [`WebhookEnvelope`] — the JSON envelope downstream NATS
//! consumers parse — lives in `nexo-tool-meta` so a third-party
//! microapp can `cargo add nexo-tool-meta` and consume the
//! envelope without depending on this crate. Re-exported here for
//! backward compat.

#![deny(missing_docs)]

pub mod client_ip;
pub mod dispatcher;

pub use client_ip::{extract_x_forwarded_for_chain, resolve_request_client_ip, ProxyHeaders};
pub use dispatcher::{
    envelope_from_handled, filter_forward_headers, format_webhook_source, DispatchError,
    RecordingWebhookDispatcher, WebhookDispatcher, WebhookEnvelope, ENVELOPE_SCHEMA_VERSION,
    FORWARD_HEADERS,
};

use std::collections::HashMap;

use bytes::Bytes;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;

/// Default body cap when `WebhookSourceConfig::body_cap_bytes` is
/// not set. 1 MB is generous for typical provider payloads while
/// guarding against accidental DoS via oversize bodies.
pub const DEFAULT_BODY_CAP_BYTES: usize = 1024 * 1024;

/// Per-source webhook configuration, loaded from operator YAML.
/// Validation runs at boot; invalid sources fail-fast so a typo
/// doesn't surface only when the first event arrives.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WebhookSourceConfig {
    /// Stable identifier — used for log correlation, capability
    /// gate naming (`WEBHOOK_<SOURCE_ID>_SECRET`), and as part of
    /// the URL when the operator wires the listener.
    pub id: String,

    /// HTTP path on which this source listens (e.g. `/webhooks/github`).
    /// Operator-controlled; the crate doesn't enforce a prefix.
    pub path: String,

    /// Signature verification spec.
    pub signature: SignatureSpec,

    /// NATS subject template — `${event_kind}` substitution
    /// supported. Example: `"webhook.github.${event_kind}"`.
    pub publish_to: String,

    /// Where to find the event kind in the request.
    pub event_kind_from: EventKindSource,

    /// Body cap. `None` defaults to `DEFAULT_BODY_CAP_BYTES`.
    /// Provided as `usize` so YAML accepts plain integer.
    #[serde(default)]
    pub body_cap_bytes: Option<usize>,
}

/// Signature verification spec — declares how the source signs
/// requests so the handler can compute the expected signature
/// and constant-time compare.
///
/// Caller-populated config (loaded from operator YAML);
/// intentionally **not** `#[non_exhaustive]`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SignatureSpec {
    /// Algorithm: `hmac-sha256`, `hmac-sha1`, or `raw-token`.
    pub algorithm: SignatureAlgorithm,

    /// HTTP header name carrying the signature (case-insensitive).
    /// Example: `"X-Hub-Signature-256"`, `"Stripe-Signature"`.
    pub header: String,

    /// Optional prefix the provider prepends (e.g. `"sha256="`).
    /// Stripped before constant-time compare. Empty string =
    /// no prefix.
    #[serde(default)]
    pub prefix: String,

    /// Environment variable carrying the shared secret. Read at
    /// runtime via `std::env::var` — rotates with daemon restart.
    pub secret_env: String,
}

/// Cryptographic algorithm a source uses to sign requests.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SignatureAlgorithm {
    /// HMAC-SHA256 — the de-facto standard (GitHub, Stripe, Slack).
    HmacSha256,
    /// HMAC-SHA1 — legacy providers; weaker collision resistance.
    HmacSha1,
    /// Raw shared token: `header == secret_env_value`. Constant-time
    /// compare. Some providers (older or simpler) use this rather
    /// than a real signature.
    RawToken,
}

/// Where in the inbound request the handler finds the event kind.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EventKindSource {
    /// Read the event kind from a header (e.g. `X-GitHub-Event`).
    Header {
        /// Header name to read (case-insensitive).
        name: String,
    },
    /// Read the event kind from a JSON body field via dotted path
    /// (e.g. `type` for top-level, `data.type` for nested).
    Body {
        /// Dotted JSON path of the field to read.
        path: String,
    },
}

/// Reasons a webhook request can be rejected. Caller maps to HTTP
/// status: 401 for signature errors, 413 for oversized body, 422
/// for missing event kind, 500 for secret-missing (operator
/// misconfig).
///
/// Operator-facing diagnostic — `#[non_exhaustive]` so future
/// reject reasons (e.g. payload schema mismatch) can land as
/// semver-minor without breaking downstream pattern matches.
#[non_exhaustive]
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RejectReason {
    /// Body length exceeds the configured cap. Caller maps to 413.
    #[error("body exceeds cap of {cap} bytes (got {got})")]
    OversizedBody {
        /// Actual body length received.
        got: usize,
        /// Configured cap.
        cap: usize,
    },
    /// The configured signature header is missing from the request.
    /// Caller maps to 401.
    #[error("signature header `{header}` is missing")]
    MissingSignatureHeader {
        /// Header name that was expected.
        header: String,
    },
    /// HMAC compare failed; the request was not authentic. Caller
    /// maps to 401.
    #[error("signature does not match (algorithm: {algorithm:?})")]
    InvalidSignature {
        /// Algorithm that was used (echoed in the error).
        algorithm: SignatureAlgorithm,
    },
    /// The configured `secret_env` env var is unset on the
    /// daemon's process. Caller maps to 500 (operator misconfig).
    #[error("secret env var `{var}` is unset")]
    SecretMissing {
        /// Env var name that was expected.
        var: String,
    },
    /// The configured event-kind extractor returned no value.
    /// Caller maps to 422.
    #[error("could not extract event kind from {origin}")]
    MissingEventKind {
        /// Human-readable origin of the extractor (header or
        /// JSON-path) for log correlation.
        origin: String,
    },
    /// The body claimed to be JSON but failed to parse. Caller
    /// maps to 422.
    #[error("body is not valid JSON (required for body-path event-kind extraction): {detail}")]
    InvalidBodyJson {
        /// Underlying serde error message.
        detail: String,
    },
    /// Event kind contains characters illegal as a NATS subject
    /// segment (`.`, `*`, `>`, whitespace). Caller maps to 422.
    #[error("event kind `{kind}` contains characters illegal for NATS subjects (`.`, `*`, `>`, whitespace)")]
    InvalidEventKindForSubject {
        /// The rejected event kind value.
        kind: String,
    },
}

/// The successful output of [`WebhookHandler::handle`]. Caller
/// publishes via `broker.publish(topic, Event::new(topic,
/// source_id, payload))`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandledEvent {
    /// Operator-assigned source identifier (matches
    /// `WebhookSourceConfig.id`).
    pub source_id: String,
    /// Event kind extracted from the inbound request.
    pub event_kind: String,
    /// Rendered NATS subject.
    pub topic: String,
    /// Inbound body, parsed as JSON. Non-JSON bodies are wrapped
    /// upstream as `{ "raw_base64": "..." }`.
    pub payload: serde_json::Value,
}

/// Verify HMAC-SHA256 / HMAC-SHA1 / raw-token signature in
/// constant time. Returns `Ok(())` on match, structured error on
/// mismatch.
pub fn verify_signature(
    spec: &SignatureSpec,
    secret: &str,
    header_value: &str,
    body: &[u8],
) -> Result<(), RejectReason> {
    let stripped = header_value
        .strip_prefix(&spec.prefix)
        .unwrap_or(header_value);

    match spec.algorithm {
        SignatureAlgorithm::HmacSha256 => {
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
                .map_err(|_| RejectReason::InvalidSignature {
                    algorithm: spec.algorithm,
                })?;
            mac.update(body);
            let want = mac.finalize().into_bytes();
            let got = hex::decode(stripped).map_err(|_| RejectReason::InvalidSignature {
                algorithm: spec.algorithm,
            })?;
            if got.len() != want.len() {
                return Err(RejectReason::InvalidSignature {
                    algorithm: spec.algorithm,
                });
            }
            if got.ct_eq(&want).into() {
                Ok(())
            } else {
                Err(RejectReason::InvalidSignature {
                    algorithm: spec.algorithm,
                })
            }
        }
        SignatureAlgorithm::HmacSha1 => {
            let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes())
                .map_err(|_| RejectReason::InvalidSignature {
                    algorithm: spec.algorithm,
                })?;
            mac.update(body);
            let want = mac.finalize().into_bytes();
            let got = hex::decode(stripped).map_err(|_| RejectReason::InvalidSignature {
                algorithm: spec.algorithm,
            })?;
            if got.len() != want.len() {
                return Err(RejectReason::InvalidSignature {
                    algorithm: spec.algorithm,
                });
            }
            if got.ct_eq(&want).into() {
                Ok(())
            } else {
                Err(RejectReason::InvalidSignature {
                    algorithm: spec.algorithm,
                })
            }
        }
        SignatureAlgorithm::RawToken => {
            // Constant-time compare on the bytes.
            if stripped.as_bytes().ct_eq(secret.as_bytes()).into() {
                Ok(())
            } else {
                Err(RejectReason::InvalidSignature {
                    algorithm: spec.algorithm,
                })
            }
        }
    }
}

/// Extract the event kind from headers or JSON body per the source
/// config. Returns `None` when the field is missing — caller
/// converts to `RejectReason::MissingEventKind`.
pub fn extract_event_kind(
    source: &EventKindSource,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> Result<Option<String>, RejectReason> {
    match source {
        EventKindSource::Header { name } => {
            // Headers are case-insensitive — the lookup compares
            // ASCII-lower-cased keys.
            let lower = name.to_ascii_lowercase();
            for (k, v) in headers {
                if k.to_ascii_lowercase() == lower {
                    return Ok(Some(v.clone()));
                }
            }
            Ok(None)
        }
        EventKindSource::Body { path } => {
            let json: serde_json::Value =
                serde_json::from_slice(body).map_err(|e| RejectReason::InvalidBodyJson {
                    detail: e.to_string(),
                })?;
            Ok(json_get_dotted(&json, path).and_then(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                _ => None,
            }))
        }
    }
}

/// Walk a dotted JSON path: `"data.type"` resolves to
/// `json["data"]["type"]`. Returns `None` on any miss along the way
/// (no panics, no unwraps).
fn json_get_dotted<'a>(json: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = json;
    for segment in path.split('.') {
        cur = cur.get(segment)?;
    }
    Some(cur)
}

/// Render the publish-to NATS subject by substituting
/// `${event_kind}` in the template. Other placeholders are left as
/// literals (forward-compatible with future template variables).
pub fn render_publish_topic(template: &str, event_kind: &str) -> String {
    template.replace("${event_kind}", event_kind)
}

/// `true` when an event-kind value is safe to use as a NATS subject
/// segment. NATS subjects use `.` as separator and `*` / `>` as
/// wildcards; whitespace is also disallowed in canonical NATS
/// usage.
fn is_event_kind_subject_safe(kind: &str) -> bool {
    !kind.is_empty()
        && !kind
            .chars()
            .any(|c| c == '.' || c == '*' || c == '>' || c.is_whitespace())
}

/// Top-level orchestrator. Operator constructs one per source
/// (typically inside a per-source HashMap keyed by `path`) and
/// invokes `handle` from the HTTP listener.
pub struct WebhookHandler {
    config: WebhookSourceConfig,
}

impl WebhookHandler {
    /// Construct from an already-validated config.
    pub fn new(config: WebhookSourceConfig) -> Self {
        Self { config }
    }

    /// Borrow the source config the handler was built from.
    pub fn config(&self) -> &WebhookSourceConfig {
        &self.config
    }

    /// Validate at boot. Currently checks: id non-empty, path non-empty,
    /// path starts with `/`, publish_to non-empty, body_cap_bytes > 0
    /// when set, signature.header non-empty, signature.secret_env
    /// non-empty.
    pub fn validate(config: &WebhookSourceConfig) -> Result<(), String> {
        if config.id.trim().is_empty() {
            return Err("webhook source id must be non-empty".into());
        }
        if config.path.trim().is_empty() {
            return Err(format!(
                "webhook source `{}` path must be non-empty",
                config.id
            ));
        }
        if !config.path.starts_with('/') {
            return Err(format!(
                "webhook source `{}` path must start with `/` (got `{}`)",
                config.id, config.path
            ));
        }
        if config.publish_to.trim().is_empty() {
            return Err(format!(
                "webhook source `{}` publish_to must be non-empty",
                config.id
            ));
        }
        if config.signature.header.trim().is_empty() {
            return Err(format!(
                "webhook source `{}` signature.header must be non-empty",
                config.id
            ));
        }
        if config.signature.secret_env.trim().is_empty() {
            return Err(format!(
                "webhook source `{}` signature.secret_env must be non-empty",
                config.id
            ));
        }
        if let Some(cap) = config.body_cap_bytes {
            if cap == 0 {
                return Err(format!(
                    "webhook source `{}` body_cap_bytes must be > 0",
                    config.id
                ));
            }
        }
        match &config.event_kind_from {
            EventKindSource::Header { name } if name.trim().is_empty() => {
                return Err(format!(
                    "webhook source `{}` event_kind_from.header.name must be non-empty",
                    config.id
                ));
            }
            EventKindSource::Body { path } if path.trim().is_empty() => {
                return Err(format!(
                    "webhook source `{}` event_kind_from.body.path must be non-empty",
                    config.id
                ));
            }
            _ => {}
        }
        Ok(())
    }

    /// Process a webhook request: enforce body cap, verify
    /// signature, extract event kind, render the publish topic,
    /// build the JSON payload. Returns the data the operator
    /// publishes via the broker.
    pub fn handle(
        &self,
        headers: &HashMap<String, String>,
        body: Bytes,
    ) -> Result<HandledEvent, RejectReason> {
        // 1. Body cap.
        let cap = self.config.body_cap_bytes.unwrap_or(DEFAULT_BODY_CAP_BYTES);
        if body.len() > cap {
            return Err(RejectReason::OversizedBody {
                got: body.len(),
                cap,
            });
        }

        // 2. Signature.
        let header_value = lookup_header_ci(headers, &self.config.signature.header)
            .ok_or_else(|| RejectReason::MissingSignatureHeader {
                header: self.config.signature.header.clone(),
            })?;
        let secret =
            std::env::var(&self.config.signature.secret_env).map_err(|_| {
                RejectReason::SecretMissing {
                    var: self.config.signature.secret_env.clone(),
                }
            })?;
        verify_signature(&self.config.signature, &secret, header_value, &body)?;

        // 3. Event kind.
        let event_kind = extract_event_kind(&self.config.event_kind_from, headers, &body)?
            .ok_or_else(|| RejectReason::MissingEventKind {
                origin: format!("{:?}", self.config.event_kind_from),
            })?;
        if !is_event_kind_subject_safe(&event_kind) {
            return Err(RejectReason::InvalidEventKindForSubject { kind: event_kind });
        }

        // 4. Topic + payload.
        let topic = render_publish_topic(&self.config.publish_to, &event_kind);
        // Body always serialises to a JSON value when present. Non-
        // JSON bodies (rare for webhook providers) get wrapped as a
        // `{ "raw_base64": "..." }` for transparency.
        let payload: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => {
                serde_json::json!({
                    "raw_base64": base64_encode(&body),
                })
            }
        };

        Ok(HandledEvent {
            source_id: self.config.id.clone(),
            event_kind,
            topic,
            payload,
        })
    }
}

fn lookup_header_ci<'a>(
    headers: &'a HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    let lower = name.to_ascii_lowercase();
    for (k, v) in headers {
        if k.to_ascii_lowercase() == lower {
            return Some(v.as_str());
        }
    }
    None
}

/// Minimal base64 encoder for the non-JSON-body fallback. Avoids
/// pulling the full `base64` crate at this dep level when the
/// `hex` crate already covers our hex needs.
fn base64_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(((bytes.len() + 2) / 3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let v = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(CHARS[((v >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((v >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() >= 2 {
            CHARS[((v >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() >= 3 {
            CHARS[(v & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_hmac_sha256() -> WebhookSourceConfig {
        WebhookSourceConfig {
            id: "github".into(),
            path: "/webhooks/github".into(),
            signature: SignatureSpec {
                algorithm: SignatureAlgorithm::HmacSha256,
                header: "X-Hub-Signature-256".into(),
                prefix: "sha256=".into(),
                secret_env: "TEST_GITHUB_SECRET".into(),
            },
            publish_to: "webhook.github.${event_kind}".into(),
            event_kind_from: EventKindSource::Header {
                name: "X-GitHub-Event".into(),
            },
            body_cap_bytes: None,
        }
    }

    fn hmac_sha256_hex(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    fn hmac_sha1_hex(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    // Sanity check the test crypto helpers match well-known shape.
    #[test]
    fn hmac_sha256_helper_matches_expected_length() {
        let mac = hmac_sha256_hex("k", b"x");
        assert_eq!(mac.len(), 64);
    }

    #[test]
    fn hmac_sha1_helper_matches_expected_length() {
        let mac = hmac_sha1_hex("k", b"x");
        assert_eq!(mac.len(), 40);
    }

    #[test]
    fn validate_accepts_well_formed_config() {
        WebhookHandler::validate(&cfg_hmac_sha256()).unwrap();
    }

    #[test]
    fn validate_rejects_empty_id() {
        let mut c = cfg_hmac_sha256();
        c.id = "".into();
        assert!(WebhookHandler::validate(&c).is_err());
    }

    #[test]
    fn validate_rejects_path_without_leading_slash() {
        let mut c = cfg_hmac_sha256();
        c.path = "webhooks/github".into();
        let err = WebhookHandler::validate(&c).unwrap_err();
        assert!(err.contains("must start with `/`"));
    }

    #[test]
    fn validate_rejects_zero_body_cap() {
        let mut c = cfg_hmac_sha256();
        c.body_cap_bytes = Some(0);
        assert!(WebhookHandler::validate(&c).is_err());
    }

    #[test]
    fn validate_rejects_empty_event_kind_header_name() {
        let mut c = cfg_hmac_sha256();
        c.event_kind_from = EventKindSource::Header { name: "".into() };
        assert!(WebhookHandler::validate(&c).is_err());
    }

    #[test]
    fn verify_signature_hmac_sha256_match() {
        let body = b"{\"hello\":\"world\"}";
        let secret = "topsecret";
        let sig = format!("sha256={}", hmac_sha256_hex(secret, body));
        let spec = SignatureSpec {
            algorithm: SignatureAlgorithm::HmacSha256,
            header: "X-Hub-Signature-256".into(),
            prefix: "sha256=".into(),
            secret_env: "_".into(),
        };
        verify_signature(&spec, secret, &sig, body).unwrap();
    }

    #[test]
    fn verify_signature_hmac_sha256_mismatch() {
        let body = b"{\"hello\":\"world\"}";
        let secret = "topsecret";
        let bogus = format!("sha256={}", hmac_sha256_hex("OTHER_SECRET", body));
        let spec = SignatureSpec {
            algorithm: SignatureAlgorithm::HmacSha256,
            header: "X-Hub-Signature-256".into(),
            prefix: "sha256=".into(),
            secret_env: "_".into(),
        };
        let err = verify_signature(&spec, secret, &bogus, body).unwrap_err();
        assert!(matches!(err, RejectReason::InvalidSignature { .. }));
    }

    #[test]
    fn verify_signature_hmac_sha1_match() {
        let body = b"x=1";
        let secret = "weakly-shared";
        let sig = hmac_sha1_hex(secret, body); // no prefix
        let spec = SignatureSpec {
            algorithm: SignatureAlgorithm::HmacSha1,
            header: "X-Sig".into(),
            prefix: "".into(),
            secret_env: "_".into(),
        };
        verify_signature(&spec, secret, &sig, body).unwrap();
    }

    #[test]
    fn verify_signature_raw_token_match() {
        let spec = SignatureSpec {
            algorithm: SignatureAlgorithm::RawToken,
            header: "X-Token".into(),
            prefix: "".into(),
            secret_env: "_".into(),
        };
        verify_signature(&spec, "exact-shared-token", "exact-shared-token", b"any")
            .unwrap();
    }

    #[test]
    fn verify_signature_raw_token_mismatch() {
        let spec = SignatureSpec {
            algorithm: SignatureAlgorithm::RawToken,
            header: "X-Token".into(),
            prefix: "".into(),
            secret_env: "_".into(),
        };
        let err = verify_signature(&spec, "want", "got", b"").unwrap_err();
        assert!(matches!(err, RejectReason::InvalidSignature { .. }));
    }

    #[test]
    fn verify_signature_garbage_hex_rejects() {
        let body = b"x";
        let spec = SignatureSpec {
            algorithm: SignatureAlgorithm::HmacSha256,
            header: "X-Sig".into(),
            prefix: "".into(),
            secret_env: "_".into(),
        };
        let err = verify_signature(&spec, "secret", "not-hex", body).unwrap_err();
        assert!(matches!(err, RejectReason::InvalidSignature { .. }));
    }

    #[test]
    fn extract_event_kind_from_header_case_insensitive() {
        let mut h = HashMap::new();
        h.insert("x-github-event".into(), "pull_request".into());
        let src = EventKindSource::Header {
            name: "X-GitHub-Event".into(),
        };
        let kind = extract_event_kind(&src, &h, b"").unwrap();
        assert_eq!(kind, Some("pull_request".to_string()));
    }

    #[test]
    fn extract_event_kind_from_header_missing_returns_none() {
        let h = HashMap::new();
        let src = EventKindSource::Header {
            name: "X-Missing".into(),
        };
        assert_eq!(extract_event_kind(&src, &h, b"").unwrap(), None);
    }

    #[test]
    fn extract_event_kind_from_body_top_level() {
        let body = br#"{"type":"checkout.completed","data":{}}"#;
        let src = EventKindSource::Body {
            path: "type".into(),
        };
        let kind = extract_event_kind(&src, &HashMap::new(), body).unwrap();
        assert_eq!(kind, Some("checkout.completed".to_string()));
        // ^ note: contains `.` so subject-safety check rejects later
        // — covered in is_event_kind_subject_safe test.
    }

    #[test]
    fn extract_event_kind_from_body_nested_path() {
        let body = br#"{"data":{"event":{"type":"created"}}}"#;
        let src = EventKindSource::Body {
            path: "data.event.type".into(),
        };
        let kind = extract_event_kind(&src, &HashMap::new(), body).unwrap();
        assert_eq!(kind, Some("created".to_string()));
    }

    #[test]
    fn extract_event_kind_from_body_missing_path_returns_none() {
        let body = br#"{"type":"x"}"#;
        let src = EventKindSource::Body {
            path: "data.type".into(),
        };
        assert_eq!(
            extract_event_kind(&src, &HashMap::new(), body).unwrap(),
            None
        );
    }

    #[test]
    fn extract_event_kind_from_body_invalid_json_errors() {
        let body = b"not json";
        let src = EventKindSource::Body {
            path: "type".into(),
        };
        let err = extract_event_kind(&src, &HashMap::new(), body).unwrap_err();
        assert!(matches!(err, RejectReason::InvalidBodyJson { .. }));
    }

    #[test]
    fn render_publish_topic_substitutes_event_kind() {
        let s = render_publish_topic("webhook.github.${event_kind}", "push");
        assert_eq!(s, "webhook.github.push");
    }

    #[test]
    fn render_publish_topic_no_var_passes_through() {
        let s = render_publish_topic("webhook.github.fixed", "push");
        assert_eq!(s, "webhook.github.fixed");
    }

    #[test]
    fn is_event_kind_subject_safe_rejects_dot() {
        assert!(!is_event_kind_subject_safe("checkout.completed"));
    }

    #[test]
    fn is_event_kind_subject_safe_rejects_wildcards_and_whitespace() {
        assert!(!is_event_kind_subject_safe("foo*bar"));
        assert!(!is_event_kind_subject_safe("foo>bar"));
        assert!(!is_event_kind_subject_safe("foo bar"));
        assert!(!is_event_kind_subject_safe(""));
    }

    #[test]
    fn is_event_kind_subject_safe_accepts_alphanumeric_dashes() {
        assert!(is_event_kind_subject_safe("pull_request"));
        assert!(is_event_kind_subject_safe("issue-comment"));
        assert!(is_event_kind_subject_safe("push"));
    }

    #[test]
    fn handle_oversized_body_rejects() {
        let mut c = cfg_hmac_sha256();
        c.body_cap_bytes = Some(10);
        let h = WebhookHandler::new(c);
        let body = Bytes::from_static(b"this body exceeds ten bytes");
        let headers = HashMap::new();
        let err = h.handle(&headers, body).unwrap_err();
        assert!(matches!(err, RejectReason::OversizedBody { .. }));
    }

    #[test]
    fn handle_missing_signature_header_rejects() {
        let h = WebhookHandler::new(cfg_hmac_sha256());
        let body = Bytes::from_static(b"{}");
        let headers = HashMap::new();
        let err = h.handle(&headers, body).unwrap_err();
        assert!(matches!(
            err,
            RejectReason::MissingSignatureHeader { .. }
        ));
    }

    #[test]
    fn handle_secret_unset_rejects() {
        // Use a unique env var that's reliably unset.
        let mut c = cfg_hmac_sha256();
        c.signature.secret_env = "TEST_DEFINITELY_UNSET_VAR_8014_ZZZ".into();
        let h = WebhookHandler::new(c);
        let body = Bytes::from_static(b"{}");
        let mut headers = HashMap::new();
        headers.insert(
            "x-hub-signature-256".into(),
            format!("sha256={}", hmac_sha256_hex("dummy", b"{}")),
        );
        std::env::remove_var("TEST_DEFINITELY_UNSET_VAR_8014_ZZZ");
        let err = h.handle(&headers, body).unwrap_err();
        assert!(matches!(err, RejectReason::SecretMissing { .. }));
    }

    #[test]
    fn handle_invalid_signature_rejects_with_correct_secret_set() {
        let mut c = cfg_hmac_sha256();
        c.signature.secret_env = "TEST_WHK_SECRET_INVALID_SIG".into();
        let h = WebhookHandler::new(c);
        std::env::set_var("TEST_WHK_SECRET_INVALID_SIG", "real");
        let body = Bytes::from_static(b"{}");
        let mut headers = HashMap::new();
        // Sign with WRONG secret.
        headers.insert(
            "x-hub-signature-256".into(),
            format!("sha256={}", hmac_sha256_hex("wrong", b"{}")),
        );
        let err = h.handle(&headers, body).unwrap_err();
        std::env::remove_var("TEST_WHK_SECRET_INVALID_SIG");
        assert!(matches!(err, RejectReason::InvalidSignature { .. }));
    }

    #[test]
    fn handle_happy_path_publishes_event() {
        let mut c = cfg_hmac_sha256();
        c.signature.secret_env = "TEST_WHK_SECRET_HAPPY".into();
        let h = WebhookHandler::new(c);
        std::env::set_var("TEST_WHK_SECRET_HAPPY", "supersecret");
        let body_bytes = b"{\"action\":\"opened\"}";
        let body = Bytes::from_static(body_bytes);
        let mut headers = HashMap::new();
        headers.insert(
            "x-hub-signature-256".into(),
            format!("sha256={}", hmac_sha256_hex("supersecret", body_bytes)),
        );
        headers.insert("x-github-event".into(), "pull_request".into());
        let evt = h.handle(&headers, body).unwrap();
        std::env::remove_var("TEST_WHK_SECRET_HAPPY");
        assert_eq!(evt.source_id, "github");
        assert_eq!(evt.event_kind, "pull_request");
        assert_eq!(evt.topic, "webhook.github.pull_request");
        assert_eq!(
            evt.payload,
            serde_json::json!({"action": "opened"})
        );
    }

    #[test]
    fn handle_event_kind_with_dot_rejects_subject_safety() {
        let mut c = cfg_hmac_sha256();
        c.signature.secret_env = "TEST_WHK_SECRET_DOT".into();
        c.event_kind_from = EventKindSource::Body {
            path: "type".into(),
        };
        let h = WebhookHandler::new(c);
        std::env::set_var("TEST_WHK_SECRET_DOT", "s");
        let body_bytes = b"{\"type\":\"checkout.completed\"}";
        let body = Bytes::from_static(body_bytes);
        let mut headers = HashMap::new();
        headers.insert(
            "x-hub-signature-256".into(),
            format!("sha256={}", hmac_sha256_hex("s", body_bytes)),
        );
        let err = h.handle(&headers, body).unwrap_err();
        std::env::remove_var("TEST_WHK_SECRET_DOT");
        assert!(matches!(
            err,
            RejectReason::InvalidEventKindForSubject { .. }
        ));
    }

    #[test]
    fn handle_non_json_body_wraps_as_raw_base64() {
        let mut c = cfg_hmac_sha256();
        c.signature.secret_env = "TEST_WHK_SECRET_RAW".into();
        c.event_kind_from = EventKindSource::Header {
            name: "X-Event-Kind".into(),
        };
        let h = WebhookHandler::new(c);
        std::env::set_var("TEST_WHK_SECRET_RAW", "s");
        let body_bytes: &[u8] = &[0x80, 0x81, 0x82]; // non-UTF-8
        let body = Bytes::copy_from_slice(body_bytes);
        let mut headers = HashMap::new();
        headers.insert(
            "x-hub-signature-256".into(),
            format!("sha256={}", hmac_sha256_hex("s", body_bytes)),
        );
        headers.insert("x-event-kind".into(), "binary_event".into());
        let evt = h.handle(&headers, body).unwrap();
        std::env::remove_var("TEST_WHK_SECRET_RAW");
        assert_eq!(evt.event_kind, "binary_event");
        assert!(evt.payload.get("raw_base64").is_some());
    }

    #[test]
    fn yaml_round_trip_full_config() {
        let yaml = r#"
id: github
path: /webhooks/github
signature:
  algorithm: hmac-sha256
  header: X-Hub-Signature-256
  prefix: "sha256="
  secret_env: GITHUB_WEBHOOK_SECRET
publish_to: webhook.github.${event_kind}
event_kind_from:
  kind: header
  name: X-GitHub-Event
"#;
        let parsed: WebhookSourceConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.id, "github");
        assert_eq!(parsed.signature.algorithm, SignatureAlgorithm::HmacSha256);
        WebhookHandler::validate(&parsed).unwrap();
    }

    #[test]
    fn yaml_round_trip_body_path_extraction() {
        let yaml = r#"
id: stripe-prod
path: /webhooks/stripe
signature:
  algorithm: hmac-sha256
  header: Stripe-Signature
  prefix: ""
  secret_env: STRIPE_WEBHOOK_SECRET
publish_to: webhook.stripe.${event_kind}
event_kind_from:
  kind: body
  path: data.event.type
"#;
        let parsed: WebhookSourceConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            parsed.event_kind_from,
            EventKindSource::Body { .. }
        ));
        WebhookHandler::validate(&parsed).unwrap();
    }
}
