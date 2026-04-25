//! LLM-facing tools for the Telegram channel. Each tool publishes an
//! event on `plugin.outbound.telegram` which the bot's dispatcher then
//! routes through the HTTP Bot API.
//!
//! Defense-in-depth: every tool honors the agent's
//! `outbound_allowlist.telegram` (configured in `agents.yaml`). Empty
//! list = unrestricted; populated = only listed chat_ids reachable.

use async_trait::async_trait;
use nexo_broker::{BrokerHandle, Event};
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use std::sync::Arc;

fn parse_chat_id(args: &Value) -> anyhow::Result<i64> {
    // Accept either integer or string (LLMs often stringify large ints).
    if let Some(i) = args.get("chat_id").and_then(|v| v.as_i64()) {
        return Ok(i);
    }
    if let Some(s) = args.get("chat_id").and_then(|v| v.as_str()) {
        return s
            .parse::<i64>()
            .map_err(|e| anyhow::anyhow!("chat_id not parseable: {e}"));
    }
    anyhow::bail!("`chat_id` is required (integer or numeric string)")
}

fn allowlist_denied(ctx: &AgentContext, chat_id: i64) -> bool {
    // Per-binding allowlist: same story as whatsapp — the effective
    // policy carries the override for the matched binding so a strict
    // sales channel can pin outbound to a specific chat while a
    // wildcard binding keeps the agent-level default.
    let effective = ctx.effective_policy();
    let list = &effective.outbound_allowlist.telegram;
    if list.is_empty() {
        return false;
    }
    !list.contains(&chat_id)
}

async fn publish_outbound(ctx: &AgentContext, payload: Value) -> anyhow::Result<()> {
    // Phase 17 — resolve the target bot instance from the agent's
    // `credentials.telegram` binding. Legacy single-bot deployments
    // without a resolver stay on the un-suffixed topic for back-compat.
    let (topic, breaker_handle) = match ctx.credentials.as_ref() {
        Some(resolver) => match resolver.resolve(&ctx.agent_id, nexo_auth::handle::TELEGRAM) {
            Ok(handle) => {
                nexo_auth::audit::audit_outbound(&handle, "plugin.outbound.telegram");
                nexo_auth::telemetry::inc_usage(
                    nexo_auth::handle::TELEGRAM,
                    handle.account_id_raw(),
                    &ctx.agent_id,
                    "outbound",
                );
                let topic = format!("plugin.outbound.telegram.{}", handle.account_id_raw());
                (topic, Some(handle))
            }
            Err(_) => ("plugin.outbound.telegram".to_string(), None),
        },
        None => ("plugin.outbound.telegram".to_string(), None),
    };

    let breaker = match (breaker_handle.as_ref(), ctx.breakers.as_ref()) {
        (Some(h), Some(reg)) => Some(reg.for_handle(h)),
        _ => None,
    };
    if let Some(ref b) = breaker {
        if !b.allow() {
            if let Some(ref h) = breaker_handle {
                nexo_auth::telemetry::set_breaker_state(
                    nexo_auth::handle::TELEGRAM,
                    h.account_id_raw(),
                    nexo_auth::telemetry::BreakerState::Open,
                );
            }
            return Err(anyhow::anyhow!(
                "circuit breaker open for telegram instance — back off and retry"
            ));
        }
    }

    let event = Event::new(&topic, &ctx.agent_id, payload);
    let result = ctx.broker.publish(&topic, event).await;
    if let (Some(b), Some(h)) = (&breaker, &breaker_handle) {
        match &result {
            Ok(_) => {
                b.on_success();
                nexo_auth::telemetry::set_breaker_state(
                    nexo_auth::handle::TELEGRAM,
                    h.account_id_raw(),
                    nexo_auth::telemetry::BreakerState::Closed,
                );
            }
            Err(_) => {
                b.on_failure();
                if b.is_open() {
                    nexo_auth::telemetry::set_breaker_state(
                        nexo_auth::handle::TELEGRAM,
                        h.account_id_raw(),
                        nexo_auth::telemetry::BreakerState::Open,
                    );
                }
            }
        }
    }
    result.map_err(anyhow::Error::from)
}

// ─────────────────────────────────────────────────────────────────────
// send_message
// ─────────────────────────────────────────────────────────────────────

pub struct TelegramSendMessageTool;

impl TelegramSendMessageTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "telegram_send_message".to_string(),
            description: "Send a Telegram text message to an arbitrary chat_id. Use \
                 when you need to notify a third party or post to a group/channel \
                 that isn't the current conversation. Honor the agent's outbound \
                 allowlist."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "chat_id": {
                        "type": ["integer", "string"],
                        "description": "Telegram chat_id (negative for groups/channels)."
                    },
                    "text": {
                        "type": "string",
                        "description": "Message body. Plain text."
                    }
                },
                "required": ["chat_id", "text"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for TelegramSendMessageTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let chat_id = parse_chat_id(&args)?;
        let text = args["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`text` is required"))?;
        if text.is_empty() {
            anyhow::bail!("`text` must not be empty");
        }
        if allowlist_denied(ctx, chat_id) {
            anyhow::bail!("chat_id {chat_id} not in telegram outbound allowlist");
        }
        publish_outbound(ctx, json!({ "text": text, "to": chat_id })).await?;
        Ok(json!({ "ok": true, "chat_id": chat_id }))
    }
}

// ─────────────────────────────────────────────────────────────────────
// send_reply
// ─────────────────────────────────────────────────────────────────────

pub struct TelegramSendReplyTool;

impl TelegramSendReplyTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "telegram_send_reply".to_string(),
            description: "Send a Telegram text that replies-to a specific message.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "chat_id": { "type": ["integer", "string"], "description": "Target chat_id." },
                    "reply_to_message_id": { "type": ["integer", "string"], "description": "ID of the message to reply to." },
                    "text": { "type": "string", "description": "Reply body." }
                },
                "required": ["chat_id", "reply_to_message_id", "text"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for TelegramSendReplyTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let chat_id = parse_chat_id(&args)?;
        let reply_to = args
            .get("reply_to_message_id")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .ok_or_else(|| anyhow::anyhow!("`reply_to_message_id` required"))?;
        let text = args["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`text` required"))?;
        if allowlist_denied(ctx, chat_id) {
            anyhow::bail!("chat_id {chat_id} not in telegram allowlist");
        }
        publish_outbound(
            ctx,
            json!({
                "kind": "reply",
                "chat_id": chat_id,
                "reply_to_message_id": reply_to,
                "text": text,
            }),
        )
        .await?;
        Ok(json!({ "ok": true }))
    }
}

// ─────────────────────────────────────────────────────────────────────
// send_reaction
// ─────────────────────────────────────────────────────────────────────

pub struct TelegramSendReactionTool;

impl TelegramSendReactionTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "telegram_send_reaction".to_string(),
            description: "React to a Telegram message with a single emoji.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "chat_id":    { "type": ["integer", "string"] },
                    "message_id": { "type": ["integer", "string"] },
                    "emoji":      { "type": "string", "description": "Single emoji." }
                },
                "required": ["chat_id", "message_id", "emoji"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for TelegramSendReactionTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let chat_id = parse_chat_id(&args)?;
        let message_id = args
            .get("message_id")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .ok_or_else(|| anyhow::anyhow!("`message_id` required"))?;
        let emoji = args["emoji"].as_str().unwrap_or("");
        if allowlist_denied(ctx, chat_id) {
            anyhow::bail!("chat_id {chat_id} not in telegram allowlist");
        }
        publish_outbound(
            ctx,
            json!({
                "kind": "reaction",
                "chat_id": chat_id,
                "message_id": message_id,
                "emoji": emoji,
            }),
        )
        .await?;
        Ok(json!({ "ok": true }))
    }
}

// ─────────────────────────────────────────────────────────────────────
// edit_message
// ─────────────────────────────────────────────────────────────────────

pub struct TelegramEditMessageTool;

impl TelegramEditMessageTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "telegram_edit_message".to_string(),
            description: "Edit the text of a Telegram message previously sent by this bot."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "chat_id":    { "type": ["integer", "string"] },
                    "message_id": { "type": ["integer", "string"] },
                    "text":       { "type": "string", "description": "Replacement body." },
                    "parse_mode": { "type": "string", "description": "Optional: Markdown, MarkdownV2, HTML." }
                },
                "required": ["chat_id", "message_id", "text"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for TelegramEditMessageTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let chat_id = parse_chat_id(&args)?;
        let message_id = args
            .get("message_id")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .ok_or_else(|| anyhow::anyhow!("`message_id` required"))?;
        let text = args["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`text` required"))?;
        let parse_mode = args["parse_mode"].as_str();
        if allowlist_denied(ctx, chat_id) {
            anyhow::bail!("chat_id {chat_id} not in telegram allowlist");
        }
        let mut payload = json!({
            "kind": "edit_message",
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
        });
        if let Some(pm) = parse_mode {
            payload["parse_mode"] = Value::String(pm.to_string());
        }
        publish_outbound(ctx, payload).await?;
        Ok(json!({ "ok": true }))
    }
}

// ─────────────────────────────────────────────────────────────────────
// send_location
// ─────────────────────────────────────────────────────────────────────

pub struct TelegramSendLocationTool;

impl TelegramSendLocationTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "telegram_send_location".to_string(),
            description: "Share a geographic location with a Telegram chat.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "chat_id":  { "type": ["integer", "string"] },
                    "latitude":  { "type": "number" },
                    "longitude": { "type": "number" }
                },
                "required": ["chat_id", "latitude", "longitude"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for TelegramSendLocationTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let chat_id = parse_chat_id(&args)?;
        let lat = args["latitude"]
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("`latitude` required"))?;
        let lng = args["longitude"]
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("`longitude` required"))?;
        if allowlist_denied(ctx, chat_id) {
            anyhow::bail!("chat_id {chat_id} not in telegram allowlist");
        }
        publish_outbound(
            ctx,
            json!({
                "kind": "send_location",
                "chat_id": chat_id,
                "latitude": lat,
                "longitude": lng,
            }),
        )
        .await?;
        Ok(json!({ "ok": true }))
    }
}

// ─────────────────────────────────────────────────────────────────────
// send_media (photo / video / document / audio / voice / animation)
// ─────────────────────────────────────────────────────────────────────

pub struct TelegramSendMediaTool;

impl TelegramSendMediaTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "telegram_send_media".to_string(),
            description: "Send media (photo/video/document/audio/voice/animation) to a \
                 Telegram chat by URL or local file path."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "chat_id": { "type": ["integer", "string"] },
                    "media":   { "type": "string", "enum": ["photo", "video", "document", "audio", "voice", "animation"] },
                    "source":  { "type": "string", "description": "URL or local file path." },
                    "caption": { "type": "string", "description": "Optional caption." }
                },
                "required": ["chat_id", "media", "source"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for TelegramSendMediaTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let chat_id = parse_chat_id(&args)?;
        let media = args["media"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`media` required"))?;
        let source = args["source"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`source` required"))?;
        let caption = args["caption"].as_str().unwrap_or("").to_string();
        if allowlist_denied(ctx, chat_id) {
            anyhow::bail!("chat_id {chat_id} not in telegram allowlist");
        }
        let kind = match media {
            "photo" => "send_photo",
            "video" => "send_video",
            "document" => "send_document",
            "audio" => "send_audio",
            "voice" => "send_voice",
            "animation" => "send_animation",
            other => anyhow::bail!("unsupported media kind: {other}"),
        };
        publish_outbound(
            ctx,
            json!({
                "kind": kind,
                "chat_id": chat_id,
                "source": source,
                "caption": caption,
            }),
        )
        .await?;
        Ok(json!({ "ok": true }))
    }
}

// ─────────────────────────────────────────────────────────────────────
// Registration helper
// ─────────────────────────────────────────────────────────────────────

pub fn register_telegram_tools(tools: &Arc<ToolRegistry>) {
    tools.register(TelegramSendMessageTool::tool_def(), TelegramSendMessageTool);
    tools.register(TelegramSendReplyTool::tool_def(), TelegramSendReplyTool);
    tools.register(
        TelegramSendReactionTool::tool_def(),
        TelegramSendReactionTool,
    );
    tools.register(TelegramEditMessageTool::tool_def(), TelegramEditMessageTool);
    tools.register(
        TelegramSendLocationTool::tool_def(),
        TelegramSendLocationTool,
    );
    tools.register(TelegramSendMediaTool::tool_def(), TelegramSendMediaTool);
}
