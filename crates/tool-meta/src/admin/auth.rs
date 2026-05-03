//! Phase 82.10.o — `nexo/admin/auth/*` wire types.
//!
//! Today exposes `auth/rotate_token` only. Operator triggers
//! a rotation via the microapp UI; daemon validates the new
//! value, persists it to the canonical operator-token file, and
//! emits `nexo/notify/token_rotated` so live microapp listeners
//! swap their `LiveTokenState` without a restart.
//!
//! Pairs with the audit emit on the firehose
//! ([`crate::admin::agent_events::SecurityEventKind::TokenRotated`])
//! so compliance audits get a durable record of every rotation
//! alongside the standard agent-event timeline.

use serde::{Deserialize, Serialize};

/// JSON-RPC method literal for `auth/rotate_token`.
pub const AUTH_ROTATE_METHOD: &str = "nexo/admin/auth/rotate_token";

/// Minimum length for an operator-supplied token. Defence-
/// in-depth: rejects trivially short bearers (`""` / `"x"`) at
/// the wire level so the daemon never persists a degenerate
/// secret. The handler clamps client input via this constant.
pub const MIN_TOKEN_LEN: usize = 16;

/// Maximum length for the operator-supplied audit `reason`.
/// The handler truncates longer values before emitting the
/// audit firehose event so subscribers see a bounded payload.
pub const REASON_MAX_LEN: usize = 200;

/// Params for `nexo/admin/auth/rotate_token`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AuthRotateInput {
    /// New bearer token. When `None`, the daemon generates a
    /// 32-byte URL-safe random value via the production
    /// adapter's CSPRNG. When `Some`, the daemon validates
    /// `len() >= `[`MIN_TOKEN_LEN`] and rejects shorter input
    /// with `InvalidParams`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_token: Option<String>,
    /// Operator-supplied audit hint (e.g. "scheduled rotation",
    /// "key compromised"). Surfaces in the audit emit. Capped
    /// to [`REASON_MAX_LEN`] chars by the handler.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Response for `nexo/admin/auth/rotate_token`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthRotateResponse {
    /// Always `true` on success. Failures surface via the
    /// `AdminError::*` typed surface, not via `ok: false`.
    pub ok: bool,
    /// New `operator_token_hash` (16-char sha256-hex prefix).
    /// Returned so the microapp UI can display "rotated to
    /// `dead…cafe`" without re-fetching.
    pub new_hash: String,
    /// Epoch milliseconds when the rotation persisted.
    pub at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_literal_matches_expected_jsonrpc_path() {
        assert_eq!(AUTH_ROTATE_METHOD, "nexo/admin/auth/rotate_token");
    }

    #[test]
    fn input_round_trips_with_optional_fields() {
        // Default (no new_token, no reason).
        let i = AuthRotateInput::default();
        let v = serde_json::to_value(&i).unwrap();
        // Optional fields skipped on the wire.
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("new_token"));
        assert!(!obj.contains_key("reason"));
        let back: AuthRotateInput = serde_json::from_value(v).unwrap();
        assert_eq!(i, back);

        // Both fields set.
        let i2 = AuthRotateInput {
            new_token: Some("super-secret-bearer-32".into()),
            reason: Some("scheduled rotation".into()),
        };
        let v2 = serde_json::to_value(&i2).unwrap();
        assert_eq!(v2["new_token"], "super-secret-bearer-32");
        assert_eq!(v2["reason"], "scheduled rotation");
        let back2: AuthRotateInput = serde_json::from_value(v2).unwrap();
        assert_eq!(i2, back2);
    }

    #[test]
    fn response_round_trip_preserves_fields() {
        let r = AuthRotateResponse {
            ok: true,
            new_hash: "deadbeefcafebabe".into(),
            at_ms: 1_700_000_000_000,
        };
        let v = serde_json::to_value(&r).unwrap();
        let back: AuthRotateResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r, back);
        assert_eq!(r.new_hash.len(), 16);
    }

    #[test]
    fn min_token_len_is_16() {
        // Lock the constant — bumping it requires a deliberate
        // wire-shape change + microapp UI validation update.
        assert_eq!(MIN_TOKEN_LEN, 16);
    }
}
