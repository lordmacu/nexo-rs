//! M2 — sha256 + size of MCP `tools/call` arguments for audit rows.
//!
//! Provider-agnostic: operates on `serde_json::Value` from the MCP
//! wire envelope. Independent of which LLM client (Claude Desktop /
//! Cursor / Continue / Cody / Aider) or which underlying LLM
//! provider (Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI
//! / Mistral / future) drives the call.
//!
//! ## Truncation length: 16 hex chars (64 bits)
//!
//! Chosen to match prior art:
//!
//! * `claude-code-leak/src/services/mcp/utils.ts:157-168`
//!   `hashMcpConfig` — `slice(0, 16)`.
//! * `claude-code-leak/src/utils/pasteStore.ts:22` — `slice(0, 16)`.
//! * `claude-code-leak/src/utils/fileOperationAnalytics.ts:15` —
//!   `slice(0, 16)`.
//! * `claude-code-leak/src/utils/fileHistory.ts:729` — `slice(0, 16)`.
//! * `claude-code-leak/src/utils/telemetry/pluginTelemetry.ts:53` —
//!   `slice(0, 16)`.
//!
//! 64 bits gives ~1.8e19 unique values; collision improbable below
//! ~4 billion rows. Sufficient for forensic correlation, not
//! cryptographic identity.
//!
//! Patterns considered and discarded:
//! * `research/src/agents/cache-trace.ts:160-162` — full sha256 hex
//!   (no truncation). Overkill for correlation analytics.
//! * `research/src/agents/tool-call-id.ts:201-203` `shortHash` —
//!   `slice(0, 8)`. Different use case (collision avoidance for
//!   provider-mandated id format), too short for correlation.

use std::fmt::Write;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::server::audit_log::config::AuditLogConfig;

/// SHA-256 over `bytes`, hex-encode the first 8 bytes, return the
/// resulting 16 lowercase hex chars (64 bits). Pure / deterministic.
///
/// Manual hex format avoids a `hex` crate dependency on the `mcp`
/// crate; sha256 always produces 32 bytes so taking the first 8 is
/// total. Used directly by tests and indirectly via
/// [`compute_args_metrics`].
pub(crate) fn args_hash_truncated(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        // `write!` on a `String` is infallible.
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// Single-serialize the MCP `tools/call` arguments + apply the
/// [`AuditLogConfig`] knobs to decide whether to record the hash.
/// Returns `(Option<String> hash, u64 size)`.
///
/// Decision tree (in order):
///
/// 1. Resolve effective `redact` flag:
///    `cfg.per_tool_redact_args.get(tool_name).copied().unwrap_or(cfg.redact_args)`.
///    Per-tool override wins over the global default.
/// 2. Compute `bytes = serde_json::to_vec(args).unwrap_or_default()`.
///    Default `Vec::new()` on the (improbable) serialize failure.
/// 3. `size = bytes.len() as u64` — always recorded, regardless of
///    config.
/// 4. Hash recorded only when:
///    - `redact == true` (operator wants raw args NOT stored — the
///      hash is the redacted-but-correlatable surrogate); AND
///    - `bytes.len() <= cfg.args_hash_max_bytes` (skip hash on
///      oversized payloads to bound CPU cost; size still recorded).
/// 5. Otherwise hash is `None`.
///
/// Provider-agnostic: `args` is the raw `Value` from the MCP
/// `params.arguments` field; the function makes no assumption about
/// which LLM client produced it.
pub(crate) fn compute_args_metrics(
    args: &Value,
    cfg: &AuditLogConfig,
    tool_name: &str,
) -> (Option<String>, u64) {
    let bytes = serde_json::to_vec(args).unwrap_or_default();
    let size = bytes.len() as u64;
    let redact = cfg
        .per_tool_redact_args
        .get(tool_name)
        .copied()
        .unwrap_or(cfg.redact_args);
    let hash = if redact && bytes.len() <= cfg.args_hash_max_bytes {
        Some(args_hash_truncated(&bytes))
    } else {
        None
    };
    (hash, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn cfg_default() -> AuditLogConfig {
        AuditLogConfig::default()
    }

    #[test]
    fn args_hash_truncated_returns_16_lowercase_hex() {
        let h = args_hash_truncated(b"hello world");
        assert_eq!(h.len(), 16, "expected 16 chars, got {}", h.len());
        assert!(
            h.chars().all(|c| c.is_ascii_hexdigit()),
            "expected hex chars, got `{h}`"
        );
        // hex::encode lowercases by default; assert no uppercase.
        assert!(
            h.chars().all(|c| !c.is_ascii_uppercase()),
            "expected lowercase, got `{h}`"
        );
    }

    #[test]
    fn args_hash_truncated_deterministic_for_same_input() {
        let a = args_hash_truncated(b"identical bytes");
        let b = args_hash_truncated(b"identical bytes");
        assert_eq!(a, b);
    }

    #[test]
    fn args_hash_truncated_changes_with_input() {
        let a = args_hash_truncated(b"a");
        let b = args_hash_truncated(b"b");
        assert_ne!(a, b);
    }

    #[test]
    fn compute_args_metrics_happy_path() {
        let args = json!({"to": "x@y.com", "subject": "hi"});
        let cfg = cfg_default();
        let (hash, size) = compute_args_metrics(&args, &cfg, "email_send");
        assert!(hash.is_some(), "default cfg should hash");
        assert_eq!(hash.as_ref().unwrap().len(), 16);
        assert!(size > 0, "JSON-serialized object must be > 0 bytes");
    }

    #[test]
    fn compute_args_metrics_redact_false_returns_none_hash() {
        let args = json!({"to": "x@y.com"});
        let mut cfg = cfg_default();
        cfg.redact_args = false;
        let (hash, size) = compute_args_metrics(&args, &cfg, "email_send");
        assert!(hash.is_none(), "redact_args=false → no hash");
        assert!(size > 0, "size still recorded regardless");
    }

    #[test]
    fn compute_args_metrics_per_tool_redact_override_wins() {
        let args = json!({"cmd": "ls -la"});
        let mut cfg = cfg_default();
        // Global says redact (hash); per-tool override says don't.
        cfg.redact_args = true;
        let mut overrides = BTreeMap::new();
        overrides.insert("bash".to_string(), false);
        cfg.per_tool_redact_args = overrides;

        // Bash (per-tool override = no redact) → no hash.
        let (hash_bash, size_bash) = compute_args_metrics(&args, &cfg, "bash");
        assert!(hash_bash.is_none(), "per-tool override (false) wins for bash");
        assert!(size_bash > 0);

        // Other tool (no override) → falls back to global redact=true → hash.
        let (hash_email, size_email) = compute_args_metrics(&args, &cfg, "email_send");
        assert!(
            hash_email.is_some(),
            "global redact=true applies to non-overridden tool"
        );
        assert!(size_email > 0);
    }

    #[test]
    fn compute_args_metrics_payload_over_cap_returns_none_hash() {
        // Build args whose serialized form exceeds 4 bytes easily.
        let args = json!({"x": "spam".repeat(100)});
        let mut cfg = cfg_default();
        cfg.args_hash_max_bytes = 4;
        let (hash, size) = compute_args_metrics(&args, &cfg, "any_tool");
        assert!(hash.is_none(), "payload > cap → skip hash");
        assert!(
            size > 4,
            "size still recorded (got {size}) so the row reflects the actual byte count"
        );
    }

    #[test]
    fn compute_args_metrics_size_zero_when_args_null() {
        let args = Value::Null;
        let cfg = cfg_default();
        let (hash, size) = compute_args_metrics(&args, &cfg, "any_tool");
        // serde_json::to_vec(&Null) = b"null" → 4 bytes.
        assert_eq!(size, 4, "Null serializes to b\"null\" (4 bytes)");
        // Default cfg → redact_args=true and 4 ≤ 1 MiB → hash present.
        assert!(hash.is_some());
        assert_eq!(hash.unwrap().len(), 16);
    }

    /// Provider-agnostic regression: the helper must produce
    /// identical output regardless of which LLM client constructed
    /// the args (the JSON shape is the only thing it sees). This
    /// test exercises four provider-shaped args (Anthropic-style
    /// nested message, OpenAI-style chat tool args, MiniMax-style,
    /// generic) and confirms each yields a 16-char hash + size > 0.
    #[test]
    fn compute_args_metrics_is_provider_agnostic() {
        let cfg = cfg_default();
        let cases = [
            // Anthropic-style tool_use arguments
            json!({"messages": [{"role": "user", "content": "x"}], "model": "claude"}),
            // OpenAI-style chat tool args
            json!({"function": {"name": "f", "arguments": "{}"}}),
            // MiniMax-style
            json!({"prompt": "x", "temperature": 0.7}),
            // Generic plain MCP tool args
            json!({"path": "/tmp/foo", "mode": 0o644}),
        ];
        let mut hashes = Vec::new();
        for args in &cases {
            let (hash, size) = compute_args_metrics(args, &cfg, "any_tool");
            assert!(hash.is_some(), "every shape should hash under default cfg");
            assert_eq!(hash.as_ref().unwrap().len(), 16);
            assert!(size > 0);
            hashes.push(hash.unwrap());
        }
        // Distinct shapes should produce distinct hashes (no
        // accidental cross-shape collision in the truncation).
        let unique: std::collections::HashSet<_> = hashes.iter().collect();
        assert_eq!(
            unique.len(),
            hashes.len(),
            "distinct provider shapes should produce distinct hashes"
        );
    }
}
