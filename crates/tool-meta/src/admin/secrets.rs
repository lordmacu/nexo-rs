//! Phase 82.10.k — `nexo/admin/secrets/*` wire types.
//!
//! Microapps with the `secrets_write` capability persist
//! operator-supplied secrets (LLM API keys, channel tokens, …)
//! to `<state_root>/secrets/<NAME>.txt` (mode 0600) AND have
//! the daemon `std::env::set_var(name, value)` so existing
//! `std::env::var(name)` consumers (LLM clients, plugin auth)
//! pick up the new value without a restart.
//!
//! Audit log redacts `value` per `audit_sqlite::redact_for_audit`
//! — only `name` is recorded.
//!
//! M9.frame.a (microapp follow-up): wires the wizard's Step 1
//! through this RPC to skip the operator's manual `export`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// JSON-RPC method for `secrets/write`. Microapps with the
/// `secrets_write` capability invoke this to atomically persist
/// a secret + inject it into the daemon's process env.
pub const SECRETS_WRITE_METHOD: &str = "nexo/admin/secrets/write";

/// Params for `nexo/admin/secrets/write`.
///
/// `Debug` is custom — it redacts `value` so the cleartext never
/// lands in process logs.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretsWriteInput {
    /// Env-var-style name. Validated server-side against
    /// `^[A-Z][A-Z0-9_]{1,63}$` (path-traversal defence + matches
    /// conventional env-var shape).
    pub name: String,
    /// Cleartext value. Length 1-8192 bytes (validated
    /// server-side). Persisted to
    /// `<state_root>/secrets/<NAME>.txt` (mode 0600) and
    /// injected via `std::env::set_var`.
    pub value: String,
}

impl std::fmt::Debug for SecretsWriteInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretsWriteInput")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Response for `nexo/admin/secrets/write`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SecretsWriteResponse {
    /// Filesystem path the daemon wrote. Operator-friendly for
    /// "where's my secret stored" questions.
    pub path: PathBuf,
    /// `true` when an existing process-env value was overwritten
    /// (was set before the call). `false` for a brand-new key.
    pub overwrote_env: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_write_input_round_trip() {
        let i = SecretsWriteInput {
            name: "MINIMAX_API_KEY".into(),
            value: "sk-test-value".into(),
        };
        let v = serde_json::to_value(&i).unwrap();
        let back: SecretsWriteInput = serde_json::from_value(v).unwrap();
        assert_eq!(i, back);
    }

    /// `Debug` MUST NOT surface the cleartext `value` — protects
    /// the secret when the dispatcher's tracing layer logs the
    /// frame for diagnostics.
    #[test]
    fn secrets_write_input_debug_redacts_value() {
        let i = SecretsWriteInput {
            name: "FOO".into(),
            value: "leak-me-please".into(),
        };
        let s = format!("{i:?}");
        assert!(s.contains("FOO"));
        assert!(s.contains("<redacted>"));
        assert!(!s.contains("leak-me-please"));
    }

    #[test]
    fn secrets_write_method_constant() {
        assert_eq!(SECRETS_WRITE_METHOD, "nexo/admin/secrets/write");
    }

    #[test]
    fn secrets_write_response_round_trip() {
        let r = SecretsWriteResponse {
            path: PathBuf::from("/tmp/secrets/FOO.txt"),
            overwrote_env: true,
        };
        let v = serde_json::to_value(&r).unwrap();
        let back: SecretsWriteResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r, back);
    }
}
