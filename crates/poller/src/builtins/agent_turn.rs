//! `kind: agent_turn` — runs an LLM turn on a schedule and dispatches
//! the reply to a configured channel.
//!
//! Where `gmail` / `rss` / `webhook_poll` are *data ingestion* modules
//! (fetch external state, emit messages), `agent_turn` is the cron
//! complement: an operator-described prompt that fires every N
//! seconds / on a cron expression / once at a specific time, runs
//! through a real `LlmClient`, and ships the model's output to a
//! channel — no Rust required.
//!
//! ## YAML
//!
//! ```yaml
//! pollers:
//!   jobs:
//!     - id: kate_daily_summary
//!       kind: agent_turn
//!       agent: kate                # used only as the OutboundDelivery sender
//!       schedule:
//!         cron: "0 0 8 * * *"
//!         tz: America/Bogota
//!       config:
//!         llm:
//!           provider: anthropic     # must be a key in llm.yaml::providers
//!           model: claude-haiku-4-5
//!         system_prompt: |
//!           You are Kate, a personal assistant.
//!         user_prompt: |
//!           Summarise unanswered emails from the last 24h. One bullet
//!           per email; include sender + subject.
//!         deliver:
//!           channel: telegram
//!           recipient: "-1001234567890"
//!         language: en              # optional — omit to let the model decide
//! ```
//!
//! ## Notes
//!
//! - The poller does NOT carry workspace docs (IDENTITY, SOUL,
//!   MEMORY) — this is intentional. `agent_turn` is for cron-style
//!   one-shot prompts, not full agent sessions. If you need MEMORY
//!   awareness, route through a real agent and have it run a tool.
//! - The runner must be wired with `with_llm(registry, config)` at
//!   boot. Without it, every tick fails with `PollerError::Config`.
//! - Errors are classified: 4xx from the LLM endpoint = `Permanent`
//!   (config bug), 5xx / network = `Transient` (retried by the runner
//!   per backoff policy). Token-limit errors land as `Permanent` so
//!   the breaker trips quickly instead of burning quota.
//! - Cursor stays empty — the job has no inter-tick state to persist.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use agent_auth::handle::{Channel, GOOGLE, TELEGRAM, WHATSAPP};
use agent_llm::{ChatMessage, ChatRequest, ChatRole, ResponseContent};

use crate::error::PollerError;
use crate::poller::{OutboundDelivery, PollContext, Poller, TickOutcome};

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct AgentTurnJobConfig {
    pub llm: LlmRef,
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    pub deliver: DeliverCfg,
    /// Optional output-language directive — same semantics as
    /// `AgentConfig.language` (Phase 19). Rendered as a
    /// `# OUTPUT LANGUAGE` system block.
    #[serde(default)]
    pub language: Option<String>,
    /// Optional per-tick max-tokens cap. None = let the model
    /// default decide.
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LlmRef {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DeliverCfg {
    pub channel: String,
    pub recipient: String,
}

pub struct AgentTurnPoller;

impl AgentTurnPoller {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AgentTurnPoller {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_channel(s: &str) -> Result<Channel, PollerError> {
    match s.trim().to_ascii_lowercase().as_str() {
        "whatsapp" => Ok(WHATSAPP),
        "telegram" => Ok(TELEGRAM),
        "google" => Ok(GOOGLE),
        other => Err(PollerError::Permanent(anyhow::anyhow!(
            "agent_turn: unknown channel '{other}' (supported: whatsapp, telegram, google)"
        ))),
    }
}

fn validate_lang(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && *c != '\n' && *c != '\r')
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(64).collect())
}

#[async_trait]
impl Poller for AgentTurnPoller {
    fn kind(&self) -> &'static str {
        "agent_turn"
    }

    fn description(&self) -> &'static str {
        "Runs an LLM turn on a schedule and dispatches the reply to a configured channel. \
         Cron-style replacement for hand-coded modules that just want 'fire prompt at time T'."
    }

    fn validate(&self, config: &Value) -> Result<(), PollerError> {
        let cfg: AgentTurnJobConfig =
            serde_json::from_value(config.clone()).map_err(|e| PollerError::Config {
                job: "<agent_turn>".into(),
                reason: e.to_string(),
            })?;
        parse_channel(&cfg.deliver.channel)?;
        if cfg.user_prompt.trim().is_empty() {
            return Err(PollerError::Config {
                job: "<agent_turn>".into(),
                reason: "user_prompt must be non-empty".into(),
            });
        }
        Ok(())
    }

    async fn tick(&self, ctx: &PollContext) -> Result<TickOutcome, PollerError> {
        let cfg: AgentTurnJobConfig =
            serde_json::from_value(ctx.config.clone()).map_err(|e| PollerError::Config {
                job: ctx.job_id.clone(),
                reason: e.to_string(),
            })?;

        let registry = ctx.llm_registry.as_ref().ok_or_else(|| PollerError::Config {
            job: ctx.job_id.clone(),
            reason: "agent_turn requires the runner to be wired with with_llm(...) at boot".into(),
        })?;
        let llm_config = ctx.llm_config.as_ref().ok_or_else(|| PollerError::Config {
            job: ctx.job_id.clone(),
            reason: "agent_turn requires LlmConfig — wire with_llm(registry, config)".into(),
        })?;

        let model_cfg = agent_config::types::agents::ModelConfig {
            provider: cfg.llm.provider.clone(),
            model: cfg.llm.model.clone(),
        };
        let client = registry
            .build(llm_config, &model_cfg)
            .map_err(|e| PollerError::Config {
                job: ctx.job_id.clone(),
                reason: format!("LLM client build: {e}"),
            })?;

        let mut messages: Vec<ChatMessage> = Vec::new();
        if let Some(sp) = cfg.system_prompt.as_deref() {
            let sp = sp.trim();
            if !sp.is_empty() {
                messages.push(ChatMessage {
                    role: ChatRole::System,
                    content: sp.to_string(),
                    attachments: Vec::new(),
                    tool_call_id: None,
                    name: None,
                    tool_calls: Vec::new(),
                });
            }
        }
        if let Some(lang) = cfg.language.as_deref().and_then(validate_lang) {
            messages.push(ChatMessage {
                role: ChatRole::System,
                content: format!(
                    "# OUTPUT LANGUAGE\n\nRespond in {lang}. Workspace docs and tool \
                     descriptions are in English; reply to the user in {lang}."
                ),
                attachments: Vec::new(),
                tool_call_id: None,
                name: None,
                tool_calls: Vec::new(),
            });
        }
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: cfg.user_prompt.clone(),
            attachments: Vec::new(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        });

        let mut req = ChatRequest::new(&cfg.llm.model, messages);
        if let Some(mt) = cfg.max_tokens {
            req.max_tokens = mt;
        }

        let response = tokio::select! {
            r = client.chat(req) => r,
            _ = ctx.cancel.cancelled() => {
                return Ok(TickOutcome::default());
            }
        }
        .map_err(|e| {
            // Conservative classification: any LLM error is Transient
            // unless it clearly looks like a config / 4xx problem. The
            // breaker still trips after `breaker_threshold` failures
            // so a permanently broken job won't run forever.
            let msg = e.to_string();
            let is_perm = msg.contains("401")
                || msg.contains("403")
                || msg.contains("not registered")
                || msg.contains("not present in config.providers");
            if is_perm {
                PollerError::Permanent(e)
            } else {
                PollerError::Transient(e)
            }
        })?;

        let text = match response.content {
            ResponseContent::Text(t) => t,
            ResponseContent::ToolCalls(_) => {
                return Err(PollerError::Permanent(anyhow::anyhow!(
                    "agent_turn does not support tool calls — the LLM returned tool_use; \
                     remove tools from the prompt or use a real agent for this workflow"
                )));
            }
        };

        let trimmed = text.trim();
        if trimmed.is_empty() {
            tracing::warn!(
                job = %ctx.job_id,
                "agent_turn produced empty text — skipping delivery"
            );
            return Ok(TickOutcome {
                items_seen: 1,
                items_dispatched: 0,
                deliver: Vec::new(),
                next_cursor: None,
                next_interval_hint: None,
            });
        }

        let channel = parse_channel(&cfg.deliver.channel)?;
        let payload = json!({
            "kind": "text",
            "to": cfg.deliver.recipient,
            "text": trimmed,
        });

        Ok(TickOutcome {
            items_seen: 1,
            items_dispatched: 1,
            deliver: vec![OutboundDelivery {
                channel,
                recipient: cfg.deliver.recipient.clone(),
                payload,
            }],
            next_cursor: None,
            next_interval_hint: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_minimal_config() {
        let cfg = json!({
            "llm": { "provider": "anthropic", "model": "claude-haiku-4-5" },
            "user_prompt": "summarise the day",
            "deliver": { "channel": "telegram", "recipient": "-100" }
        });
        AgentTurnPoller::new()
            .validate(&cfg)
            .expect("minimal config validates");
    }

    #[test]
    fn validate_rejects_unknown_channel() {
        let cfg = json!({
            "llm": { "provider": "anthropic", "model": "x" },
            "user_prompt": "p",
            "deliver": { "channel": "smtp", "recipient": "a" }
        });
        let err = AgentTurnPoller::new().validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("smtp"));
    }

    #[test]
    fn validate_rejects_empty_user_prompt() {
        let cfg = json!({
            "llm": { "provider": "anthropic", "model": "x" },
            "user_prompt": "   ",
            "deliver": { "channel": "telegram", "recipient": "a" }
        });
        let err = AgentTurnPoller::new().validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("user_prompt"));
    }

    #[test]
    fn validate_rejects_unknown_field() {
        let cfg = json!({
            "llm": { "provider": "anthropic", "model": "x" },
            "user_prompt": "p",
            "deliver": { "channel": "telegram", "recipient": "a" },
            "bogus_key": 1
        });
        AgentTurnPoller::new()
            .validate(&cfg)
            .expect_err("deny_unknown_fields rejects typos");
    }

    #[test]
    fn lang_sanitiser_strips_newlines() {
        assert_eq!(validate_lang("es\nbomb"), Some("esbomb".to_string()));
        assert_eq!(validate_lang("   "), None);
        assert_eq!(validate_lang(""), None);
    }
}
