//! Phase 12.8 — MCP `notifications/message` capability.
//!
//! Per MCP spec 2024-11-05, servers may push log messages at runtime
//! with a level (RFC 5424 subset) plus optional `logger` component and
//! arbitrary JSON `data`. We parse the payload and forward it through
//! `tracing` with an appropriate level; logs flow into whatever
//! subscriber the agent has configured.

use serde_json::Value;

/// Parse a `notifications/message` params object and emit it through the
/// `tracing` subscriber. Best-effort: missing/invalid fields degrade to
/// info-level with raw content rather than dropping the message.
pub fn emit_log_notification(mcp_name: &str, params: &Value) {
    let level = params
        .get("level")
        .and_then(|v| v.as_str())
        .unwrap_or("info");
    let logger = params.get("logger").and_then(|v| v.as_str());
    let data = match params.get("data") {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    };

    match level {
        "debug" => tracing::debug!(
            mcp = %mcp_name,
            mcp_logger = ?logger,
            data = %data,
            "mcp log"
        ),
        "info" | "notice" => tracing::info!(
            mcp = %mcp_name,
            mcp_logger = ?logger,
            data = %data,
            "mcp log"
        ),
        "warning" => tracing::warn!(
            mcp = %mcp_name,
            mcp_logger = ?logger,
            data = %data,
            "mcp log"
        ),
        "error" | "critical" | "alert" | "emergency" => tracing::error!(
            mcp = %mcp_name,
            mcp_logger = ?logger,
            data = %data,
            "mcp log"
        ),
        other => tracing::info!(
            mcp = %mcp_name,
            mcp_logger = ?logger,
            level = %other,
            data = %data,
            "mcp log (unknown level)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // `emit_log_notification` is fire-and-forget; tests assert it doesn't
    // panic on edge cases. Subscriber capture is covered by the
    // integration test in `tests/mock_server_test.rs`.

    #[test]
    fn handles_each_level_without_panicking() {
        for level in [
            "debug",
            "info",
            "notice",
            "warning",
            "error",
            "critical",
            "alert",
            "emergency",
            "unknown-level",
        ] {
            emit_log_notification("mock", &json!({ "level": level, "data": "hello" }));
        }
    }

    #[test]
    fn handles_missing_data_without_panicking() {
        emit_log_notification("mock", &json!({ "level": "info" }));
    }

    #[test]
    fn handles_structured_data() {
        emit_log_notification(
            "mock",
            &json!({
                "level": "warning",
                "logger": "db",
                "data": { "query": "SELECT 1", "rows": 3 }
            }),
        );
    }

    #[test]
    fn handles_missing_level_defaults_to_info() {
        emit_log_notification("mock", &json!({ "data": "no level" }));
    }
}
