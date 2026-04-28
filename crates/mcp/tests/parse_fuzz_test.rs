//! Phase 76.12 — property-based fuzz over `parse_jsonrpc_frame`.
//!
//! Invariant: for ANY byte sequence, `parse_jsonrpc_frame` never
//! panics. It may return `Ok(Value)` or any `Err(JsonRpcParseError)`
//! variant — the caller is responsible for error handling, not the
//! parser. No panic = no crash gate for adversarial HTTP clients.
//!
//! Gated behind `--features server-conformance` so normal
//! `cargo test -p nexo-mcp` stays fast.

#![cfg(feature = "server-conformance")]

use nexo_mcp::server::parse::parse_jsonrpc_frame;
use proptest::prelude::*;

// 1. Completely arbitrary bytes — covers invalid UTF-8, NUL bytes,
//    truncated frames, enormous inputs.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]
    #[test]
    fn arbitrary_bytes_no_panic(input in any::<Vec<u8>>()) {
        let _ = parse_jsonrpc_frame(&input);
    }
}

// 2. Arbitrary UTF-8 strings — valid encoding, arbitrary content.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]
    #[test]
    fn arbitrary_string_no_panic(s in any::<String>()) {
        let _ = parse_jsonrpc_frame(s.as_bytes());
    }
}

// 3. Valid JSON envelope with arbitrary method string — exercises
//    the method-length cap and any edge in the string-field check.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]
    #[test]
    fn valid_envelope_arbitrary_method_no_panic(
        method in any::<String>(),
        id in 0i64..=i64::MAX,
    ) {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "id": id
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let _ = parse_jsonrpc_frame(&bytes);
    }
}

// 4. Deeply nested objects — from 1 to 200 levels. The depth guard
//    (MAX_DEPTH = 64) must reject without panicking or stack-
//    overflowing at any depth.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]
    #[test]
    fn depth_sweep_no_panic(depth in 1usize..=200) {
        let mut s = String::new();
        for _ in 0..depth {
            s.push_str("{\"a\":");
        }
        s.push_str("null");
        for _ in 0..depth {
            s.push('}');
        }
        let _ = parse_jsonrpc_frame(s.as_bytes());
    }
}

// 5. Array roots (batch) — must return `BatchNotSupported` without
//    panicking regardless of array contents.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]
    #[test]
    fn batch_arrays_no_panic(
        len in 0usize..=10,
        method in "[a-z/]{1,20}",
    ) {
        // Build a JSON array of `len` valid request objects.
        let items: Vec<serde_json::Value> = (0..len)
            .map(|i| serde_json::json!({"jsonrpc":"2.0","method":method,"id":i}))
            .collect();
        let bytes = serde_json::to_vec(&items).unwrap();
        let result = parse_jsonrpc_frame(&bytes);
        if len == 0 {
            // `[]` — not a batch per se but an array root; still
            // BatchNotSupported or InvalidEnvelope.
            assert!(result.is_err(), "array root must be rejected");
        } else {
            assert!(result.is_err(), "batch must be rejected");
        }
        // The important thing is that we got here without panicking.
    }
}
