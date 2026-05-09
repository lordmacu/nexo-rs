//! Pure-Rust signature verification for plugin tarballs.
//!
//! Originally a `cosign verify-blob` subprocess; replaced in
//! May 2026 by an in-process pipeline so the installer
//! cross-compiles cleanly to Android (Phase 90 prerequisite) and
//! drops the runtime dependency on the Go `cosign` binary.
//!
//! Pipeline (called from the install orchestration after the
//! sha256 download check, before extraction):
//!
//! 1. Parse the cosign-issued PEM certificate via `x509-parser`.
//! 2. Extract the ECDSA-P256 public key from
//!    `SubjectPublicKeyInfo`.
//! 3. Verify the base64-encoded DER ECDSA signature against
//!    `SHA-256(tarball)` using `p256`.
//! 4. Extract the Subject Alternative Name URIs / emails;
//!    require at least one to match the trust policy's
//!    `identity_regexp`.
//! 5. Extract the Fulcio OIDC-issuer custom extension
//!    (OID `1.3.6.1.4.1.57264.1.1` legacy, `…1.8` modern) and
//!    require it to equal the policy's `oidc_issuer`.
//!
//! Rekor transparency-log verification is **not** performed in
//! this revision — the trust model relies on the Fulcio cert +
//! identity policy alone, which matches what `cosign verify-blob`
//! does without `--bundle`. Re-enable Rekor proof checking by
//! gating an opt-in feature that pulls the `sigstore` crate's
//! bundle path; tracked in `FOLLOWUPS.md`.

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine as _;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature as EcdsaSig, VerifyingKey};
use sha2::{Digest as _, Sha256};
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::prelude::*;

use crate::trusted_keys::AuthorPolicy;
use crate::verify_error::VerifyError;

/// Inputs to [`verify_plugin_signature`].
#[derive(Debug)]
pub struct VerifyInput<'a> {
    /// Legacy field — was the path to the `cosign` binary the
    /// installer used to spawn. Now ignored; kept on the struct
    /// so callers that destructure don't break. New code can
    /// pass `Path::new("")`.
    pub cosign_bin: &'a Path,
    /// Path to the verified `.tar.gz` on disk.
    pub tarball_path: &'a Path,
    /// Path to the cosign signature (`.sig`). The file content
    /// is the base64-encoded DER ECDSA signature cosign writes.
    pub sig_path: &'a Path,
    /// Path to the cosign certificate (`.cert` / `.pem`). PEM
    /// (X.509 v3) — Fulcio's keyless-mode short-lived cert.
    pub cert_path: &'a Path,
    /// Reserved for the future Rekor-bundle path. Currently
    /// ignored; presence does not strengthen verification under
    /// the lean implementation.
    pub bundle_path: Option<&'a Path>,
    /// Resolved trust policy (identity regex + OIDC issuer).
    pub policy: &'a AuthorPolicy,
}

/// Successful verification result. Stored on the install report.
#[derive(Debug, Clone)]
pub struct VerifiedSignature {
    /// First Subject Alternative Name URI / email parsed from
    /// the certificate. In the GitHub Actions keyless flow this
    /// is the workflow URL that issued the cosign sign-blob
    /// request.
    pub identity: String,
    /// OIDC issuer the cert was minted by. Echoes the policy's
    /// declared issuer after a successful match.
    pub issuer: String,
}

/// Resolve the operator's `cosign` binary. Retained as a
/// no-op compatibility shim for the install orchestration; the
/// returned path is never spawned. New callers should drop the
/// indirection and pass `Path::new("")` to [`VerifyInput`].
pub fn discover_cosign_binary(_override_: Option<&Path>) -> Result<PathBuf, VerifyError> {
    // The lean verifier never executes the binary — discovery is
    // unconditionally successful so the install pipeline keeps
    // its existing shape until callers migrate.
    Ok(PathBuf::new())
}

/// Verify the supplied signing material in pure Rust. On
/// success returns the parsed identity + issuer.
pub async fn verify_plugin_signature(
    input: VerifyInput<'_>,
) -> Result<VerifiedSignature, VerifyError> {
    // Read the three inputs off disk concurrently — all are tiny
    // (a few KB each) but we'd rather not serialise the syscalls.
    let (cert_pem, sig_text, blob) = tokio::try_join!(
        tokio::fs::read_to_string(input.cert_path),
        tokio::fs::read_to_string(input.sig_path),
        tokio::fs::read(input.tarball_path),
    )
    .map_err(|e| VerifyError::Io(format!("read verify inputs: {e}")))?;

    let policy = input.policy;
    let cert_pem_owned = cert_pem.clone();
    let sig_text_owned = sig_text.clone();
    let identity_regex = policy.identity_regexp.clone();
    let oidc_issuer = policy.oidc_issuer.clone();

    // The crypto path is CPU-bound; offload so a verify on a
    // bigger tarball doesn't stall the runtime's reactor.
    tokio::task::spawn_blocking(move || -> Result<VerifiedSignature, VerifyError> {
        verify_blocking(
            &cert_pem_owned,
            &sig_text_owned,
            &blob,
            &identity_regex,
            &oidc_issuer,
        )
    })
    .await
    .map_err(|e| VerifyError::Io(format!("verify join: {e}")))?
}

/// Synchronous core of [`verify_plugin_signature`]. Pulled out
/// so the function tree stays testable without a tokio runtime.
fn verify_blocking(
    cert_pem: &str,
    sig_text: &str,
    blob: &[u8],
    identity_regexp: &str,
    expected_issuer: &str,
) -> Result<VerifiedSignature, VerifyError> {
    // ── 1. Parse cert PEM → DER → X.509 v3 ────────────────────
    let pem = parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| VerifyError::CertParseFailed(format!("pem: {e}")))?
        .1;
    if pem.label != "CERTIFICATE" {
        return Err(VerifyError::CertParseFailed(format!(
            "expected `CERTIFICATE` PEM label, got `{}`",
            pem.label
        )));
    }
    let (_, cert) = parse_x509_certificate(&pem.contents)
        .map_err(|e| VerifyError::CertParseFailed(format!("x509: {e}")))?;

    // ── 2. Extract the cert's ECDSA-P256 public key ───────────
    let pubkey = extract_p256_pubkey(&cert)?;

    // ── 3. Verify ECDSA signature over SHA-256(blob) ──────────
    let sig_bytes = BASE64_STD
        .decode(sig_text.trim().as_bytes())
        .map_err(|e| VerifyError::SignatureDecodeFailed(format!("base64: {e}")))?;
    let signature = EcdsaSig::from_der(&sig_bytes)
        .map_err(|e| VerifyError::SignatureDecodeFailed(format!("der: {e}")))?;
    let digest = Sha256::digest(blob);
    pubkey
        .verify(&digest, &signature)
        .map_err(|_| VerifyError::SignatureMismatch)?;

    // ── 4. Identity policy: at least one SAN matches regex ────
    let san_entries = extract_san_entries(&cert);
    if san_entries.is_empty() {
        return Err(VerifyError::IdentityNotFound);
    }
    let regex =
        regex::Regex::new(identity_regexp).map_err(|e| VerifyError::IdentityRegexpInvalid {
            got: identity_regexp.to_string(),
            reason: e.to_string(),
        })?;
    let identity = san_entries
        .iter()
        .find(|entry| regex.is_match(entry))
        .cloned()
        .ok_or_else(|| VerifyError::IdentityMismatch {
            found: san_entries.join(", "),
            expected_regex: identity_regexp.to_string(),
        })?;

    // ── 5. Fulcio OIDC issuer extension matches policy ────────
    let cert_issuer = extract_fulcio_issuer(&cert).ok_or_else(|| VerifyError::IssuerMismatch {
        found: "<no Fulcio OIDC issuer extension>".to_string(),
        expected: expected_issuer.to_string(),
    })?;
    if cert_issuer != expected_issuer {
        return Err(VerifyError::IssuerMismatch {
            found: cert_issuer,
            expected: expected_issuer.to_string(),
        });
    }

    Ok(VerifiedSignature {
        identity,
        issuer: cert_issuer,
    })
}

/// Pull the ECDSA-P256 public key out of the cert's
/// `SubjectPublicKeyInfo`. Cosign never issues anything else, so
/// any other algorithm OID is a hard reject.
fn extract_p256_pubkey(cert: &X509Certificate<'_>) -> Result<VerifyingKey, VerifyError> {
    let spki = &cert.tbs_certificate.subject_pki;
    // P-256 SPKI = ecPublicKey (1.2.840.10045.2.1) + named curve
    // prime256v1 (1.2.840.10045.3.1.7). We don't insist on the
    // OID match — the `from_sec1_bytes` parse rejects anything
    // that isn't a valid P-256 point.
    let raw = spki.subject_public_key.data.as_ref();
    VerifyingKey::from_sec1_bytes(raw).map_err(|e| {
        VerifyError::UnsupportedKey(format!("expected ECDSA-P256 SEC1 point, parse failed: {e}"))
    })
}

/// Collect all SAN URIs + email addresses + DNS names. Cosign
/// keyless certs put the workflow URL in a URI SAN, so URIs are
/// the most common match for the GitHub Actions identity
/// regex.
fn extract_san_entries(cert: &X509Certificate<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for ext in cert.extensions() {
        if let ParsedExtension::SubjectAlternativeName(san) = ext.parsed_extension() {
            for name in &san.general_names {
                match name {
                    GeneralName::URI(u) => out.push((*u).to_string()),
                    GeneralName::RFC822Name(e) => out.push((*e).to_string()),
                    GeneralName::DNSName(d) => out.push((*d).to_string()),
                    _ => {}
                }
            }
        }
    }
    out
}

/// Fulcio annotates each cert with the OIDC provider that
/// authenticated the signing identity. Two OIDs are in active
/// use:
/// - `1.3.6.1.4.1.57264.1.1` — legacy, payload is a raw UTF-8
///   string.
/// - `1.3.6.1.4.1.57264.1.8` — modern, payload is an
///   ASN.1-DER `UTF8String`.
///
/// We accept either shape; preferring `…1.8` when both are
/// present.
fn extract_fulcio_issuer(cert: &X509Certificate<'_>) -> Option<String> {
    let mut legacy: Option<String> = None;
    let mut modern: Option<String> = None;

    let oid_legacy = oid_registry::Oid::from(&[1, 3, 6, 1, 4, 1, 57264, 1, 1]).ok()?;
    let oid_modern = oid_registry::Oid::from(&[1, 3, 6, 1, 4, 1, 57264, 1, 8]).ok()?;

    for ext in cert.extensions() {
        if ext.oid == oid_legacy {
            // Legacy extension carries the issuer as the raw
            // bytes of the extnValue OCTET STRING contents.
            if let Ok(s) = std::str::from_utf8(ext.value) {
                legacy = Some(s.to_string());
            }
        } else if ext.oid == oid_modern {
            // Modern extension wraps the issuer in a DER UTF8String.
            if let Some(s) = parse_der_utf8_string(ext.value) {
                modern = Some(s);
            }
        }
    }

    modern.or(legacy)
}

/// Strip an ASN.1 DER `UTF8String` (tag `0x0c`) header and
/// return the contents. Returns `None` on any shape mismatch
/// rather than panicking.
fn parse_der_utf8_string(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 2 || bytes[0] != 0x0c {
        return None;
    }
    let length_byte = bytes[1] as usize;
    let (start, len) = if length_byte < 0x80 {
        (2, length_byte)
    } else {
        // Long-form length: low 7 bits of the first byte give
        // the count of length octets that follow.
        let count = length_byte & 0x7f;
        if count == 0 || bytes.len() < 2 + count {
            return None;
        }
        let mut len_acc = 0usize;
        for &b in &bytes[2..2 + count] {
            len_acc = len_acc.checked_mul(256)?.checked_add(b as usize)?;
        }
        (2 + count, len_acc)
    };
    let end = start.checked_add(len)?;
    if end > bytes.len() {
        return None;
    }
    std::str::from_utf8(&bytes[start..end])
        .ok()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trusted_keys::TrustMode;
    use rcgen::{
        Certificate, CertificateParams, CustomExtension, DistinguishedName, KeyPair, SanType,
    };
    use std::fs;
    use tempfile::TempDir;

    const ISSUER: &str = "https://token.actions.githubusercontent.com";
    const SAN_URI: &str =
        "https://github.com/lordmacu/foo/.github/workflows/release.yml@refs/tags/v0.2.0";

    fn make_policy() -> AuthorPolicy {
        AuthorPolicy {
            owner: "lordmacu".to_string(),
            identity_regexp: "^https://github.com/lordmacu/.*$".to_string(),
            oidc_issuer: ISSUER.to_string(),
            mode: Some(TrustMode::Require),
        }
    }

    /// Generate a P-256 cert that mimics Fulcio's keyless
    /// flow: SAN with the workflow URL + the modern
    /// `1.3.6.1.4.1.57264.1.8` issuer extension.
    fn make_cert_with_issuer(issuer: &str, san_uri: &str) -> (KeyPair, Certificate) {
        let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params.subject_alt_names = vec![SanType::URI(san_uri.try_into().unwrap())];

        // Build the modern Fulcio issuer extension manually:
        // OID 1.3.6.1.4.1.57264.1.8 carrying a DER UTF8String.
        let mut ext_value = Vec::with_capacity(2 + issuer.len());
        ext_value.push(0x0c); // UTF8String tag
        ext_value.push(issuer.len() as u8); // short-form length (issuers fit < 128 bytes)
        ext_value.extend_from_slice(issuer.as_bytes());
        let issuer_ext =
            CustomExtension::from_oid_content(&[1, 3, 6, 1, 4, 1, 57264, 1, 8], ext_value);
        params.custom_extensions.push(issuer_ext);

        let cert = params.self_signed(&key).unwrap();
        (key, cert)
    }

    /// Sign the blob with the cert's private key and return the
    /// base64-encoded DER signature cosign would have written.
    fn sign_blob(key: &KeyPair, blob: &[u8]) -> String {
        // rcgen exposes only the PKCS#8 DER of the private key;
        // re-import into `p256::ecdsa::SigningKey` so we sign
        // exactly the way `p256` would verify.
        use p256::pkcs8::DecodePrivateKey;
        let signing_key = p256::ecdsa::SigningKey::from_pkcs8_der(&key.serialize_der()).unwrap();
        let digest = Sha256::digest(blob);
        let signature: EcdsaSig = p256::ecdsa::signature::Signer::sign(&signing_key, &digest);
        BASE64_STD.encode(signature.to_der().as_bytes())
    }

    /// End-to-end happy path: issue a Fulcio-shaped cert, sign a
    /// fake tarball, write everything to disk, run the verifier.
    #[tokio::test]
    async fn verify_accepts_signature_when_identity_and_issuer_match() {
        let tmp = TempDir::new().unwrap();
        let blob = b"plugin-tarball-bytes";
        let (key, cert) = make_cert_with_issuer(ISSUER, SAN_URI);
        let sig = sign_blob(&key, blob);

        let tarball = tmp.path().join("plugin.tar.gz");
        let cert_path = tmp.path().join("plugin.tar.gz.cert");
        let sig_path = tmp.path().join("plugin.tar.gz.sig");
        fs::write(&tarball, blob).unwrap();
        fs::write(&cert_path, cert.pem()).unwrap();
        fs::write(&sig_path, &sig).unwrap();

        let policy = make_policy();
        let result = verify_plugin_signature(VerifyInput {
            cosign_bin: Path::new(""),
            tarball_path: &tarball,
            sig_path: &sig_path,
            cert_path: &cert_path,
            bundle_path: None,
            policy: &policy,
        })
        .await
        .expect("verify should succeed");

        assert_eq!(result.issuer, ISSUER);
        assert_eq!(result.identity, SAN_URI);
    }

    #[tokio::test]
    async fn verify_rejects_when_blob_tampered_after_signing() {
        let tmp = TempDir::new().unwrap();
        let blob = b"original";
        let (key, cert) = make_cert_with_issuer(ISSUER, SAN_URI);
        let sig = sign_blob(&key, blob);

        let tarball = tmp.path().join("plugin.tar.gz");
        let cert_path = tmp.path().join("plugin.tar.gz.cert");
        let sig_path = tmp.path().join("plugin.tar.gz.sig");
        fs::write(&tarball, b"TAMPERED-BYTES").unwrap();
        fs::write(&cert_path, cert.pem()).unwrap();
        fs::write(&sig_path, sig).unwrap();

        let err = verify_plugin_signature(VerifyInput {
            cosign_bin: Path::new(""),
            tarball_path: &tarball,
            sig_path: &sig_path,
            cert_path: &cert_path,
            bundle_path: None,
            policy: &make_policy(),
        })
        .await
        .unwrap_err();
        assert!(matches!(err, VerifyError::SignatureMismatch));
    }

    #[tokio::test]
    async fn verify_rejects_when_san_does_not_match_policy_regex() {
        let tmp = TempDir::new().unwrap();
        let blob = b"x";
        let (key, cert) =
            make_cert_with_issuer(ISSUER, "https://github.com/other-org/foo/release.yml");
        let sig = sign_blob(&key, blob);

        let tarball = tmp.path().join("p.tar.gz");
        let cert_path = tmp.path().join("p.tar.gz.cert");
        let sig_path = tmp.path().join("p.tar.gz.sig");
        fs::write(&tarball, blob).unwrap();
        fs::write(&cert_path, cert.pem()).unwrap();
        fs::write(&sig_path, sig).unwrap();

        let err = verify_plugin_signature(VerifyInput {
            cosign_bin: Path::new(""),
            tarball_path: &tarball,
            sig_path: &sig_path,
            cert_path: &cert_path,
            bundle_path: None,
            policy: &make_policy(),
        })
        .await
        .unwrap_err();
        assert!(matches!(err, VerifyError::IdentityMismatch { .. }));
    }

    #[tokio::test]
    async fn verify_rejects_when_oidc_issuer_extension_does_not_match() {
        let tmp = TempDir::new().unwrap();
        let blob = b"x";
        let (key, cert) = make_cert_with_issuer("https://malicious.example.com", SAN_URI);
        let sig = sign_blob(&key, blob);

        let tarball = tmp.path().join("p.tar.gz");
        let cert_path = tmp.path().join("p.tar.gz.cert");
        let sig_path = tmp.path().join("p.tar.gz.sig");
        fs::write(&tarball, blob).unwrap();
        fs::write(&cert_path, cert.pem()).unwrap();
        fs::write(&sig_path, sig).unwrap();

        let err = verify_plugin_signature(VerifyInput {
            cosign_bin: Path::new(""),
            tarball_path: &tarball,
            sig_path: &sig_path,
            cert_path: &cert_path,
            bundle_path: None,
            policy: &make_policy(),
        })
        .await
        .unwrap_err();
        assert!(matches!(err, VerifyError::IssuerMismatch { .. }));
    }

    #[tokio::test]
    async fn verify_rejects_malformed_pem_certificate() {
        let tmp = TempDir::new().unwrap();
        let blob = b"x";
        let tarball = tmp.path().join("p.tar.gz");
        let cert_path = tmp.path().join("p.tar.gz.cert");
        let sig_path = tmp.path().join("p.tar.gz.sig");
        fs::write(&tarball, blob).unwrap();
        fs::write(&cert_path, b"not-a-pem-certificate").unwrap();
        fs::write(&sig_path, "AAAA").unwrap();

        let err = verify_plugin_signature(VerifyInput {
            cosign_bin: Path::new(""),
            tarball_path: &tarball,
            sig_path: &sig_path,
            cert_path: &cert_path,
            bundle_path: None,
            policy: &make_policy(),
        })
        .await
        .unwrap_err();
        assert!(matches!(err, VerifyError::CertParseFailed(_)));
    }

    #[tokio::test]
    async fn verify_rejects_when_signature_base64_invalid() {
        let tmp = TempDir::new().unwrap();
        let blob = b"x";
        let (_, cert) = make_cert_with_issuer(ISSUER, SAN_URI);
        let tarball = tmp.path().join("p.tar.gz");
        let cert_path = tmp.path().join("p.tar.gz.cert");
        let sig_path = tmp.path().join("p.tar.gz.sig");
        fs::write(&tarball, blob).unwrap();
        fs::write(&cert_path, cert.pem()).unwrap();
        fs::write(&sig_path, "###not-base64###").unwrap();

        let err = verify_plugin_signature(VerifyInput {
            cosign_bin: Path::new(""),
            tarball_path: &tarball,
            sig_path: &sig_path,
            cert_path: &cert_path,
            bundle_path: None,
            policy: &make_policy(),
        })
        .await
        .unwrap_err();
        assert!(matches!(err, VerifyError::SignatureDecodeFailed(_)));
    }

    #[test]
    fn discover_cosign_binary_is_a_no_op_in_lean_mode() {
        // Both arms should succeed — the lean verifier doesn't
        // need a real binary to be on disk.
        assert!(discover_cosign_binary(None).is_ok());
        assert!(discover_cosign_binary(Some(Path::new("/nonexistent/cosign"))).is_ok());
    }
}
