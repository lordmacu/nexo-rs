//! Phase 95 — EffectivePolicy::for_tool slice extractor tests.
//!
//! Verifies the slice shape sent across the JSON-RPC envelope.
//! `EffectiveBindingPolicy` itself is heavy to construct in tests
//! (requires a full `AgentConfig`); we rely on the unit tests
//! inside `effective.rs` to exercise the `resolve(...)` path and
//! cover the slice extractor here via a freshly-constructed
//! policy with default-built fields.

use nexo_core::agent::effective::WebSearchPolicy;

#[test]
fn web_search_policy_serialises_to_expected_json_shape() {
    let policy = WebSearchPolicy::default();
    let value = serde_json::to_value(&policy).expect("serialise default policy");
    // The `for_tool("web_search")` extractor passes this value
    // verbatim onto the `tool.invoke` envelope. Field names + types
    // must match what the subprocess plugin's policy parser expects.
    assert_eq!(value["enabled"], serde_json::json!(false));
    assert_eq!(value["provider"], serde_json::json!("auto"));
    assert_eq!(value["default_count"], serde_json::json!(5));
    assert_eq!(value["cache_ttl_secs"], serde_json::json!(600));
    assert_eq!(value["expand_default"], serde_json::json!(false));
}

#[test]
fn web_search_policy_round_trips_through_json_value() {
    // Operator-shape verification: parse a fully-populated policy
    // out of a literal JSON value matching what the subprocess
    // plugin would see in `ToolInvocation.policy`.
    let raw = serde_json::json!({
        "enabled": true,
        "provider": "brave",
        "default_count": 3,
        "cache_ttl_secs": 1800,
        "expand_default": true,
    });
    let parsed: WebSearchPolicy =
        serde_json::from_value(raw).expect("policy round-trip");
    assert!(parsed.enabled);
    assert_eq!(parsed.provider, "brave");
    assert_eq!(parsed.default_count, 3);
    assert_eq!(parsed.cache_ttl_secs, 1800);
    assert!(parsed.expand_default);
}
