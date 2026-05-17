//! `kind: agent_turn` — runs an LLM turn on a schedule and dispatches
//! the reply to a configured channel.
//!
//! Where ingestion pollers fetch external state and emit messages,
//! `agent_turn` is the cron complement: an operator-described prompt
//! that fires on a schedule, runs through `PollerHost::llm_invoke`,
//! and ships the model's output to a channel — no Rust required.
//!
//! ## YAML
//!
//! ```yaml
//! pollers:
//!   jobs:
//!     - id: kate_daily_summary
//!       kind: agent_turn
//!       agent: kate                # used to resolve outbound credentials
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
//!           Summarise unanswered emails from the last 24h.
//!         deliver:
//!           channel: telegram
//!           to: "-1001234567890"
//!         language: en              # optional — omit to let the model decide
//! ```

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::deliver::DeliverCfg;
use crate::error::PollerError;
use crate::host::{LlmInvokeRequest, LlmMessage, TickAck, TickMetrics};
use crate::poller::{PollContext, Poller};

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct AgentTurnJobConfig {
    pub llm: LlmRef,
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    pub deliver: DeliverCfg,
    /// Optional output-language directive — rendered as a
    /// `# OUTPUT LANGUAGE` system block prepended to messages.
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
        "Runs an LLM turn on a schedule and dispatches the reply to a configured channel."
    }

    fn validate(&self, config: &Value) -> Result<(), PollerError> {
        let cfg: AgentTurnJobConfig =
            serde_json::from_value(config.clone()).map_err(|e| PollerError::Config {
                job: "<agent_turn>".into(),
                reason: e.to_string(),
            })?;
        if cfg.user_prompt.trim().is_empty() {
            return Err(PollerError::Config {
                job: "<agent_turn>".into(),
                reason: "user_prompt must be non-empty".into(),
            });
        }
        if cfg.deliver.channel.trim().is_empty() {
            return Err(PollerError::Config {
                job: "<agent_turn>".into(),
                reason: "deliver.channel must be non-empty".into(),
            });
        }
        Ok(())
    }

    async fn tick(&self, ctx: &PollContext) -> Result<TickAck, PollerError> {
        let cfg: AgentTurnJobConfig =
            serde_json::from_value(ctx.config.clone()).map_err(|e| PollerError::Config {
                job: ctx.job_id.clone(),
                reason: e.to_string(),
            })?;

        let mut messages: Vec<LlmMessage> = Vec::new();
        if let Some(sp) = cfg.system_prompt.as_deref() {
            let sp = sp.trim();
            if !sp.is_empty() {
                messages.push(LlmMessage {
                    role: "system".into(),
                    content: sp.to_string(),
                });
            }
        }
        if let Some(lang) = cfg.language.as_deref().and_then(validate_lang) {
            messages.push(LlmMessage {
                role: "system".into(),
                content: format!(
                    "# OUTPUT LANGUAGE\n\nRespond in {lang}. Workspace docs and tool \
                     descriptions are in English; reply to the user in {lang}."
                ),
            });
        }
        messages.push(LlmMessage {
            role: "user".into(),
            content: cfg.user_prompt.clone(),
        });

        let llm_req = LlmInvokeRequest {
            provider: cfg.llm.provider.clone(),
            model: cfg.llm.model.clone(),
            messages,
            max_tokens: cfg.max_tokens,
            temperature: None,
        };

        let response = tokio::select! {
            r = ctx.host.llm_invoke(llm_req) => r,
            _ = ctx.cancel.cancelled() => {
                return Ok(TickAck::default());
            }
        }
        .map_err(|e: crate::host::HostError| {
            use crate::host::HostErrorKind;
            match e.into_poller_kind() {
                HostErrorKind::Permanent => {
                    PollerError::Permanent(anyhow::anyhow!("llm_invoke failed"))
                }
                HostErrorKind::Config => PollerError::Config {
                    job: ctx.job_id.clone(),
                    reason: "llm_invoke config error".into(),
                },
                HostErrorKind::Transient => {
                    PollerError::Transient(anyhow::anyhow!("llm_invoke failed"))
                }
            }
        })?;

        let trimmed = response.content.trim();
        if trimmed.is_empty() {
            tracing::warn!(
                job = %ctx.job_id,
                "agent_turn produced empty text — skipping delivery"
            );
            return Ok(TickAck {
                next_cursor: None,
                next_interval_hint: None,
                metrics: Some(TickMetrics {
                    items_seen: 1,
                    items_dispatched: 0,
                }),
            });
        }

        // Resolve account_id for the configured channel + publish via host.
        let cred = ctx
            .host
            .credentials_get(cfg.deliver.channel.clone())
            .await
            .map_err(|e| PollerError::Permanent(anyhow::anyhow!("credentials_get: {e}")))?;
        let account_id = cred
            .get("account_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                PollerError::Permanent(anyhow::anyhow!(
                    "credentials_get('{}') returned no `account_id`",
                    cfg.deliver.channel
                ))
            })?
            .to_string();
        let topic = format!("plugin.outbound.{}.{}", cfg.deliver.channel, account_id);
        let payload = json!({
            "kind": "text",
            "to": cfg.deliver.to,
            "text": trimmed,
        });
        let payload_bytes = serde_json::to_vec(&payload)
            .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;
        ctx.host
            .broker_publish(topic, payload_bytes)
            .await
            .map_err(|e| PollerError::Transient(anyhow::anyhow!("broker_publish: {e}")))?;

        Ok(TickAck {
            next_cursor: None,
            next_interval_hint: None,
            metrics: Some(TickMetrics {
                items_seen: 1,
                items_dispatched: 1,
            }),
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
            "deliver": { "channel": "telegram", "to": "-100" }
        });
        AgentTurnPoller::new()
            .validate(&cfg)
            .expect("minimal config validates");
    }

    #[test]
    fn validate_rejects_empty_user_prompt() {
        let cfg = json!({
            "llm": { "provider": "anthropic", "model": "x" },
            "user_prompt": "   ",
            "deliver": { "channel": "telegram", "to": "a" }
        });
        let err = AgentTurnPoller::new().validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("user_prompt"));
    }

    #[test]
    fn validate_rejects_unknown_field() {
        let cfg = json!({
            "llm": { "provider": "anthropic", "model": "x" },
            "user_prompt": "p",
            "deliver": { "channel": "telegram", "to": "a" },
            "bogus_key": 1
        });
        AgentTurnPoller::new()
            .validate(&cfg)
            .expect_err("deny_unknown_fields rejects typos");
    }

    #[test]
    fn validate_accepts_recipient_alias() {
        let cfg = json!({
            "llm": { "provider": "anthropic", "model": "x" },
            "user_prompt": "p",
            "deliver": { "channel": "telegram", "recipient": "a" }
        });
        AgentTurnPoller::new()
            .validate(&cfg)
            .expect("recipient alias accepted");
    }

    #[test]
    fn lang_sanitiser_strips_newlines() {
        assert_eq!(validate_lang("es\nbomb"), Some("esbomb".to_string()));
        assert_eq!(validate_lang("   "), None);
        assert_eq!(validate_lang(""), None);
    }
}
