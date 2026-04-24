//! Phase 12.8 — client-side events propagated from live MCP connections.
//!
//! MCP servers can push notifications like `notifications/tools/list_changed`
//! and `notifications/resources/list_changed`. `StdioMcpClient` and
//! `HttpMcpClient` emit these as `ClientEvent`s on a broadcast channel so
//! `SessionMcpRuntime` (and anything else that subscribes) can react —
//! typically by invalidating the tool catalog and re-registering.

/// Events emitted by an active MCP client connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    /// Server sent `notifications/tools/list_changed`.
    ToolsListChanged,
    /// Server sent `notifications/resources/list_changed`.
    ResourcesListChanged,
    /// Any other notification method — forwarded verbatim for observers.
    Other(String),
}

/// Map a JSON-RPC notification `method` string to a `ClientEvent`. Returns
/// `None` when the string is not a notification (doesn't start with
/// `notifications/`) — the caller already had to classify it as a notif.
pub fn method_to_event(method: &str) -> ClientEvent {
    match method {
        "notifications/tools/list_changed" => ClientEvent::ToolsListChanged,
        "notifications/resources/list_changed" => ClientEvent::ResourcesListChanged,
        other => ClientEvent::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_tools_list_changed() {
        assert_eq!(
            method_to_event("notifications/tools/list_changed"),
            ClientEvent::ToolsListChanged
        );
    }

    #[test]
    fn maps_resources_list_changed() {
        assert_eq!(
            method_to_event("notifications/resources/list_changed"),
            ClientEvent::ResourcesListChanged
        );
    }

    #[test]
    fn unknown_method_is_other() {
        let e = method_to_event("notifications/progress");
        match e {
            ClientEvent::Other(s) => assert_eq!(s, "notifications/progress"),
            _ => panic!("expected Other"),
        }
    }
}
