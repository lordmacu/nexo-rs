//! Phase 83.8.4.a — outbound channel dispatch surface for admin
//! RPC handlers.
//!
//! `processing::intervention` (Phase 82.13) and any future admin
//! handler that needs to send a message *out* through a channel
//! plugin (whatsapp / telegram / email / future) does so against
//! [`ChannelOutboundDispatcher`]. Production wiring lives in
//! `nexo-setup`'s `admin_adapters.rs` and bridges the trait to
//! each plugin's existing send path; tests inject mocks.
//!
//! The trait is intentionally agnostic over channel kind — the
//! caller passes the channel name (`"whatsapp"`, `"telegram"`,
//! …) so this surface stays usable from microapps targeting any
//! channel. `Phase 82.3` outbound dispatch from extensions
//! addresses the *inbound side of microapp authoring*; this
//! addresses the *operator-driven outbound side*.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

/// Operator-driven outbound message. Passed to a channel-plugin
/// adapter that knows how to translate into its native send call.
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    /// Channel plugin id (`"whatsapp"`, `"telegram"`, `"email"`, …).
    pub channel: String,
    /// Channel-side account id (the sender — for multi-tenant
    /// daemons each agent typically pins to one account per
    /// channel).
    pub account_id: String,
    /// Recipient — channel-native contact id (phone number, chat
    /// id, email address, …).
    pub to: String,
    /// Body text or template payload.
    pub body: String,
    /// `"text"`, `"template"`, `"media"`, future. Plugins surface
    /// `-32601` from the dispatcher when they cannot honour a
    /// kind they do not implement.
    pub msg_kind: String,
    /// Optional channel-specific attachments (media URLs, file
    /// blobs, template variables).
    pub attachments: Vec<Value>,
    /// Reply-to message id when the channel supports threaded
    /// replies. `None` produces a fresh message.
    pub reply_to_msg_id: Option<String>,
}

/// Acknowledgement returned to the admin handler. Used to populate
/// the audit log + the SDK-side `ProcessingIntervenAck`.
#[derive(Debug, Clone, Default)]
pub struct OutboundAck {
    /// Provider-side message id when the channel returned one.
    /// Always `None` for fire-and-forget channels.
    pub outbound_message_id: Option<String>,
}

/// Errors the dispatcher returns. `MethodNotFound`-style when the
/// caller asks for a channel name no adapter knows about, fail
/// when the plugin itself rejects the payload, transport when the
/// underlying I/O fails.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ChannelOutboundError {
    /// No production adapter is registered for this channel name.
    /// Maps to `-32004 channel_unavailable` at the admin RPC
    /// layer.
    #[error("channel_unavailable: no adapter registered for `{0}`")]
    ChannelUnavailable(String),
    /// Plugin rejected the payload — bad recipient, unsupported
    /// `msg_kind`, etc. Maps to `-32602 invalid_params`.
    #[error("invalid_params: {0}")]
    InvalidParams(String),
    /// Transport failure (timeout, network, plugin crash). Maps
    /// to `-32603 internal`.
    #[error("transport: {0}")]
    Transport(String),
}

/// Trait the admin RPC dispatcher consults when an operator action
/// requires sending a message out through a channel plugin. v0
/// callers: [`crate::agent::admin_rpc::domains::processing::intervention`]
/// — when `InterventionAction::Reply` is dispatched.
///
/// Implementors are typically `Send + Sync + 'static` Arc-wrapped
/// adapters held inside `nexo-setup` that route by `channel` to
/// the matching plugin's send entry point.
#[async_trait]
pub trait ChannelOutboundDispatcher: Send + Sync + std::fmt::Debug {
    /// Send one message. Best-effort delivery — the dispatcher
    /// MAY block until the plugin returns an acknowledgement, but
    /// MUST NOT hold the caller for longer than the channel-side
    /// transport timeout (plugins guard their own).
    async fn send(&self, msg: OutboundMessage) -> Result<OutboundAck, ChannelOutboundError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct CapturingDispatcher {
        pub sent: Mutex<Vec<OutboundMessage>>,
        pub respond_with_id: Option<String>,
    }

    #[async_trait]
    impl ChannelOutboundDispatcher for CapturingDispatcher {
        async fn send(
            &self,
            msg: OutboundMessage,
        ) -> Result<OutboundAck, ChannelOutboundError> {
            self.sent.lock().unwrap().push(msg);
            Ok(OutboundAck {
                outbound_message_id: self.respond_with_id.clone(),
            })
        }
    }

    #[derive(Debug, Default)]
    struct AlwaysFailingDispatcher;

    #[async_trait]
    impl ChannelOutboundDispatcher for AlwaysFailingDispatcher {
        async fn send(
            &self,
            _msg: OutboundMessage,
        ) -> Result<OutboundAck, ChannelOutboundError> {
            Err(ChannelOutboundError::Transport("simulated".into()))
        }
    }

    #[tokio::test]
    async fn capturing_dispatcher_records_payload() {
        let d = CapturingDispatcher::default();
        let msg = OutboundMessage {
            channel: "whatsapp".into(),
            account_id: "wa.0".into(),
            to: "wa.42".into(),
            body: "hello".into(),
            msg_kind: "text".into(),
            attachments: vec![],
            reply_to_msg_id: None,
        };
        let ack = d.send(msg.clone()).await.unwrap();
        assert!(ack.outbound_message_id.is_none());
        let sent = d.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].body, "hello");
    }

    #[tokio::test]
    async fn capturing_dispatcher_round_trips_provider_id() {
        let d = CapturingDispatcher {
            respond_with_id: Some("msg-42".into()),
            ..Default::default()
        };
        let ack = d
            .send(OutboundMessage {
                channel: "whatsapp".into(),
                account_id: "wa.0".into(),
                to: "wa.99".into(),
                body: "x".into(),
                msg_kind: "text".into(),
                attachments: vec![],
                reply_to_msg_id: None,
            })
            .await
            .unwrap();
        assert_eq!(ack.outbound_message_id.as_deref(), Some("msg-42"));
    }

    #[tokio::test]
    async fn failing_dispatcher_returns_transport_error() {
        let d = AlwaysFailingDispatcher;
        let r = d
            .send(OutboundMessage {
                channel: "whatsapp".into(),
                account_id: "wa.0".into(),
                to: "wa.99".into(),
                body: "x".into(),
                msg_kind: "text".into(),
                attachments: vec![],
                reply_to_msg_id: None,
            })
            .await;
        assert!(matches!(r, Err(ChannelOutboundError::Transport(_))));
    }
}
