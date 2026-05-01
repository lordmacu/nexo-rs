//! Phase 82.12 — HTTP server capability wire shapes.
//!
//! Microapps that declare `[capabilities.http_server]` in
//! `plugin.toml` get an operator-provided shared bearer token
//! at boot. Token rotation arrives as a JSON-RPC notification
//! whose method is [`TOKEN_ROTATED_NOTIFY_METHOD`] — microapps
//! swap the in-memory secret without restart.
//!
//! Health checks ride on whatever path the manifest declares
//! (default `/healthz`). The boot supervisor that polls it
//! lives daemon-side in `nexo-setup`; this module only owns
//! the wire-shape types both halves share.

use serde::{Deserialize, Serialize};

/// JSON-RPC notification method emitted by the daemon when an
/// operator rotates a microapp's `token_env`. Frame shape:
/// `{"jsonrpc":"2.0","method":"nexo/notify/token_rotated",
///  "params": <TokenRotated>}`.
pub const TOKEN_ROTATED_NOTIFY_METHOD: &str = "nexo/notify/token_rotated";

/// Token rotation payload. Microapps validate against
/// `old_hash` (SHA-256 hex of the previous bearer token,
/// truncated to 16 chars to keep it small on the wire) before
/// swapping in `new`. Hash check guards against stale rotation
/// notifications hitting a microapp that has already been
/// restarted onto a different token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenRotated {
    /// SHA-256 hex of the OLD token, truncated to the first 16
    /// chars. Microapps compare against the hash they computed
    /// at boot. Mismatch → ignore the notification (warn log).
    pub old_hash: String,
    /// The replacement bearer token in cleartext. Microapps
    /// stash it in memory only; the daemon's source of truth
    /// is whatever value is set in `token_env`.
    pub new: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_rotated_round_trip() {
        let evt = TokenRotated {
            old_hash: "abcdef0123456789".into(),
            new: "fresh-token".into(),
        };
        let v = serde_json::to_value(&evt).unwrap();
        assert_eq!(v["old_hash"], "abcdef0123456789");
        assert_eq!(v["new"], "fresh-token");
        let back: TokenRotated = serde_json::from_value(v).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn notify_method_constant() {
        assert_eq!(TOKEN_ROTATED_NOTIFY_METHOD, "nexo/notify/token_rotated");
    }
}
