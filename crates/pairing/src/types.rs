//! Public types shared by store / gate / setup-code.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRequest {
    pub channel: String,
    pub account_id: String,
    pub sender_id: String,
    pub code: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub meta: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedRequest {
    pub channel: String,
    pub account_id: String,
    pub sender_id: String,
    pub approved_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UpsertOutcome {
    pub code: String,
    /// `true` when this call inserted a new pending row, `false` when
    /// it returned the code from an existing one.
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Sender is in `allow_from`; publish as normal.
    Admit,
    /// Sender is unknown; reply with `code` and drop the message.
    Challenge { code: String },
    /// Drop without reply (max-pending exhausted, or `auto_challenge`
    /// is off and sender is unknown).
    Drop,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingPolicy {
    /// When `true`, unknown senders trigger a challenge reply. When
    /// `false`, the gate is a no-op (every message is admitted).
    /// Default `false` keeps existing setups working without changes.
    #[serde(default)]
    pub auto_challenge: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupCode {
    pub url: String,
    pub bootstrap_token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenClaims {
    pub profile: String,
    pub expires_at: DateTime<Utc>,
    pub nonce: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_label: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    #[error("unknown code")]
    UnknownCode,
    #[error("expired")]
    Expired,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("max pending reached for {channel}:{account_id}")]
    MaxPending { channel: String, account_id: String },
    #[error("storage: {0}")]
    Storage(String),
    #[error("io: {0}")]
    Io(String),
    #[error("invalid: {0}")]
    Invalid(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_round_trip() {
        let p = PendingRequest {
            channel: "whatsapp".into(),
            account_id: "personal".into(),
            sender_id: "+57111".into(),
            code: "ABCDEFGH".into(),
            created_at: Utc::now(),
            meta: serde_json::json!({"k":"v"}),
        };
        let s = serde_json::to_string(&p).unwrap();
        let p2: PendingRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(p.code, p2.code);
        assert_eq!(p.meta, p2.meta);
    }

    #[test]
    fn policy_defaults_off() {
        let p: PairingPolicy = serde_json::from_str("{}").unwrap();
        assert!(!p.auto_challenge);
    }

    #[test]
    fn policy_rejects_unknown_keys() {
        let res: Result<PairingPolicy, _> = serde_json::from_str("{\"bogus\": true}");
        assert!(res.is_err());
    }
}
