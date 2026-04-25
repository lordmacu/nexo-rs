//! Cache key for AllowSession decisions. Lets the bin skip the
//! decider on a repeat call with the same `(tool_name, input)` pair.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::Value;

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct SessionCacheKey {
    pub tool_name: String,
    pub input_fingerprint: u64,
}

impl SessionCacheKey {
    pub fn from_request(tool_name: &str, input: &Value) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            input_fingerprint: fingerprint(input),
        }
    }
}

/// Cheap, stable-enough hash of a JSON value. Uses `to_string` (which
/// preserves serde_json's deterministic key order on read) into
/// `DefaultHasher`. Good enough for the in-process cache; the real
/// risk is a key-reorder LLM, which we accept as a documented
/// limitation rather than canonicalising eagerly.
pub fn fingerprint(value: &Value) -> u64 {
    let s = value.to_string();
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fingerprint_is_stable_across_calls() {
        let v = json!({"a": 1, "b": [1, 2]});
        let a = fingerprint(&v);
        let b = fingerprint(&v);
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_differs_for_distinct_inputs() {
        let a = fingerprint(&json!({"file": "a.rs"}));
        let b = fingerprint(&json!({"file": "b.rs"}));
        assert_ne!(a, b);
    }

    #[test]
    fn cache_key_combines_tool_and_input() {
        let k1 = SessionCacheKey::from_request("Edit", &json!({"x": 1}));
        let k2 = SessionCacheKey::from_request("Edit", &json!({"x": 1}));
        let k3 = SessionCacheKey::from_request("Bash", &json!({"x": 1}));
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }
}
