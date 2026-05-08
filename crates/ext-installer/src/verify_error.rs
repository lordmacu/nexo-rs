//! Phase 31.3 — error variants returned by the cosign signature
//! verification pipeline.

use std::path::PathBuf;

/// Errors surfaced by [`crate::verify::verify_plugin_signature`]
/// and the `trusted_keys.toml` loader.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// `cosign` binary not found on PATH or in any well-known
    /// location, and no `cosign_binary` override was configured.
    #[error("cosign binary not found (searched: {searched:?}). Install via brew/apt/dnf.")]
    CosignNotFound {
        /// Paths the discoverer probed.
        searched: Vec<PathBuf>,
    },

    /// `cosign verify-blob` exited with a non-zero status.
    #[error("cosign verify-blob exited non-zero: {stderr}")]
    CosignFailed {
        /// stderr captured from the cosign invocation. Preserved
        /// verbatim so operators can diagnose identity mismatches
        /// or transparency-log failures.
        stderr: String,
    },

    /// IO error during signature/cert/bundle download or local
    /// process spawning.
    #[error("verify io error: {0}")]
    Io(String),

    /// Trust policy is `Require` but the resolved release has no
    /// `.sig` + `.cert` assets.
    #[error(
        "trust policy requires signature for `{owner}`, but release has no .sig + .cert assets"
    )]
    PolicyRequiresSig {
        /// Repo owner that the policy resolved against.
        owner: String,
    },

    /// Release ships one half of the signing material but not the
    /// other (e.g. `.sig` without `.cert`). Implies a publish
    /// convention violation upstream; rejected outside `Ignore`.
    #[error("release has `{present}` but missing companion `{missing}`")]
    AssetIncomplete {
        /// Asset suffix that was present (e.g. `.sig`).
        present: &'static str,
        /// Asset suffix expected as companion (e.g. `.cert`).
        missing: &'static str,
    },

    /// `trusted_keys.toml` failed to parse.
    #[error("trusted_keys.toml at `{path}` invalid: {reason}")]
    TrustedKeysParse {
        /// Path on disk where the file was loaded from.
        path: PathBuf,
        /// Parse failure reason.
        reason: String,
    },

    /// One of the `[[authors]]` entries declared an
    /// `identity_regexp` that does not compile under the Rust
    /// regex engine.
    #[error("identity_regexp `{got}` invalid: {reason}")]
    IdentityRegexpInvalid {
        /// Offending regex string.
        got: String,
        /// `regex::Regex::new` error message.
        reason: String,
    },

    /// PEM/DER certificate could not be parsed.
    #[error("certificate parse failed: {0}")]
    CertParseFailed(String),

    /// Certificate is well-formed but does not carry an
    /// ECDSA-P256 public key — the only key type cosign uses
    /// today.
    #[error("certificate public key is not ECDSA-P256: {0}")]
    UnsupportedKey(String),

    /// Signature file could not be base64-decoded or DER-parsed.
    #[error("signature decode failed: {0}")]
    SignatureDecodeFailed(String),

    /// Signature did not verify against the certificate's public
    /// key over `SHA-256(blob)`. Either the blob, the
    /// certificate, or the signature has been tampered with.
    #[error("signature does not verify against the certificate")]
    SignatureMismatch,

    /// Certificate has no Subject Alternative Name URIs / emails
    /// — Fulcio always emits at least one, so an empty SAN
    /// almost certainly means we got handed the wrong cert.
    #[error("certificate has no Subject Alternative Name entries")]
    IdentityNotFound,

    /// Certificate's SAN entries do not match the policy's
    /// `identity_regexp`.
    #[error(
        "identity `{found}` does not match policy regex `{expected_regex}`"
    )]
    IdentityMismatch {
        /// First SAN entry the verifier inspected (URIs/emails
        /// joined for diagnostic clarity).
        found: String,
        /// Regex from the matched author entry.
        expected_regex: String,
    },

    /// Certificate's Fulcio OIDC-issuer extension does not match
    /// the policy.
    #[error(
        "OIDC issuer mismatch: cert claims `{found}`, policy requires `{expected}`"
    )]
    IssuerMismatch {
        /// What the cert's `1.3.6.1.4.1.57264.1.{1,8}` extension
        /// reported.
        found: String,
        /// What the trust policy required.
        expected: String,
    },
}
