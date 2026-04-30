//! Phase 12.8 — client-side events propagated from live MCP connections.
//!
//! MCP servers can push notifications like `notifications/tools/list_changed`
//! and `notifications/resources/list_changed`. `StdioMcpClient` and
//! `HttpMcpClient` emit these as `ClientEvent`s on a broadcast channel so
//! `SessionMcpRuntime` (and anything else that subscribes) can react —
//! typically by invalidating the tool catalog and re-registering.
//!
//! Phase 80.9.c — channel notifications carry user-visible content
//! that the gate + dispatcher need to inspect, so the variant
//! [`ClientEvent::ChannelMessage`] retains the params payload
//! verbatim. Existing variants stay payload-free.

use crate::channel::CHANNEL_NOTIFICATION_METHOD;
use crate::channel_permission::PERMISSION_RESPONSE_METHOD;

/// Events emitted by an active MCP client connection.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    /// Server sent `notifications/tools/list_changed`.
    ToolsListChanged,
    /// Server sent `notifications/resources/list_changed`.
    ResourcesListChanged,
    /// Server sent `notifications/nexo/channel`. Carries the raw
    /// `params` so the inbound loop can run
    /// [`crate::channel::parse_channel_notification`] without
    /// having to re-fetch the JSON-RPC frame. Phase 80.9.c.
    ChannelMessage { params: serde_json::Value },
    /// Server sent `notifications/nexo/channel/permission` —
    /// structured reply to a permission prompt the runtime
    /// emitted via
    /// [`crate::channel_permission::PERMISSION_REQUEST_METHOD`].
    /// Phase 80.9.b.
    ChannelPermissionResponse {
        params: serde_json::Value,
    },
    /// Any other notification method — forwarded verbatim for observers.
    Other(String),
}

/// Map a JSON-RPC notification `method` string to a `ClientEvent`.
/// Channel notifications go through [`channel_message_event`]
/// because they need params; this function is for payload-free
/// variants only — pass an empty `Value::Null` if you need a fallback.
pub fn method_to_event(method: &str) -> ClientEvent {
    match method {
        "notifications/tools/list_changed" => ClientEvent::ToolsListChanged,
        "notifications/resources/list_changed" => ClientEvent::ResourcesListChanged,
        m if m == CHANNEL_NOTIFICATION_METHOD || m == PERMISSION_RESPONSE_METHOD => {
            // Channel + permission notifications without params
            // shouldn't happen in well-behaved servers. Surface
            // them as `Other` so the inbound loop's typed
            // dispatch never sees a payload-less variant.
            ClientEvent::Other(m.to_string())
        }
        other => ClientEvent::Other(other.to_string()),
    }
}

/// Build a `ChannelMessage` event from raw params. The caller has
/// already classified the method as `CHANNEL_NOTIFICATION_METHOD`.
pub fn channel_message_event(params: serde_json::Value) -> ClientEvent {
    ClientEvent::ChannelMessage { params }
}

/// Build a `ChannelPermissionResponse` event from raw params.
/// Caller already classified the method as
/// `PERMISSION_RESPONSE_METHOD`. Phase 80.9.b.
pub fn channel_permission_response_event(params: serde_json::Value) -> ClientEvent {
    ClientEvent::ChannelPermissionResponse { params }
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

    #[test]
    fn channel_method_alone_falls_through_to_other() {
        // `method_to_event` is the payload-free path. Channel
        // notifications are dispatched via `channel_message_event`
        // so a stray classifier call shouldn't synthesise a
        // payload-less ChannelMessage.
        let e = method_to_event(CHANNEL_NOTIFICATION_METHOD);
        match e {
            ClientEvent::Other(s) => assert_eq!(s, CHANNEL_NOTIFICATION_METHOD),
            _ => panic!("expected Other"),
        }
    }

    #[test]
    fn channel_message_event_preserves_params() {
        let params = serde_json::json!({"content": "hi", "meta": {"chat_id": "C1"}});
        let e = channel_message_event(params.clone());
        match e {
            ClientEvent::ChannelMessage { params: got } => assert_eq!(got, params),
            _ => panic!("expected ChannelMessage"),
        }
    }

    #[test]
    fn permission_method_falls_through_to_other() {
        let e = method_to_event(PERMISSION_RESPONSE_METHOD);
        match e {
            ClientEvent::Other(s) => assert_eq!(s, PERMISSION_RESPONSE_METHOD),
            _ => panic!("expected Other"),
        }
    }

    #[test]
    fn channel_permission_response_event_preserves_params() {
        let params = serde_json::json!({"request_id": "abcde", "behavior": "allow"});
        let e = channel_permission_response_event(params.clone());
        match e {
            ClientEvent::ChannelPermissionResponse { params: got } => assert_eq!(got, params),
            _ => panic!("expected ChannelPermissionResponse"),
        }
    }
}
