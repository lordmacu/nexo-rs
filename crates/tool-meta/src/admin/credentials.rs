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
}
