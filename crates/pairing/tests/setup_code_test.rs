use std::time::Duration;

use nexo_pairing::setup_code::{decode_setup_code, encode_setup_code};
use nexo_pairing::SetupCodeIssuer;

fn issuer() -> (SetupCodeIssuer, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pairing.key");
    let issuer = SetupCodeIssuer::open_or_create(&path).unwrap();
    (issuer, dir)
}

#[test]
fn issue_then_verify_round_trip() {
    let (issuer, _d) = issuer();
    let code = issuer
        .issue("wss://example.com", "companion-v1", Duration::from_secs(600), Some("phone-7"))
        .unwrap();
    let claims = issuer.verify(&code.bootstrap_token).unwrap();
    assert_eq!(claims.profile, "companion-v1");
    assert_eq!(claims.device_label.as_deref(), Some("phone-7"));
}

#[test]
fn tampered_claims_rejected() {
    let (issuer, _d) = issuer();
    let code = issuer
        .issue("wss://example.com", "p", Duration::from_secs(600), None)
        .unwrap();
    // Flip a byte in the token claims segment.
    let mut bytes: Vec<u8> = code.bootstrap_token.into_bytes();
    bytes[0] ^= 0x01;
    let tampered = String::from_utf8(bytes).unwrap();
    let err = issuer.verify(&tampered).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("invalid signature") || msg.contains("invalid"),
        "got {msg}"
    );
}

#[test]
fn expired_token_rejected() {
    let (issuer, _d) = issuer();
    // Issue with TTL 1ns then sleep past expiry. (chrono is fine with sub-second.)
    let code = issuer
        .issue("wss://example.com", "p", Duration::from_nanos(1), None)
        .unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let err = issuer.verify(&code.bootstrap_token).unwrap_err();
    assert!(err.to_string().contains("expired"));
}

#[test]
fn second_issuer_with_different_secret_rejects() {
    let (issuer_a, _da) = issuer();
    let (issuer_b, _db) = issuer();
    let code = issuer_a
        .issue("wss://x", "p", Duration::from_secs(60), None)
        .unwrap();
    let err = issuer_b.verify(&code.bootstrap_token).unwrap_err();
    assert!(err.to_string().contains("invalid signature"));
}

#[test]
fn encode_decode_setup_code_round_trip() {
    let (issuer, _d) = issuer();
    let code = issuer
        .issue("wss://example.com", "p", Duration::from_secs(60), None)
        .unwrap();
    let encoded = encode_setup_code(&code).unwrap();
    let decoded = decode_setup_code(&encoded).unwrap();
    assert_eq!(decoded.url, code.url);
    assert_eq!(decoded.bootstrap_token, code.bootstrap_token);
}

#[cfg(unix)]
#[test]
fn generated_secret_has_0600_perms() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pairing.key");
    let _ = SetupCodeIssuer::open_or_create(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "expected 0600, got {:o}", mode);
}
