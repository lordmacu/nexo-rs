//! LLM-facing tools for sending WhatsApp messages programmatically —
//! independent of the reactive reply path. Every tool publishes an
//! event on `plugin.outbound.whatsapp` which the plugin's dispatcher
//! then routes through the live `Session`.
//!
//! Defense-in-depth: each tool honors the agent's
//! `outbound_allowlist.whatsapp` (configured in `agents.yaml`). Empty
//! list = unrestricted (back-compat); populated list = recipient must
//! match or the call is rejected before any broker publish. That
//! keeps a prompt-injected agent from spraying arbitrary numbers.

use agent_broker::{BrokerHandle, Event};
use agent_core::agent::context::AgentContext;
use agent_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use agent_llm::ToolDef;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Normalize a phone number to the digits-only form we allowlist against
/// and to the JID the WhatsApp dispatcher expects (`<digits>@s.whatsapp.net`).
/// Accepts forms: `+573115728852`, `573115728852`, `573115728852@s.whatsapp.net`.
fn normalize_to(raw: &str) -> (String, String) {
    let base = raw.trim();
    let stripped = base
        .trim_start_matches('+')
        .split('@')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>();
    let jid = if base.contains('@') {
        base.to_string()
    } else {
        format!("{stripped}@s.whatsapp.net")
    };
    (stripped, jid)
}

fn allowlist_denied(ctx: &AgentContext, digits: &str) -> bool {
    // Per-binding allowlist: the effective policy carries whichever
    // override the matched binding defined (or the agent-level default
    // when no override). This means a sales binding can lock outbound
    // to the advisor's number while a private-channel binding keeps
    // the wider agent-level list.
    let effective = ctx.effective_policy();
    let list = &effective.outbound_allowlist.whatsapp;
    if list.is_empty() {
        return false;
    }
    // Normalize each allowlist entry the same way we normalize the target
    // so `+57...`, `57...`, and `57...@s.whatsapp.net` all match.
    !list.iter().any(|entry| {
        let (entry_digits, _) = normalize_to(entry);
        entry_digits == digits
    })
}

async fn publish_outbound(
    ctx: &AgentContext,
    payload: Value,
) -> anyhow::Result<()> {
    let topic = "plugin.outbound.whatsapp";
    let event = Event::new(topic, &ctx.agent_id, payload);
    ctx.broker.publish(topic, event).await?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// send_message
// ─────────────────────────────────────────────────────────────────────

pub struct WhatsappSendMessageTool;

impl WhatsappSendMessageTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "whatsapp_send_message".to_string(),
            description:
                "Send a WhatsApp text message to an arbitrary recipient \
                 (not necessarily the current conversation). Use when you \
                 need to notify a third party — e.g. a sales lead, an \
                 escalation channel. Honor the agent's outbound allowlist."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "Recipient phone number (e.g. +573115728852) or WhatsApp JID."
                    },
                    "text": {
                        "type": "string",
                        "description": "Message body to send. Plain text."
                    }
                },
                "required": ["to", "text"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for WhatsappSendMessageTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let to = args["to"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`to` is required"))?;
        let text = args["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`text` is required"))?;
        if text.is_empty() {
            anyhow::bail!("`text` must not be empty");
        }
        let (digits, jid) = normalize_to(to);
        if digits.is_empty() {
            anyhow::bail!("`to` must contain digits");
        }
        if allowlist_denied(ctx, &digits) {
            anyhow::bail!(
                "recipient {digits} is not in this agent's whatsapp outbound allowlist"
            );
        }
        publish_outbound(
            ctx,
            json!({ "kind": "text", "to": jid, "text": text }),
        )
        .await?;
        Ok(json!({ "ok": true, "to": digits }))
    }
}

// ─────────────────────────────────────────────────────────────────────
// send_reply (quotes a specific inbound message)
// ─────────────────────────────────────────────────────────────────────

pub struct WhatsappSendReplyTool;

impl WhatsappSendReplyTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "whatsapp_send_reply".to_string(),
            description:
                "Send a WhatsApp text that quotes a prior message from the \
                 same chat. Use for threaded replies where context matters."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to":     { "type": "string", "description": "Chat JID or phone number." },
                    "msg_id": { "type": "string", "description": "ID of the message being quoted." },
                    "text":   { "type": "string", "description": "Reply body." }
                },
                "required": ["to", "msg_id", "text"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for WhatsappSendReplyTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let to = args["to"].as_str().ok_or_else(|| anyhow::anyhow!("`to` required"))?;
        let msg_id = args["msg_id"].as_str().ok_or_else(|| anyhow::anyhow!("`msg_id` required"))?;
        let text = args["text"].as_str().ok_or_else(|| anyhow::anyhow!("`text` required"))?;
        let (digits, jid) = normalize_to(to);
        if allowlist_denied(ctx, &digits) {
            anyhow::bail!("recipient {digits} not in whatsapp allowlist");
        }
        publish_outbound(
            ctx,
            json!({ "kind": "reply", "to": jid, "msg_id": msg_id, "text": text }),
        )
        .await?;
        Ok(json!({ "ok": true }))
    }
}

// ─────────────────────────────────────────────────────────────────────
// send_reaction (emoji tap-back on a specific message)
// ─────────────────────────────────────────────────────────────────────

pub struct WhatsappSendReactionTool;

impl WhatsappSendReactionTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "whatsapp_send_reaction".to_string(),
            description:
                "React to a WhatsApp message with a single emoji. Empty \
                 emoji removes the reaction."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to":     { "type": "string", "description": "Chat JID or phone number." },
                    "msg_id": { "type": "string", "description": "Target message ID." },
                    "emoji":  { "type": "string", "description": "Single emoji (or empty to clear)." }
                },
                "required": ["to", "msg_id", "emoji"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for WhatsappSendReactionTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let to = args["to"].as_str().ok_or_else(|| anyhow::anyhow!("`to` required"))?;
        let msg_id = args["msg_id"].as_str().ok_or_else(|| anyhow::anyhow!("`msg_id` required"))?;
        let emoji = args["emoji"].as_str().unwrap_or("");
        let (digits, jid) = normalize_to(to);
        if allowlist_denied(ctx, &digits) {
            anyhow::bail!("recipient {digits} not in whatsapp allowlist");
        }
        publish_outbound(
            ctx,
            json!({ "kind": "react", "to": jid, "msg_id": msg_id, "emoji": emoji }),
        )
        .await?;
        Ok(json!({ "ok": true }))
    }
}

// ─────────────────────────────────────────────────────────────────────
// send_media (image / video / document / audio via URL)
// ─────────────────────────────────────────────────────────────────────

pub struct WhatsappSendMediaTool;

impl WhatsappSendMediaTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "whatsapp_send_media".to_string(),
            description:
                "Send an image, video, document, or audio file to a \
                 WhatsApp chat. The media is referenced by URL (the plugin \
                 downloads and re-uploads through WA)."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to":        { "type": "string", "description": "Chat JID or phone number." },
                    "url":       { "type": "string", "description": "Public URL or local file path of the media." },
                    "caption":   { "type": "string", "description": "Optional caption." },
                    "file_name": { "type": "string", "description": "Optional file name (for documents)." }
                },
                "required": ["to", "url"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for WhatsappSendMediaTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let to = args["to"].as_str().ok_or_else(|| anyhow::anyhow!("`to` required"))?;
        let url = args["url"].as_str().ok_or_else(|| anyhow::anyhow!("`url` required"))?;
        let caption = args["caption"].as_str().unwrap_or("").to_string();
        let file_name = args["file_name"].as_str().map(|s| s.to_string());
        let (digits, jid) = normalize_to(to);
        if allowlist_denied(ctx, &digits) {
            anyhow::bail!("recipient {digits} not in whatsapp allowlist");
        }
        let mut payload = json!({
            "kind": "media",
            "to": jid,
            "url": url,
            "caption": caption,
        });
        if let Some(name) = file_name {
            payload["file_name"] = Value::String(name);
        }
        publish_outbound(ctx, payload).await?;
        Ok(json!({ "ok": true }))
    }
}

// ─────────────────────────────────────────────────────────────────────
// Registration helper
// ─────────────────────────────────────────────────────────────────────

/// Register the full `whatsapp_*` tool family. Call once per agent that
/// has the `whatsapp` plugin enabled.
pub fn register_whatsapp_tools(tools: &Arc<ToolRegistry>) {
    tools.register(WhatsappSendMessageTool::tool_def(), WhatsappSendMessageTool);
    tools.register(WhatsappSendReplyTool::tool_def(), WhatsappSendReplyTool);
    tools.register(WhatsappSendReactionTool::tool_def(), WhatsappSendReactionTool);
    tools.register(WhatsappSendMediaTool::tool_def(), WhatsappSendMediaTool);
}
