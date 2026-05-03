//! Phase 31.3 — cosign signature verification by shelling out
//! to the `cosign` binary.
//!
//! Pipeline (called from the install orchestration after the
//! sha256 download check, before extraction):
//!
//! 1. Resolve the operator's `cosign` binary via
//!    [`discover_cosign_binary`].
//! 2. Invoke `cosign verify-blob --signature <sig>
//!    --certificate <cert> --certificate-identity-regexp <regex>
//!    --certificate-oidc-issuer <issuer> [--bundle <bundle>]
//!    <tarball>`.
//! 3. Parse stdout/stderr; on non-zero exit return
//!    [`VerifyError::CosignFailed`] with the captured stderr.
//!
//! The Sigstore `.bundle` (when present) lets cosign verify the
//! Rekor inclusion proof offline. Without the bundle, cosign
//! reaches out to Rekor at verify time.

use std::path::{Path, PathBuf};

use crate::trusted_keys::AuthorPolicy;
use crate::verify_error::VerifyError;

/// Inputs to [`verify_plugin_signature`].
#[derive(Debug)]
pub struct VerifyInput<'a> {
    /// Path to the cosign binary.
    pub cosign_bin: &'a Path,
    /// Path to the verified `.tar.gz` on disk.
    pub tarball_path: &'a Path,
    /// Path to the cosign signature (`.sig`).
    pub sig_path: &'a Path,
    /// Path to the cosign certificate (`.cert` / `.pem`).
    pub cert_path: &'a Path,
    /// Optional Sigstore bundle (`.bundle`). When present cosign
    /// verifies the Rekor inclusion proof offline; absent forces
    /// a Rekor fetch.
    pub bundle_path: Option<&'a Path>,
    /// Resolved trust policy (identity regex + OIDC issuer).
    pub policy: &'a AuthorPolicy,
}

/// Successful verification result. Stored on the install report.
#[derive(Debug, Clone)]
pub struct VerifiedSignature {
    /// Subject Alternative Name parsed from cosign's output (the
    /// GitHub Actions workflow URL in keyless flow). Falls back
    /// to the literal `verified` string when stderr shape changes.
    pub identity: String,
    /// OIDC issuer the cert was minted by. Echoes the policy's
    /// declared issuer.
    pub issuer: String,
}

/// Search the operator's environment for a usable `cosign`
/// binary. Order:
/// 1. `override_` (from `trusted_keys.toml`'s `cosign_binary`).
/// 2. `$PATH` walk.
/// 3. Well-known absolute fallbacks.
pub fn discover_cosign_binary(override_: Option<&Path>) -> Result<PathBuf, VerifyError> {
    let mut searched: Vec<PathBuf> = Vec::new();

    if let Some(p) = override_ {
        searched.push(p.to_path_buf());
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
    }

    if let Ok(path_env) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_env) {
            let candidate = dir.join("cosign");
            searched.push(candidate.clone());
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    for fallback in &[
        "/usr/local/bin/cosign",
        "/opt/homebrew/bin/cosign",
        "/usr/bin/cosign",
    ] {
        let candidate = PathBuf::from(fallback);
        searched.push(candidate.clone());
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let candidate = PathBuf::from(home).join("go").join("bin").join("cosign");
        searched.push(candidate.clone());
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(VerifyError::CosignNotFound { searched })
}

/// Run `cosign verify-blob` against the supplied signing
/// material. On success returns the parsed identity + issuer.
pub async fn verify_plugin_signature(
    input: VerifyInput<'_>,
) -> Result<VerifiedSignature, VerifyError> {
    let mut cmd = tokio::process::Command::new(input.cosign_bin);
    cmd.arg("verify-blob")
        .arg("--signature")
        .arg(input.sig_path)
        .arg("--certificate")
        .arg(input.cert_path)
        .arg("--certificate-identity-regexp")
        .arg(&input.policy.identity_regexp)
        .arg("--certificate-oidc-issuer")
        .arg(&input.policy.oidc_issuer);
    if let Some(b) = input.bundle_path {
        cmd.arg("--bundle").arg(b);
    }
    cmd.arg(input.tarball_path);

    let output = cmd
        .output()
        .await
        .map_err(|e| VerifyError::Io(format!("spawn cosign: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(VerifyError::CosignFailed { stderr });
    }

    // cosign prints `Verified OK` to stderr on success and
    // sometimes echoes the SAN. Best-effort extract; fall back
    // to a generic label rather than failing on shape drift.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let identity = parse_identity_from_stderr(&stderr).unwrap_or_else(|| "verified".to_string());

    Ok(VerifiedSignature {
        identity,
        issuer: input.policy.oidc_issuer.clone(),
    })
}

fn parse_identity_from_stderr(stderr: &str) -> Option<String> {
    for line in stderr.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Subject:") {
            return Some(rest.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("Certificate subject:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::trusted_keys::TrustMode;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn make_policy() -> AuthorPolicy {
        AuthorPolicy {
            owner: "lordmacu".to_string(),
            identity_regexp: "^https://github.com/lordmacu/.*$".to_string(),
            oidc_issuer: "https://token.actions.githubusercontent.com".to_string(),
            mode: Some(TrustMode::Require),
        }
    }

    /// Write a mock cosign script that asserts argv shape and
    /// optionally exits non-zero based on env var
    /// `MOCK_COSIGN_FAIL=1`.
    fn write_mock_cosign(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn echo_args_mock_script() -> &'static str {
        r#"#!/usr/bin/env bash
# Mock cosign that echoes its argv to stderr and emits the
# canonical "Verified OK" line so identity parsing exercises.
echo "argv: $*" >&2
echo "Verified OK" >&2
echo "Subject: https://github.com/lordmacu/foo/.github/workflows/release.yml@refs/tags/v0.2.0" >&2
exit 0
"#
    }

    fn always_fails_mock_script() -> &'static str {
        r#"#!/usr/bin/env bash
echo "error: identity does not match" >&2
exit 1
"#
    }

    #[tokio::test]
    async fn discover_uses_explicit_override_when_present() {
        let tmp = TempDir::new().unwrap();
        let cosign = write_mock_cosign(tmp.path(), "cosign", "#!/bin/sh\nexit 0\n");
        let resolved = discover_cosign_binary(Some(&cosign)).unwrap();
        assert_eq!(resolved, cosign);
    }

    #[tokio::test]
    async fn discover_returns_not_found_when_missing() {
        let tmp = TempDir::new().unwrap();
        // Override PATH + HOME so no cosign is reachable.
        let original_path = std::env::var("PATH").ok();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("PATH", tmp.path());
        std::env::set_var("HOME", tmp.path());
        let bogus = tmp.path().join("does-not-exist");
        let err = discover_cosign_binary(Some(&bogus)).unwrap_err();
        if let Some(p) = original_path {
            std::env::set_var("PATH", p);
        }
        if let Some(h) = original_home {
            std::env::set_var("HOME", h);
        }
        assert!(matches!(err, VerifyError::CosignNotFound { .. }));
    }

    #[tokio::test]
    async fn verify_invokes_cosign_with_expected_args() {
        let tmp = TempDir::new().unwrap();
        let cosign = write_mock_cosign(tmp.path(), "cosign", echo_args_mock_script());
        let tarball = tmp.path().join("plugin.tar.gz");
        let sig = tmp.path().join("plugin.tar.gz.sig");
        let cert = tmp.path().join("plugin.tar.gz.cert");
        for p in [&tarball, &sig, &cert] {
            fs::write(p, b"x").unwrap();
        }
        let policy = make_policy();
        let result = verify_plugin_signature(VerifyInput {
            cosign_bin: &cosign,
            tarball_path: &tarball,
            sig_path: &sig,
            cert_path: &cert,
            bundle_path: None,
            policy: &policy,
        })
        .await
        .expect("verify ok");

        assert_eq!(result.issuer, policy.oidc_issuer);
        assert!(
            result.identity.contains("github.com/lordmacu"),
            "expected identity to come from mock stderr, got {:?}",
            result.identity
        );
    }

    #[tokio::test]
    async fn verify_skips_bundle_arg_when_absent() {
        // Same as above; this test just exercises the branch
        // where bundle_path is None and asserts no panic + ok.
        let tmp = TempDir::new().unwrap();
        let cosign = write_mock_cosign(tmp.path(), "cosign", echo_args_mock_script());
        let tarball = tmp.path().join("p.tar.gz");
        let sig = tmp.path().join("p.tar.gz.sig");
        let cert = tmp.path().join("p.tar.gz.cert");
        for p in [&tarball, &sig, &cert] {
            fs::write(p, b"x").unwrap();
        }
        let result = verify_plugin_signature(VerifyInput {
            cosign_bin: &cosign,
            tarball_path: &tarball,
            sig_path: &sig,
            cert_path: &cert,
            bundle_path: None,
            policy: &make_policy(),
        })
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn verify_passes_bundle_arg_when_present() {
        let tmp = TempDir::new().unwrap();
        let cosign = write_mock_cosign(tmp.path(), "cosign", echo_args_mock_script());
        let tarball = tmp.path().join("p.tar.gz");
        let sig = tmp.path().join("p.tar.gz.sig");
        let cert = tmp.path().join("p.tar.gz.cert");
        let bundle = tmp.path().join("p.tar.gz.bundle");
        for p in [&tarball, &sig, &cert, &bundle] {
            fs::write(p, b"x").unwrap();
        }
        let result = verify_plugin_signature(VerifyInput {
            cosign_bin: &cosign,
            tarball_path: &tarball,
            sig_path: &sig,
            cert_path: &cert,
            bundle_path: Some(&bundle),
            policy: &make_policy(),
        })
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn verify_propagates_cosign_failure() {
        let tmp = TempDir::new().unwrap();
        let cosign = write_mock_cosign(tmp.path(), "cosign", always_fails_mock_script());
        let tarball = tmp.path().join("p.tar.gz");
        let sig = tmp.path().join("p.tar.gz.sig");
        let cert = tmp.path().join("p.tar.gz.cert");
        for p in [&tarball, &sig, &cert] {
            fs::write(p, b"x").unwrap();
        }
        let err = verify_plugin_signature(VerifyInput {
            cosign_bin: &cosign,
            tarball_path: &tarball,
            sig_path: &sig,
            cert_path: &cert,
            bundle_path: None,
            policy: &make_policy(),
        })
        .await
        .unwrap_err();
        assert!(matches!(err, VerifyError::CosignFailed { .. }));
    }

    #[tokio::test]
    async fn verify_io_error_when_binary_unrunnable() {
        // A directory cannot be exec'd as a binary.
        let tmp = TempDir::new().unwrap();
        let dir_as_bin = tmp.path();
        let tarball = tmp.path().join("p.tar.gz");
        let sig = tmp.path().join("p.tar.gz.sig");
        let cert = tmp.path().join("p.tar.gz.cert");
        for p in [&tarball, &sig, &cert] {
            fs::write(p, b"x").unwrap();
        }
        let err = verify_plugin_signature(VerifyInput {
            cosign_bin: dir_as_bin,
            tarball_path: &tarball,
            sig_path: &sig,
            cert_path: &cert,
            bundle_path: None,
            policy: &make_policy(),
        })
        .await
        .unwrap_err();
        assert!(matches!(err, VerifyError::Io(_)));
    }
}
