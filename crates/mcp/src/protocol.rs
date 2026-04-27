//! MCP wire helpers on top of `nexo_extensions::runtime::wire` (JSON-RPC 2.0
//! over line-delimited stdio). MCP adds notifications (no `id`) and a fixed
//! protocol version string.

use serde::Serialize;

pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// MCP protocol versions the server has been verified against.
/// When a client sends one of these in `initialize.protocolVersion`
/// we echo it back so the client moves forward at its preferred
/// version. Anything else falls back to [`PROTOCOL_VERSION`].
///
/// 2024-11-05 — original release that this crate was first wired
///              against; still used by older clients (Anthropic
///              SDK helpers, mock fixtures, internal probes).
/// 2025-03-26 — interim revision that some Claude desktop builds
///              still negotiate against; behaviourally identical
///              for our subset (initialize → tools/list →
///              tools/call).
/// 2025-06-18 — pre-release used briefly by Claude Code 2.0.x;
///              listed for parity, no behaviour difference.
/// 2025-11-25 — version Claude Code 2.1+ negotiates. Phase 73
///              exposed the bug where echoing 2024-11-05 to a
///              2025-11-25 client made Claude register the
///              server but silently drop every tool from its
///              permission registry, surfacing as "Available MCP
///              tools: none" on the first permission check.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// `true` when `v` is one of the protocol versions the server has
/// been verified to honour.
pub fn is_supported_protocol_version(v: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&v)
}

/// Reserved id for the `initialize` request. Regular calls start at 1.
pub const INITIALIZE_ID: u64 = 0;

/// Encode a JSON-RPC 2.0 request (has an `id`). Delegates to the shared
/// extensions wire module so framing stays identical across crates.
pub fn encode_request<P: Serialize>(
    method: &str,
    params: P,
    id: u64,
) -> Result<String, serde_json::Error> {
    nexo_extensions::runtime::wire::encode(method, params, id)
}

/// Encode a JSON-RPC 2.0 notification (no `id`). MCP uses this for
/// `notifications/initialized`, `notifications/cancelled`, and server→client
/// pushes like `notifications/tools/list_changed`.
pub fn encode_notification<P: Serialize>(
    method: &str,
    params: P,
) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct Notification<'a, P: Serialize> {
        jsonrpc: &'static str,
        method: &'a str,
        params: P,
    }
    let n = Notification {
        jsonrpc: "2.0",
        method,
        params,
    };
    let mut s = serde_json::to_string(&n)?;
    s.push('\n');
    Ok(s)
}

/// Classify a raw JSON-RPC 2.0 message as a notification. Returns the
/// `method` string when the value has `"method"` set and no `"id"` field;
/// otherwise `None` (requests carry id, responses carry id+result/error).
pub fn parse_notification_method(raw: &serde_json::Value) -> Option<String> {
    if raw.get("id").is_some() {
        return None;
    }
    raw.get("method")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
}

/// Initialize request params as defined by the MCP spec (2024-11-05).
#[derive(Debug, Serialize)]
pub(crate) struct InitializeParams<'a> {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    pub capabilities: serde_json::Value,
    #[serde(rename = "clientInfo")]
    pub client_info: &'a crate::types::McpClientInfo,
}

impl<'a> InitializeParams<'a> {
    pub fn new(client_info: &'a crate::types::McpClientInfo) -> Self {
        Self::with_sampling(client_info, false)
    }

    /// Build initialize params declaring the `sampling` capability when
    /// `sampling=true`. Servers that don't support sampling can still
    /// talk to us; we just won't advertise it when the host has no
    /// `SamplingProvider` configured.
    pub fn with_sampling(client_info: &'a crate::types::McpClientInfo, sampling: bool) -> Self {
        let capabilities = if sampling {
            serde_json::json!({"tools": {}, "sampling": {}})
        } else {
            serde_json::json!({"tools": {}})
        };
        Self {
            protocol_version: PROTOCOL_VERSION,
            capabilities,
            client_info,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_has_id() {
        let s = encode_request("tools/list", serde_json::json!({}), 7).unwrap();
        assert!(s.contains("\"id\":7"));
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn notification_has_no_id() {
        let s = encode_notification("notifications/initialized", serde_json::json!({})).unwrap();
        assert!(!s.contains("\"id\""));
        assert!(s.contains("\"method\":\"notifications/initialized\""));
    }

    #[test]
    fn notification_is_detected() {
        let v: serde_json::Value = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/tools/list_changed"
        });
        assert_eq!(
            parse_notification_method(&v).as_deref(),
            Some("notifications/tools/list_changed")
        );
    }

    #[test]
    fn response_is_not_notification() {
        let v: serde_json::Value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {}
        });
        assert!(parse_notification_method(&v).is_none());
    }

    #[test]
    fn request_is_not_notification() {
        let v: serde_json::Value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        });
        assert!(parse_notification_method(&v).is_none());
    }

    #[test]
    fn invalid_returns_none() {
        let v: serde_json::Value = serde_json::json!({"jsonrpc": "2.0"});
        assert!(parse_notification_method(&v).is_none());
    }

    #[test]
    fn initialize_params_shape() {
        let ci = crate::types::McpClientInfo {
            name: "agent".into(),
            version: "0.1.0".into(),
        };
        let p = InitializeParams::new(&ci);
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["protocolVersion"], PROTOCOL_VERSION);
        assert!(v["capabilities"]["tools"].is_object());
        assert_eq!(v["clientInfo"]["name"], "agent");
    }
}
