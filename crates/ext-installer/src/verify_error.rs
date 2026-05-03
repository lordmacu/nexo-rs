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
    #[error("trust policy requires signature for `{owner}`, but release has no .sig + .cert assets")]
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
}
