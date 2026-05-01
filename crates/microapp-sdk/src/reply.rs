//! `ToolReply` — typed wire shape for the JSON-RPC `result`
//! field returned by tool handlers.

use serde_json::{json, Value};

/// JSON value returned by a tool handler. Wraps `serde_json::Value`
/// with ergonomic constructors for the most common shapes.
///
/// Wire-shape struct — caller-built; field additions are
/// deliberate semver-major. Intentionally **not**
/// `#[non_exhaustive]`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolReply {
    /// The JSON value the SDK serialises into the JSON-RPC `result`
    /// field. Public so handlers can build custom shapes when the
    /// constructors below don't fit.
    pub value: Value,
}

impl ToolReply {
    /// Plain text reply: `{ "ok": true, "text": "..." }`.
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            value: json!({ "ok": true, "text": text.into() }),
        }
    }

    /// Arbitrary JSON reply — caller's `value` is returned as-is.
    pub fn ok_json(value: Value) -> Self {
        Self { value }
    }

    /// Empty reply: `null`. Useful for fire-and-forget tools.
    pub fn empty() -> Self {
        Self { value: Value::Null }
    }

    /// Borrow the inner `serde_json::Value`.
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    /// Consume into the inner `serde_json::Value`.
    pub fn into_value(self) -> Value {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_constructs_ok_text_shape() {
        let r = ToolReply::ok("hello");
        assert_eq!(r.value["ok"], true);
        assert_eq!(r.value["text"], "hello");
    }

    #[test]
    fn ok_json_passes_value_through() {
        let r = ToolReply::ok_json(json!({"a": 1, "b": "two"}));
        assert_eq!(r.value["a"], 1);
        assert_eq!(r.value["b"], "two");
    }

    #[test]
    fn empty_returns_null() {
        let r = ToolReply::empty();
        assert!(r.value.is_null());
    }

    #[test]
    fn as_value_and_into_value_round_trip() {
        let r = ToolReply::ok("x");
        let snapshot = r.value.clone();
        assert_eq!(r.as_value(), &snapshot);
        assert_eq!(r.into_value(), snapshot);
    }
}
