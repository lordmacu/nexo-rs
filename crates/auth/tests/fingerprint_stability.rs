//! Step 22: fingerprint stability across boots. `Fingerprint::of` is
//! pinned to `sha256(account_id)[..8]` — two `Fingerprint` values for
//! the same account id must always compare equal, and a known-value
//! vector prevents an accidental algorithm switch from silently
//! invalidating existing log / metric tags.

use agent_auth::handle::Fingerprint;

#[test]
fn same_input_produces_same_fingerprint() {
    let a = Fingerprint::of("ana@gmail.com");
    let b = Fingerprint::of("ana@gmail.com");
    assert_eq!(a, b);
    assert_eq!(a.to_hex(), b.to_hex());
}

#[test]
fn fingerprint_of_known_input_is_pinned() {
    // sha256("+573001234567")[..8] — pin this vector. Updating the
    // algorithm requires updating existing log / metric tags too, so
    // make that an explicit decision.
    let fp = Fingerprint::of("+573001234567");
    let expected = "5b181b132cc2c1b4";
    assert_eq!(fp.to_hex(), expected, "fingerprint algorithm drifted");
}

#[test]
fn no_collisions_in_1000_random_ids() {
    use std::collections::HashSet;
    let mut seen: HashSet<[u8; 8]> = HashSet::new();
    for i in 0..1_000u32 {
        let id = format!("account-{i:08x}@example.com");
        let fp = Fingerprint::of(&id);
        assert!(
            seen.insert(*fp.as_bytes()),
            "unexpected fingerprint collision at i={i}"
        );
    }
}

#[test]
fn all_hex_lowercase() {
    let fp = Fingerprint::of("ana");
    let hex = fp.to_hex();
    assert_eq!(hex.len(), 16);
    assert!(hex.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
}
