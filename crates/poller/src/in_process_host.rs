//! `InProcessHost` — daemon-side [`PollerHost`] for in-tree built-in
//! pollers (`webhook_poll`, `agent_turn`). Translates host trait calls
//! into direct calls against the runner's `AnyBroker`,
//! `AgentCredentialResolver`, optional `LlmRegistry` + `LlmConfig`.
//!
//! Subprocess pollers (plugin v2) use a different host impl in
//! `nexo-microapp-sdk::poller` that routes the same operations over
//! reverse-RPC.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_auth::CredentialsBundle;
use nexo_broker::{AnyBroker, BrokerHandle, Event};
use nexo_llm::{ChatMessage, ChatRequest, ChatRole, ResponseContent};
use serde_json::{json, Value};

use crate::host::{
    HostError, LlmInvokeRequest, LlmInvokeResponse, LlmMessage, LlmUsage, LogLevel, PollerHost,
};

/// Host adapter that talks directly to the daemon's in-memory broker,
/// credential resolver, and LLM registry. One per tick (cheap to
/// construct — only `Arc` clones).
pub struct InProcessHost {
    agent_id: String,
    job_id: String,
    broker: AnyBroker,
    credentials: Arc<CredentialsBundle>,
    llm_registry: Option<Arc<nexo_llm::LlmRegistry>>,
    llm_config: Option<Arc<nexo_config::LlmConfig>>,
}

impl InProcessHost {
    pub fn new(
        agent_id: impl Into<String>,
        job_id: impl Into<String>,
        broker: AnyBroker,
        credentials: Arc<CredentialsBundle>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            job_id: job_id.into(),
            broker,
            credentials,
            llm_registry: None,
            llm_config: None,
        }
    }

    pub fn with_llm(
        mut self,
        registry: Arc<nexo_llm::LlmRegistry>,
        config: Arc<nexo_config::LlmConfig>,
    ) -> Self {
        self.llm_registry = Some(registry);
        self.llm_config = Some(config);
        self
    }
}

const SOURCE: &str = "plugin.poller";

#[async_trait]
impl PollerHost for InProcessHost {
    async fn broker_publish(&self, topic: String, payload: Vec<u8>) -> Result<(), HostError> {
        let value: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
        let event = Event::new(&topic, SOURCE, value);
        self.broker
            .publish(&topic, event)
            .await
            .map_err(|e| HostError::BrokerUnavailable(e.to_string()))?;
        Ok(())
    }

    async fn credentials_get(&self, channel: String) -> Result<Value, HostError> {
        let channel_static: &'static str = match channel.as_str() {
            "whatsapp" => nexo_auth::handle::WHATSAPP,
            "telegram" => nexo_auth::handle::TELEGRAM,
            "google" => nexo_auth::handle::GOOGLE,
            // Daemon does not pre-translate other channels; return
            // CredentialsMissing so the poller classifies as Permanent
            // and the operator wires the channel in `credentials.yaml`.
            _ => return Err(HostError::CredentialsMissing(channel)),
        };

        let handle = self
            .credentials
            .resolver
            .resolve(&self.agent_id, channel_static)
            .map_err(|_| HostError::CredentialsMissing(channel_static.to_string()))?;

        let mut out = json!({
            "channel": channel_static,
            "account_id": handle.account_id_raw(),
            "agent_id": self.agent_id,
        });

        // Per-channel typed accessors layered on top so pollers don't
        // have to know about credential bundle internals. Adding a
        // new accessor here is the channel-specific touch point.
        if channel_static == nexo_auth::handle::GOOGLE {
            if let Some(acct) = self.credentials.google_account(&self.agent_id) {
                if let Value::Object(ref mut m) = out {
                    m.insert(
                        "client_id_path".into(),
                        Value::String(acct.client_id_path.to_string_lossy().into_owned()),
                    );
                    m.insert(
                        "token_path".into(),
                        Value::String(acct.token_path.to_string_lossy().into_owned()),
                    );
                }
            }
        }

        Ok(out)
    }

    async fn log(
        &self,
        level: LogLevel,
        message: String,
        fields: Value,
    ) -> Result<(), HostError> {
        match level {
            LogLevel::Trace => tracing::trace!(job_id = %self.job_id, %message, ?fields, "poller log"),
            LogLevel::Debug => tracing::debug!(job_id = %self.job_id, %message, ?fields, "poller log"),
            LogLevel::Info => tracing::info!(job_id = %self.job_id, %message, ?fields, "poller log"),
            LogLevel::Warn => tracing::warn!(job_id = %self.job_id, %message, ?fields, "poller log"),
            LogLevel::Error => tracing::error!(job_id = %self.job_id, %message, ?fields, "poller log"),
        }
        Ok(())
    }

    async fn metric_inc(&self, name: String, labels: Value) -> Result<(), HostError> {
        // V1: emit as a tracing event so operators see the increment in
        // logs. The poller crate's telemetry module does not expose a
        // generic Prometheus counter API yet; adding one is tracked as
        // a Phase 96 follow-up so subprocess pollers can fan out
        // arbitrary counters into the daemon's `/metrics` endpoint.
        tracing::info!(
            metric = %name,
            job_id = %self.job_id,
            agent_id = %self.agent_id,
            ?labels,
            "poller metric_inc"
        );
        Ok(())
    }

    async fn llm_invoke(
        &self,
        request: LlmInvokeRequest,
    ) -> Result<LlmInvokeResponse, HostError> {
        let registry = self.llm_registry.as_ref().ok_or_else(|| HostError::Rpc {
            code: -32602,
            message: "InProcessHost has no LlmRegistry — wire runner with with_llm(...)".into(),
        })?;
        let config = self.llm_config.as_ref().ok_or_else(|| HostError::Rpc {
            code: -32602,
            message: "InProcessHost has no LlmConfig — wire runner with with_llm(...)".into(),
        })?;

        let model_cfg = nexo_config::types::agents::ModelConfig {
            provider: request.provider.clone(),
            model: request.model.clone(),
        };
        let client = registry.build(config, &model_cfg).map_err(|e| HostError::Rpc {
            code: -32602,
            message: format!("LLM client build failed: {e}"),
        })?;

        let mut messages: Vec<ChatMessage> = Vec::with_capacity(request.messages.len());
        for m in request.messages {
            let role = match m.role.as_str() {
                "system" => ChatRole::System,
                "user" => ChatRole::User,
                "assistant" => ChatRole::Assistant,
                other => {
                    return Err(HostError::Rpc {
                        code: -32602,
                        message: format!("unknown role '{other}' (system|user|assistant)"),
                    });
                }
            };
            messages.push(ChatMessage {
                role,
                content: m.content,
                attachments: Vec::new(),
                tool_call_id: None,
                name: None,
                tool_calls: Vec::new(),
            });
        }

        let mut req = ChatRequest::new(&request.model, messages);
        if let Some(mt) = request.max_tokens {
            req.max_tokens = mt;
        }
        // ChatRequest does not expose `temperature` on every provider;
        // skip silently rather than fail when the field is set.

        let response = client.chat(req).await.map_err(|e| {
            let msg = e.to_string();
            let is_perm = msg.contains("401")
                || msg.contains("403")
                || msg.contains("not registered")
                || msg.contains("not present in config.providers");
            if is_perm {
                HostError::Rpc {
                    code: -32002,
                    message: msg,
                }
            } else {
                HostError::Other(e)
            }
        })?;

        let text = match response.content {
            ResponseContent::Text(t) => t,
            ResponseContent::ToolCalls(_) => {
                return Err(HostError::Rpc {
                    code: -32602,
                    message: "llm_invoke does not support tool calls — drop tools from request"
                        .into(),
                });
            }
        };

        let usage = (response.usage.prompt_tokens > 0 || response.usage.completion_tokens > 0)
            .then_some(LlmUsage {
                input_tokens: response.usage.prompt_tokens,
                output_tokens: response.usage.completion_tokens,
            });

        Ok(LlmInvokeResponse {
            content: text,
            model_id: request.model,
            usage,
        })
    }
}

// Suppress dead-code lint when neither the in-tree builtins nor an
// integration test ever read these fields — kept as Arc clones so the
// host outlives the tick.
const _: fn() = || {
    let _ = LlmMessage {
        role: String::new(),
        content: String::new(),
    };
};
