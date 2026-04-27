//! Phase 76.1 — JSON-RPC parse defenses for HTTP request bodies.
//!
//! Stdio path keeps its existing parser (newline-delimited, one
//! object per line, lenient on bad lines because the peer is
//! cooperative). HTTP routes its bodies through this hardened
//! parser BEFORE the dispatcher sees them.
//!
//! Defenses, in order applied:
//!   1. UTF-8 well-formedness.
//!   2. Linear-scan depth check (no `serde_json` invoked yet) —
//!      reject before stack-overflow risk.
//!   3. `serde_json` parse to `Value`.
//!   4. Reject array roots (MCP 2025-11-25 forbids batches).
//!   5. Reject objects without `"jsonrpc": "2.0"` envelope.
//!   6. Reject `id` types other than string / number / null /
//!      missing.
//!   7. Cap on string fields known to be attacker-controlled
//!      (`method`, `params.name` if string).

use serde_json::Value;
use thiserror::Error;

/// Maximum allowed nesting depth for an HTTP request body.
/// `serde_json` does not enforce a depth limit on stable; a
/// pathological client can otherwise bury the parser deep enough
/// to blow the stack. Linear scan rejects before allocation.
pub(crate) const MAX_DEPTH: usize = 64;

/// Maximum allowed length, in bytes, for sensitive string fields
/// (`method`, `params.name`). Prevents log-line poisoning and
/// memory-pressure attacks via single huge identifiers.
pub(crate) const MAX_STRING_FIELD: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum JsonRpcParseError {
    #[error("body is not valid utf-8")]
    NotUtf8,
    #[error("body is not valid json: {0}")]
    InvalidJson(String),
    #[error("nesting depth exceeds {MAX_DEPTH}")]
    TooDeep,
    #[error("batch (array) requests are not supported")]
    BatchNotSupported,
    #[error("missing or invalid jsonrpc envelope")]
    InvalidEnvelope,
    #[error("invalid id (must be string, number, or null)")]
    InvalidId,
    #[error(
        "string field too long ({field}, {len} bytes, max {})",
        MAX_STRING_FIELD
    )]
    StringTooLong { field: &'static str, len: usize },
    #[error("empty body")]
    Empty,
}

impl JsonRpcParseError {
    /// JSON-RPC error code that maps best to this parse failure.
    /// Spec: -32700 parse error, -32600 invalid request.
    pub fn code(&self) -> i32 {
        match self {
            Self::NotUtf8 | Self::InvalidJson(_) | Self::Empty => -32700,
            Self::TooDeep
            | Self::BatchNotSupported
            | Self::InvalidEnvelope
            | Self::InvalidId
            | Self::StringTooLong { .. } => -32600,
        }
    }
}

/// Hardened JSON-RPC frame parser. Returns the validated
/// `serde_json::Value` (always an object on success) ready to be
/// fed into `Dispatcher::dispatch` after extracting `method` /
/// `params` / `id`.
pub fn parse_jsonrpc_frame(bytes: &[u8]) -> Result<Value, JsonRpcParseError> {
    if bytes.is_empty() || bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Err(JsonRpcParseError::Empty);
    }

    // 1. UTF-8.
    let text = std::str::from_utf8(bytes).map_err(|_| JsonRpcParseError::NotUtf8)?;

    // 2. Depth scan — reject pathological nesting before invoking
    //    serde_json (avoids stack-overflow risk).
    if exceeds_max_depth(text, MAX_DEPTH) {
        return Err(JsonRpcParseError::TooDeep);
    }

    // 3. Parse to Value.
    let value: Value =
        serde_json::from_str(text).map_err(|e| JsonRpcParseError::InvalidJson(e.to_string()))?;

    // 4. Batch reject.
    if value.is_array() {
        return Err(JsonRpcParseError::BatchNotSupported);
    }

    // 5. Envelope.
    let obj = value
        .as_object()
        .ok_or(JsonRpcParseError::InvalidEnvelope)?;
    match obj.get("jsonrpc").and_then(|v| v.as_str()) {
        Some("2.0") => {}
        _ => return Err(JsonRpcParseError::InvalidEnvelope),
    }

    // 6. id type (when present).
    if let Some(id) = obj.get("id") {
        if !(id.is_string() || id.is_number() || id.is_null()) {
            return Err(JsonRpcParseError::InvalidId);
        }
    }

    // 7. method length (when present — notifications and requests
    //    both carry a method).
    if let Some(method) = obj.get("method").and_then(|v| v.as_str()) {
        if method.len() > MAX_STRING_FIELD {
            return Err(JsonRpcParseError::StringTooLong {
                field: "method",
                len: method.len(),
            });
        }
    }
    // params.name (most-common attacker-controlled identifier).
    if let Some(name) = obj
        .get("params")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
    {
        if name.len() > MAX_STRING_FIELD {
            return Err(JsonRpcParseError::StringTooLong {
                field: "params.name",
                len: name.len(),
            });
        }
    }

    Ok(value)
}

/// Linear scan of `s` counting open-brace / open-bracket nesting,
/// honouring JSON string escapes so `"{{{"` literals do not
/// inflate depth. Returns true iff at any point the depth would
/// exceed `max`.
fn exceeds_max_depth(s: &str, max: usize) -> bool {
    let bytes = s.as_bytes();
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escape = false;
    for &b in bytes {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > max {
                    return true;
                }
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_request() {
        let body = br#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#;
        let v = parse_jsonrpc_frame(body).unwrap();
        assert_eq!(v["method"], "tools/list");
    }

    #[test]
    fn parses_valid_notification() {
        let body = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        parse_jsonrpc_frame(body).unwrap();
    }

    #[test]
    fn rejects_batch_array() {
        let body = br#"[{"jsonrpc":"2.0","method":"x","id":1}]"#;
        let err = parse_jsonrpc_frame(body).unwrap_err();
        assert!(matches!(err, JsonRpcParseError::BatchNotSupported));
    }

    #[test]
    fn rejects_deep_nesting() {
        // 100 nested objects; should fail before serde_json sees it.
        let mut body = String::new();
        for _ in 0..100 {
            body.push('{');
            body.push_str("\"a\":");
        }
        body.push_str("null");
        for _ in 0..100 {
            body.push('}');
        }
        let err = parse_jsonrpc_frame(body.as_bytes()).unwrap_err();
        assert!(matches!(err, JsonRpcParseError::TooDeep));
    }

    #[test]
    fn does_not_count_braces_inside_strings() {
        // 70 literal `{` inside a string — single-depth object.
        let mut deep_str = String::from(r#"{"jsonrpc":"2.0","method":"x","params":{"s":""#);
        for _ in 0..70 {
            deep_str.push('{');
        }
        deep_str.push_str("\"},\"id\":1}");
        let v = parse_jsonrpc_frame(deep_str.as_bytes()).unwrap();
        assert_eq!(v["method"], "x");
    }

    #[test]
    fn handles_escaped_quotes_in_strings() {
        // Escaped backslash before a quote keeps us inside the
        // string, so nested-object characters do not count.
        let body = br#"{"jsonrpc":"2.0","method":"x","params":{"s":"\\\"{{{{{{{"},"id":1}"#;
        let v = parse_jsonrpc_frame(body).unwrap();
        assert_eq!(v["method"], "x");
    }

    #[test]
    fn rejects_missing_jsonrpc() {
        let body = br#"{"method":"x","id":1}"#;
        let err = parse_jsonrpc_frame(body).unwrap_err();
        assert!(matches!(err, JsonRpcParseError::InvalidEnvelope));
    }

    #[test]
    fn rejects_wrong_jsonrpc_version() {
        let body = br#"{"jsonrpc":"1.0","method":"x","id":1}"#;
        let err = parse_jsonrpc_frame(body).unwrap_err();
        assert!(matches!(err, JsonRpcParseError::InvalidEnvelope));
    }

    #[test]
    fn rejects_object_id() {
        let body = br#"{"jsonrpc":"2.0","method":"x","id":{"x":1}}"#;
        let err = parse_jsonrpc_frame(body).unwrap_err();
        assert!(matches!(err, JsonRpcParseError::InvalidId));
    }

    #[test]
    fn accepts_null_id() {
        let body = br#"{"jsonrpc":"2.0","method":"x","id":null}"#;
        parse_jsonrpc_frame(body).unwrap();
    }

    #[test]
    fn rejects_method_too_long() {
        let huge = "a".repeat(MAX_STRING_FIELD + 1);
        let body = format!(r#"{{"jsonrpc":"2.0","method":"{}","id":1}}"#, huge);
        let err = parse_jsonrpc_frame(body.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            JsonRpcParseError::StringTooLong {
                field: "method",
                ..
            }
        ));
    }

    #[test]
    fn rejects_empty_body() {
        let err = parse_jsonrpc_frame(b"").unwrap_err();
        assert!(matches!(err, JsonRpcParseError::Empty));
        let err = parse_jsonrpc_frame(b"   \n\t").unwrap_err();
        assert!(matches!(err, JsonRpcParseError::Empty));
    }

    #[test]
    fn rejects_non_utf8() {
        // Lone continuation byte — invalid UTF-8.
        let body = b"\xff\xfe";
        let err = parse_jsonrpc_frame(body).unwrap_err();
        assert!(matches!(err, JsonRpcParseError::NotUtf8));
    }

    #[test]
    fn rejects_invalid_json() {
        let body = br#"{not json}"#;
        let err = parse_jsonrpc_frame(body).unwrap_err();
        assert!(matches!(err, JsonRpcParseError::InvalidJson(_)));
    }

    #[test]
    fn parse_error_codes_match_spec() {
        assert_eq!(JsonRpcParseError::Empty.code(), -32700);
        assert_eq!(JsonRpcParseError::NotUtf8.code(), -32700);
        assert_eq!(JsonRpcParseError::InvalidJson("x".into()).code(), -32700);
        assert_eq!(JsonRpcParseError::TooDeep.code(), -32600);
        assert_eq!(JsonRpcParseError::BatchNotSupported.code(), -32600);
        assert_eq!(JsonRpcParseError::InvalidEnvelope.code(), -32600);
        assert_eq!(JsonRpcParseError::InvalidId.code(), -32600);
        assert_eq!(
            JsonRpcParseError::StringTooLong { field: "x", len: 1 }.code(),
            -32600
        );
    }
}
