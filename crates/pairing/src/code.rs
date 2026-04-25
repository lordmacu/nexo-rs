//! Human-friendly pairing code generator.
//!
//! 8 chars from `ABCDEFGHJKLMNPQRSTUVWXYZ23456789` (no `0/O/1/I/L`),
//! retried up to 500 times if the generator collides with an
//! already-active code in the store.

use rand::Rng;
use std::collections::HashSet;

pub const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const LENGTH: usize = 8;
const MAX_ATTEMPTS: usize = 500;

pub fn random() -> String {
    let mut rng = rand::thread_rng();
    (0..LENGTH)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// Generate a code that does not collide with any entry in `existing`.
/// Returns `Err` after `MAX_ATTEMPTS` collisions — caller should
/// surface that as a 5xx-equivalent: the keyspace is large
/// (32^8 ≈ 10^12), so 500 collisions in a row means something else
/// is wrong (e.g. RNG broken).
pub fn generate_unique(existing: &HashSet<String>) -> Result<String, &'static str> {
    for _ in 0..MAX_ATTEMPTS {
        let candidate = random();
        if !existing.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Err("failed to generate unique pairing code after 500 attempts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alphabet_excludes_ambiguous_chars() {
        // Mirrors OpenClaw's choice (research/src/pairing/pairing-store.ts:32).
        // 0/O/1/I dropped; L stays — empirically distinguishable in
        // the fixed-width fonts the operator sees.
        for c in [b'0', b'O', b'1', b'I'] {
            assert!(!ALPHABET.contains(&c), "ambiguous char {} in alphabet", c as char);
        }
    }

    #[test]
    fn generated_code_uses_only_alphabet() {
        for _ in 0..50 {
            let c = random();
            assert_eq!(c.len(), LENGTH);
            assert!(c.chars().all(|ch| ALPHABET.contains(&(ch as u8))));
        }
    }

    #[test]
    fn generate_unique_avoids_collision() {
        let mut existing = HashSet::new();
        for _ in 0..200 {
            let c = generate_unique(&existing).unwrap();
            assert!(!existing.contains(&c));
            existing.insert(c);
        }
    }
}
