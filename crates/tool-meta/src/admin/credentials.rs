//! Phase 82.10.d — `nexo/admin/credentials/*` wire types.
//!
//! Many-to-many semantic: 1 channel credential
//! (`(channel, instance)`) can serve N agents simultaneously.
//! The credential payload (token / API key / OAuth state) lives
//! in a per-credential filesystem entry; the agents that consume
//! it list the binding `{plugin: channel, instance}` in their
//! `inbound_bindings`. Operators rebind from either side
//! (`credentials/register` accepts `agent_ids`;
//! `agents/upsert` accepts `inbound_bindings`).
//!
//! Phase 82.10.n adds the `metadata` field on
//! [`CredentialRegisterInput`] + the [`CredentialRegisterResponse`]
//! / [`CredentialValidationOutcome`] pair so the dispatcher can
//! bridge to channel-specific plugin yaml + return a probe
//! outcome to the operator UI.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Filter for `nexo/admin/credentials/list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CredentialsListFilter {
    /// When set, only return credentials whose `channel` matches.
    /// `None` = all channels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_filter: Option<String>,
}

/// One row in the `credentials/list` response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CredentialSummary {
    /// Channel id (`whatsapp`, future `telegram` / `email`).
    pub channel: String,
    /// Instance / account discriminator (`personal`, `business`,
    /// …). `None` for single-instance channels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Agent ids that currently bind to this credential via their
    /// `agents.yaml.<id>.inbound_bindings` list. Empty when the
    /// credential exists on disk but no agent is bound (orphan
    /// credential — operator must rebind or revoke).
    pub agent_ids: Vec<String>,
}

/// Response for `nexo/admin/credentials/list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CredentialsListResponse {
    /// Matching credentials in stable order
    /// (alpha by `channel`, then `instance`).
    pub credentials: Vec<CredentialSummary>,
}

/// Params for `nexo/admin/credentials/register`. Many-to-many:
/// caller supplies the list of agent ids that should consume this
/// credential. Existing bindings on those agents stay; the new
/// binding is appended (idempotent — duplicate binding skipped).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CredentialRegisterInput {
    /// Channel id.
    pub channel: String,
    /// Instance discriminator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Agent ids to bind to this credential. Empty list means
    /// "store the credential on disk but don't bind it to any
    /// agent yet" (operator can bind later via `agents/upsert`).
    pub agent_ids: Vec<String>,
    /// Opaque payload written to the per-credential filesystem
    /// entry. Shape depends on channel: for whatsapp, e.g. the
    /// pairing token; for future telegram, the bot API key; for
    /// future email, the OAuth refresh token. Format is the
    /// channel plugin's responsibility — the admin RPC layer is
    /// channel-agnostic.
    pub payload: serde_json::Value,
    /// Phase 82.10.n — channel-specific structured hints consumed
    /// by the registered [`crate::admin::credentials`] persister
    /// (e.g. email = `{ imap: { host, port, tls }, smtp: {…},
    /// address }`; telegram = `{ polling: { enabled, interval_ms
    /// }, allow_agents }`; whatsapp = empty). The persister owns
    /// the schema and validates it. The admin RPC layer treats
    /// this map as opaque data passed through to the persister.
    /// Defaults to an empty map for back-compat with pre-82.10.n
    /// callers.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Phase 82.10.n — outcome of the channel persister's optional
/// connectivity probe (telegram `getMe`, email IMAP `LOGIN +
/// NOOP`). Returned inside [`CredentialRegisterResponse`] so the
/// operator UI can render a health badge alongside the credential
/// entry without the operator having to call a separate probe
/// endpoint.
///
/// Probes are best-effort: a probe failure does NOT abort
/// `credentials/register` (the credential still lands on disk and
/// the agent bindings still get written). Operators retry from
/// the UI when the probe reports unhealthy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CredentialValidationOutcome {
    /// `true` when the persister actually attempted a probe;
    /// `false` when the persister opted out (e.g. whatsapp
    /// pairing handles its own connectivity flow).
    pub probed: bool,
    /// `true` only when `probed` AND the probe reported the
    /// channel reachable + authenticated. `false` otherwise
    /// (including `probed = false`).
    pub healthy: bool,
    /// Human-readable detail for surfacing in the UI. Persister
    /// crafts this — keep it short, no trailing newline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Stable machine-readable code from
    /// [`reason_code`]. Used by tests + UI to render
    /// translated messages. `None` when persister did not
    /// classify the outcome.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}

/// Phase 82.10.n response shape for `nexo/admin/credentials/register`.
/// Wraps the existing [`CredentialSummary`] with the optional
/// persister probe outcome. Pre-82.10.n callers consumed
/// `CredentialSummary` directly — this is a breaking change for
/// internal SDK consumers (one-line accessor refactor).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CredentialRegisterResponse {
    /// Summary of the registered credential (channel, instance,
    /// agent ids bound).
    pub summary: CredentialSummary,
    /// Probe outcome from the registered channel persister.
    /// `None` when no persister is registered for the channel
    /// (opaque-only path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation: Option<CredentialValidationOutcome>,
}

/// Stable machine-readable reason codes for
/// [`CredentialValidationOutcome::reason_code`]. Mirrors the
/// pattern in `research/docs/auth-credential-semantics.md` —
/// codes are part of the wire contract, never mutated; new codes
/// land via additive change only.
pub mod reason_code {
    /// Probe completed; channel reachable + authenticated.
    pub const OK: &str = "ok";
    /// No persister registered for the channel id.
    pub const UNSUPPORTED_CHANNEL: &str = "unsupported_channel";
    /// Persister rejected the payload shape (missing token,
    /// wrong type, …).
    pub const INVALID_PAYLOAD: &str = "invalid_payload";
    /// Persister rejected the metadata shape (missing imap.host,
    /// wrong tls value, …).
    pub const INVALID_METADATA: &str = "invalid_metadata";
    /// Probe reached the network but the connection failed
    /// (DNS, TCP, timeout, circuit breaker open).
    pub const CONNECTIVITY_FAILED: &str = "connectivity_failed";
    /// Probe reached the provider but credentials were rejected
    /// (telegram 401, IMAP `NO`).
    pub const AUTH_FAILED: &str = "auth_failed";
    /// TLS handshake failed (cert invalid, protocol mismatch).
    pub const TLS_FAILED: &str = "tls_failed";
    /// Persister opted out of probing (whatsapp default).
    pub const NOT_PROBED: &str = "not_probed";
}

/// Params for `nexo/admin/credentials/revoke`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CredentialsRevokeParams {
    /// Channel id.
    pub channel: String,
    /// Instance discriminator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

/// Response for `nexo/admin/credentials/revoke`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CredentialsRevokeResponse {
    /// `true` when the filesystem entry was removed.
    /// `false` = was already absent (idempotent revoke).
    pub removed: bool,
    /// Agent ids whose `inbound_bindings` list was edited to
    /// remove the now-revoked credential.
    pub unbound_agents: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_summary_round_trip() {
        let s = CredentialSummary {
            channel: "whatsapp".into(),
            instance: Some("personal".into()),
            agent_ids: vec!["ana".into(), "carlos".into()],
        };
        let v = serde_json::to_value(&s).unwrap();
        let back: CredentialSummary = serde_json::from_value(v).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn register_input_default_empty_agent_ids_round_trip() {
        let i = CredentialRegisterInput {
            channel: "whatsapp".into(),
            instance: None,
            agent_ids: vec![],
            payload: serde_json::json!({ "token": "wa.X" }),
            metadata: HashMap::new(),
        };
        let v = serde_json::to_value(&i).unwrap();
        let back: CredentialRegisterInput = serde_json::from_value(v).unwrap();
        assert_eq!(i, back);
        assert!(back.instance.is_none());
    }

    #[test]
    fn revoke_response_default_empty() {
        let r = CredentialsRevokeResponse::default();
        assert!(!r.removed);
        assert!(r.unbound_agents.is_empty());
    }

    #[test]
    fn register_input_metadata_default_empty_skips_in_serialization() {
        let i = CredentialRegisterInput {
            channel: "telegram".into(),
            instance: Some("kate".into()),
            agent_ids: vec!["kate".into()],
            payload: serde_json::json!({ "token": "tg.X" }),
            metadata: HashMap::new(),
        };
        let v = serde_json::to_value(&i).unwrap();
        // Empty metadata should not serialise (back-compat: pre-82.10.n
        // callers + parsers see no `metadata` key).
        assert!(v.get("metadata").is_none());
        let back: CredentialRegisterInput = serde_json::from_value(v).unwrap();
        assert_eq!(i, back);
        assert!(back.metadata.is_empty());
    }

    #[test]
    fn register_input_with_metadata_round_trip() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "imap".into(),
            serde_json::json!({ "host": "imap.gmail.com", "port": 993, "tls": "implicit_tls" }),
        );
        metadata.insert("address".into(), serde_json::json!("ops@example.com"));
        let i = CredentialRegisterInput {
            channel: "email".into(),
            instance: Some("ops".into()),
            agent_ids: vec!["ana".into()],
            payload: serde_json::json!({ "password": "p" }),
            metadata,
        };
        let v = serde_json::to_value(&i).unwrap();
        assert!(v.get("metadata").is_some());
        let back: CredentialRegisterInput = serde_json::from_value(v).unwrap();
        assert_eq!(i, back);
    }

    #[test]
    fn register_input_pre_82_10_n_payload_parses_with_default_metadata() {
        // Wire frame as emitted by pre-82.10.n SDK callers (no
        // `metadata` field present).
        let v = serde_json::json!({
            "channel": "whatsapp",
            "instance": "personal",
            "agent_ids": ["ana"],
            "payload": { "token": "wa.X" }
        });
        let parsed: CredentialRegisterInput = serde_json::from_value(v).unwrap();
        assert!(parsed.metadata.is_empty());
        assert_eq!(parsed.channel, "whatsapp");
    }

    #[test]
    fn validation_outcome_round_trip_full() {
        let o = CredentialValidationOutcome {
            probed: true,
            healthy: false,
            detail: Some("IMAP LOGIN rejected".into()),
            reason_code: Some(reason_code::AUTH_FAILED.into()),
        };
        let v = serde_json::to_value(&o).unwrap();
        let back: CredentialValidationOutcome = serde_json::from_value(v).unwrap();
        assert_eq!(o, back);
    }

    #[test]
    fn validation_outcome_skips_optional_fields_when_absent() {
        let o = CredentialValidationOutcome {
            probed: false,
            healthy: false,
            detail: None,
            reason_code: None,
        };
        let v = serde_json::to_value(&o).unwrap();
        assert!(v.get("detail").is_none());
        assert!(v.get("reason_code").is_none());
    }

    #[test]
    fn register_response_round_trip_with_validation() {
        let r = CredentialRegisterResponse {
            summary: CredentialSummary {
                channel: "telegram".into(),
                instance: Some("kate".into()),
                agent_ids: vec!["kate".into()],
            },
            validation: Some(CredentialValidationOutcome {
                probed: true,
                healthy: true,
                detail: None,
                reason_code: Some(reason_code::OK.into()),
            }),
        };
        let v = serde_json::to_value(&r).unwrap();
        let back: CredentialRegisterResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn register_response_round_trip_no_validation_skips_field() {
        let r = CredentialRegisterResponse {
            summary: CredentialSummary {
                channel: "whatsapp".into(),
                instance: None,
                agent_ids: vec![],
            },
            validation: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("validation").is_none());
        let back: CredentialRegisterResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn reason_code_constants_are_stable() {
        // Locking the wire contract — these strings are part of
        // the public API contract for UI translation lookup.
        assert_eq!(reason_code::OK, "ok");
        assert_eq!(reason_code::UNSUPPORTED_CHANNEL, "unsupported_channel");
        assert_eq!(reason_code::INVALID_PAYLOAD, "invalid_payload");
        assert_eq!(reason_code::INVALID_METADATA, "invalid_metadata");
        assert_eq!(reason_code::CONNECTIVITY_FAILED, "connectivity_failed");
        assert_eq!(reason_code::AUTH_FAILED, "auth_failed");
        assert_eq!(reason_code::TLS_FAILED, "tls_failed");
        assert_eq!(reason_code::NOT_PROBED, "not_probed");
    }
}
