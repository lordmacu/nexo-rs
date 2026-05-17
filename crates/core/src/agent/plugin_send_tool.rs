//! `plugin_channel_send` — agnostic outbound-send LLM tool.
//!
//! Wraps an [`Arc<dyn ChannelOutboundDispatcher>`](crate::agent::admin_rpc::channel_outbound::ChannelOutboundDispatcher)
//! so agents (and the MCP autonomous worker, Phase 81.20.x F3
//! follow-up) can dispatch outbound messages through any channel
//! plugin without knowing the plugin-specific broker topic or
//! payload shape. The dispatcher's registered
//! [`ChannelPayloadTranslator`] for the named channel handles
//! translation; missing translator surfaces as
//! `channel_unavailable` at call time.
//!
//! Use this tool when the agent must send a one-off message
//! through a channel it normally talks to (email follow-up,
//! WhatsApp reminder, Telegram nudge) and the agent does NOT
//! have direct access to the plugin's native tool (e.g. the
//! autonomous_worker MCP mode runs out-of-process and cannot
//! reach the daemon's `RemoteToolHandler` instances).

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use std::sync::Arc;

use super::admin_rpc::channel_outbound::{
    ChannelOutboundDispatcher, ChannelOutboundError, OutboundMessage,
};
use super::tool_registry::ToolHandler;
use super::AgentContext;

pub const TOOL_NAME: &str = "plugin_channel_send";

/// Stable wrapper that lets an agent send a message through any
/// channel plugin registered with the dispatcher. Agnostic over
/// the plugin's native protocol — the dispatcher's translator
/// owns broker topic + payload encoding.
#[derive(Debug)]
pub struct PluginChannelSendTool {
    dispatcher: Arc<dyn ChannelOutboundDispatcher>,
}

impl PluginChannelSendTool {
    pub fn new(dispatcher: Arc<dyn ChannelOutboundDispatcher>) -> Self {
        Self { dispatcher }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: TOOL_NAME.into(),
            description: "Send one outbound message through any channel plugin (email, \
                          telegram, whatsapp, …) without knowing the plugin's native \
                          tool name. The framework's outbound dispatcher routes by \
                          `channel`, translating the call into the plugin's broker \
                          topic + payload. Returns the provider-side \
                          `outbound_message_id` when the channel surfaces one, or \
                          null for fire-and-forget delivery."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "channel": {
                        "type": "string",
                        "description": "Plugin id (`email`, `telegram`, `whatsapp`, …)."
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Channel-side account / instance label \
                                        (e.g. the email account `instance`, the \
                                        telegram bot username, the whatsapp \
                                        session). Empty / `default` routes via \
                                        the plugin's default-instance topic."
                    },
                    "to": {
                        "type": "string",
                        "description": "Channel-native recipient (email address, \
                                        telegram chat id as string, whatsapp jid)."
                    },
                    "body": {
                        "type": "string",
                        "description": "Plain-text body / template payload."
                    },
                    "msg_kind": {
                        "type": "string",
                        "description": "`text` (default), `template`, `media`, or \
                                        plugin-specific. Plugins surface \
                                        `invalid_params` for unsupported kinds.",
                        "default": "text"
                    },
                    "reply_to_msg_id": {
                        "type": "string",
                        "description": "Provider message id to thread the reply \
                                        against. Plugins that ignore threading drop \
                                        this field silently."
                    },
                    "attachments": {
                        "type": "array",
                        "description": "Optional channel-specific attachments \
                                        (subject for email, media URLs, template \
                                        variables). Each entry is plugin-defined.",
                        "items": { "type": "object" }
                    }
                },
                "required": ["channel", "account_id", "to", "body"]
            }),
        }
    }

    fn build_message(args: &Value) -> Result<OutboundMessage> {
        let channel = args
            .get("channel")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("plugin_channel_send: 'channel' must be a non-empty string"))?
            .to_string();
        let account_id = args
            .get("account_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let to = args
            .get("to")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("plugin_channel_send: 'to' must be a non-empty string"))?
            .to_string();
        let body = args
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let msg_kind = args
            .get("msg_kind")
            .and_then(Value::as_str)
            .unwrap_or("text")
            .to_string();
        let reply_to_msg_id = args
            .get("reply_to_msg_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let attachments = args
            .get("attachments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(OutboundMessage {
            channel,
            account_id,
            to,
            body,
            msg_kind,
            attachments,
            reply_to_msg_id,
        })
    }
}

#[async_trait]
impl ToolHandler for PluginChannelSendTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        let msg = Self::build_message(&args)?;
        match self.dispatcher.send(msg).await {
            Ok(ack) => Ok(json!({
                "ok": true,
                "outbound_message_id": ack.outbound_message_id,
            })),
            Err(e) => match e {
                ChannelOutboundError::ChannelUnavailable(ch) => {
                    Err(anyhow!("channel_unavailable: {}", ch))
                }
                ChannelOutboundError::InvalidParams(s) => Err(anyhow!("invalid_params: {}", s)),
                ChannelOutboundError::Transport(s) => Err(anyhow!("transport: {}", s)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    use super::super::admin_rpc::channel_outbound::OutboundAck;

    #[derive(Debug)]
    struct CapturingDispatcher {
        captured: Mutex<Option<OutboundMessage>>,
    }

    #[async_trait]
    impl ChannelOutboundDispatcher for CapturingDispatcher {
        async fn send(
            &self,
            msg: OutboundMessage,
        ) -> std::result::Result<OutboundAck, ChannelOutboundError> {
            *self.captured.lock().unwrap() = Some(msg);
            Ok(OutboundAck {
                outbound_message_id: Some("msg-7".into()),
            })
        }
    }

    #[test]
    fn build_message_defaults_msg_kind_to_text() {
        let args = json!({
            "channel": "email",
            "account_id": "support",
            "to": "user@example.com",
            "body": "hello world",
        });
        let msg = PluginChannelSendTool::build_message(&args).unwrap();
        assert_eq!(msg.channel, "email");
        assert_eq!(msg.account_id, "support");
        assert_eq!(msg.to, "user@example.com");
        assert_eq!(msg.body, "hello world");
        assert_eq!(msg.msg_kind, "text");
        assert!(msg.reply_to_msg_id.is_none());
        assert!(msg.attachments.is_empty());
    }

    #[test]
    fn build_message_carries_attachments_and_reply_id() {
        let args = json!({
            "channel": "email",
            "account_id": "support",
            "to": "user@example.com",
            "body": "hi",
            "msg_kind": "reply",
            "reply_to_msg_id": "<abc@example.com>",
            "attachments": [{ "subject": "Re: hello" }],
        });
        let msg = PluginChannelSendTool::build_message(&args).unwrap();
        assert_eq!(msg.msg_kind, "reply");
        assert_eq!(msg.reply_to_msg_id.as_deref(), Some("<abc@example.com>"));
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0]["subject"], json!("Re: hello"));
    }

    #[test]
    fn build_message_missing_channel_errors() {
        let args = json!({ "to": "x@y.com", "body": "hi", "account_id": "" });
        let err = PluginChannelSendTool::build_message(&args).unwrap_err();
        assert!(err.to_string().contains("'channel'"));
    }

    #[test]
    fn build_message_empty_to_errors() {
        let args = json!({ "channel": "email", "account_id": "", "to": "", "body": "x" });
        let err = PluginChannelSendTool::build_message(&args).unwrap_err();
        assert!(err.to_string().contains("'to'"));
    }

    #[tokio::test]
    async fn dispatcher_receives_message_and_returns_ack() {
        let cap = Arc::new(CapturingDispatcher {
            captured: Mutex::new(None),
        });
        let msg = OutboundMessage {
            channel: "email".into(),
            account_id: "support".into(),
            to: "user@example.com".into(),
            body: "hello".into(),
            msg_kind: "text".into(),
            attachments: vec![],
            reply_to_msg_id: None,
        };
        let ack = cap.send(msg).await.unwrap();
        assert_eq!(ack.outbound_message_id.as_deref(), Some("msg-7"));
        assert_eq!(
            cap.captured.lock().unwrap().as_ref().unwrap().channel,
            "email"
        );
    }

    #[tokio::test]
    async fn dispatcher_channel_unavailable_propagates() {
        #[derive(Debug)]
        struct Unavail;
        #[async_trait]
        impl ChannelOutboundDispatcher for Unavail {
            async fn send(
                &self,
                msg: OutboundMessage,
            ) -> std::result::Result<OutboundAck, ChannelOutboundError> {
                Err(ChannelOutboundError::ChannelUnavailable(msg.channel))
            }
        }
        let d: Arc<dyn ChannelOutboundDispatcher> = Arc::new(Unavail);
        let msg = OutboundMessage {
            channel: "imessage".into(),
            account_id: "".into(),
            to: "x@y.com".into(),
            body: "hi".into(),
            msg_kind: "text".into(),
            attachments: vec![],
            reply_to_msg_id: None,
        };
        let err = d.send(msg).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("channel_unavailable"), "{}", msg);
        assert!(msg.contains("imessage"), "{}", msg);
    }

    #[test]
    fn tool_def_has_expected_schema_keys() {
        let def = PluginChannelSendTool::tool_def();
        assert_eq!(def.name, TOOL_NAME);
        let required = def
            .parameters
            .get("required")
            .and_then(Value::as_array)
            .unwrap();
        let required_set: std::collections::HashSet<&str> =
            required.iter().filter_map(Value::as_str).collect();
        assert!(required_set.contains("channel"));
        assert!(required_set.contains("account_id"));
        assert!(required_set.contains("to"));
        assert!(required_set.contains("body"));
    }
}
