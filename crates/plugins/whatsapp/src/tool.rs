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

use async_trait::async_trait;
use nexo_broker::{BrokerHandle, Event};
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
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
        // Empty list = unrestricted (back-compat), but operators
        // migrating from older config without an explicit
        // `outbound_allowlist.whatsapp:` may not realise the agent
        // can DM any number. Warn once per agent to surface the
        // gap. Using a process-wide map so we don't spam logs.
        warn_once_unrestricted(&ctx.agent_id);
        return false;
    }
    // Normalize each allowlist entry the same way we normalize the target
    // so `+57...`, `57...`, and `57...@s.whatsapp.net` all match.
    !list.iter().any(|entry| {
        let (entry_digits, _) = normalize_to(entry);
        entry_digits == digits
    })
}

fn warn_once_unrestricted(agent_id: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let set = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = match set.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if guard.insert(agent_id.to_string()) {
        tracing::warn!(
            agent = %agent_id,
            "WhatsApp outbound_allowlist is empty — unrestricted sends enabled (any number can be DM'd). \
             Set `outbound_allowlist.whatsapp` in agents.yaml or the binding override to lock down."
        );
    }
}

async fn publish_outbound(ctx: &AgentContext, payload: Value) -> anyhow::Result<()> {
    // Phase 17 — resolve the target WhatsApp instance from the agent's
    // credential binding. Falls back to the legacy single-topic
    // `plugin.outbound.whatsapp` when no resolver is attached (early
    // boot paths, tests) or when the agent has no `credentials.whatsapp`
    // declared (back-compat single-account deployments).
    let (topic, breaker_handle) = match ctx.credentials.as_ref() {
        Some(resolver) => match resolver.resolve(&ctx.agent_id, nexo_auth::handle::WHATSAPP) {
            Ok(handle) => {
                nexo_auth::audit::audit_outbound(&handle, "plugin.outbound.whatsapp");
                nexo_auth::telemetry::inc_usage(
                    nexo_auth::handle::WHATSAPP,
                    handle.account_id_raw(),
                    &ctx.agent_id,
                    "outbound",
                );
                let topic = format!("plugin.outbound.whatsapp.{}", handle.account_id_raw());
                (topic, Some(handle))
            }
            Err(_) => ("plugin.outbound.whatsapp".to_string(), None),
        },
        None => ("plugin.outbound.whatsapp".to_string(), None),
    };

    // Per-instance circuit breaker — a 429 from one number must not
    // trip the breaker for another. The breaker registry lives on the
    // AgentContext attached by `AgentRuntime::with_breakers`.
    let breaker = match (breaker_handle.as_ref(), ctx.breakers.as_ref()) {
        (Some(h), Some(reg)) => Some(reg.for_handle(h)),
        _ => None,
    };
    if let Some(ref b) = breaker {
        if !b.allow() {
            if let Some(ref h) = breaker_handle {
                nexo_auth::telemetry::set_breaker_state(
                    nexo_auth::handle::WHATSAPP,
                    h.account_id_raw(),
                    nexo_auth::telemetry::BreakerState::Open,
                );
            }
            return Err(anyhow::anyhow!(
                "circuit breaker open for whatsapp instance — back off and retry"
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
                    nexo_auth::handle::WHATSAPP,
                    h.account_id_raw(),
                    nexo_auth::telemetry::BreakerState::Closed,
                );
            }
            Err(_) => {
                b.on_failure();
                if b.is_open() {
                    nexo_auth::telemetry::set_breaker_state(
                        nexo_auth::handle::WHATSAPP,
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

pub struct WhatsappSendMessageTool;

impl WhatsappSendMessageTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "whatsapp_send_message".to_string(),
            description: "Send a WhatsApp text message to an arbitrary recipient \
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
            anyhow::bail!("recipient {digits} is not in this agent's whatsapp outbound allowlist");
        }
        publish_outbound(ctx, json!({ "kind": "text", "to": jid, "text": text })).await?;
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
            description: "Send a WhatsApp text that quotes a prior message from the \
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
        let to = args["to"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`to` required"))?;
        let msg_id = args["msg_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`msg_id` required"))?;
        let text = args["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`text` required"))?;
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
            description: "React to a WhatsApp message with a single emoji. Empty \
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
        let to = args["to"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`to` required"))?;
        let msg_id = args["msg_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`msg_id` required"))?;
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
            description: "Send an image, video, document, or audio file to a \
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
        let to = args["to"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`to` required"))?;
        let url = args["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("`url` required"))?;
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
    tools.register(
        WhatsappSendReactionTool::tool_def(),
        WhatsappSendReactionTool,
    );
    tools.register(WhatsappSendMediaTool::tool_def(), WhatsappSendMediaTool);
}
