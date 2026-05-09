//! Phase 82.10.t.x — production [`LlmCompleter`] adapter.
//!
//! Bridges `nexo/admin/llm/complete` to the daemon's
//! [`LlmRegistry`] + [`LlmConfig`]. Resolves the provider
//! instance + factory exactly the way the agent runtime does
//! (so behaviour is consistent: same key resolution, same
//! tenant scoping, same retry semantics) — extensions just
//! piggyback on the same plumbing without duplicating it.
//!
//! Tests construct the adapter against a stub `LlmConfig` +
//! the registry's `with_factory_for_test` builder. Production
//! wires `RegistryLlmCompleter::new(registry, llm_cfg_handle)`
//! during `setup::admin_bootstrap`.

use std::sync::Arc;

use async_trait::async_trait;

use nexo_config::types::agents::ModelConfig;
use nexo_config::types::llm::LlmConfig;
use nexo_core::agent::admin_rpc::dispatcher::AdminRpcError;
use nexo_core::agent::admin_rpc::domains::llm::LlmCompleter;
use nexo_llm::registry::LlmRegistry;
use nexo_llm::types::{ChatMessage, ChatRequest, ChatRole, ResponseContent};
use nexo_tool_meta::admin::llm::{LlmCompleteInput, LlmCompleteResponse, LlmUsage};

/// Production adapter. Holds the registry + a hot-swappable
/// `LlmConfig` snapshot so the next admin call always reads the
/// latest providers (operator-edited via
/// `nexo/admin/llm_providers/upsert` triggers a reload).
pub struct RegistryLlmCompleter {
    registry: Arc<LlmRegistry>,
    cfg: Arc<LlmConfig>,
}

impl RegistryLlmCompleter {
    /// Wire at boot with the daemon's snapshot of `llm.yaml`.
    /// The daemon doesn't hot-reload providers in v1 (operator
    /// must restart after `llm_providers/upsert`), so a plain
    /// `Arc<LlmConfig>` is sufficient. When a future phase
    /// lands hot-reload, swap to `Arc<ArcSwap<LlmConfig>>`
    /// here without changing the public surface.
    pub fn new(registry: Arc<LlmRegistry>, cfg: Arc<LlmConfig>) -> Arc<Self> {
        Arc::new(Self { registry, cfg })
    }
}

fn role_from_str(role: &str) -> ChatRole {
    match role {
        "system" => ChatRole::System,
        "assistant" => ChatRole::Assistant,
        "tool" => ChatRole::Tool,
        // The dispatcher already validated against the
        // {system, user, assistant, tool} set, so anything
        // else here would mean a bypass — default to `User`
        // rather than panicking.
        _ => ChatRole::User,
    }
}

#[async_trait]
impl LlmCompleter for RegistryLlmCompleter {
    async fn complete(
        &self,
        input: LlmCompleteInput,
    ) -> Result<LlmCompleteResponse, AdminRpcError> {
        // Resolve provider via the registry. `ModelConfig` is
        // the same shape an agent's `model:` block uses; reusing
        // it keeps the resolution path identical to the runtime
        // agent loop.
        let model_cfg = ModelConfig {
            provider: input.provider.clone(),
            model: input.model.clone(),
        };
        let client = self
            .registry
            .build(self.cfg.as_ref(), &model_cfg)
            .map_err(|e| {
                AdminRpcError::Internal(format!(
                    "build llm client for provider `{}` model `{}`: {e}",
                    input.provider, input.model
                ))
            })?;

        let mut messages: Vec<ChatMessage> = input
            .messages
            .into_iter()
            .map(|m| ChatMessage {
                role: role_from_str(&m.role),
                content: m.content,
                tool_call_id: None,
                name: None,
                tool_calls: vec![],
                attachments: vec![],
            })
            .collect();

        // `system_prompt` convenience field: when caller
        // supplied it AND no `system` role message is already
        // first, splice it in front. Preserves caller intent
        // either way.
        if let Some(sp) = input.system_prompt.as_ref() {
            let already_has_system = messages
                .first()
                .is_some_and(|m| matches!(m.role, ChatRole::System));
            if !already_has_system {
                messages.insert(
                    0,
                    ChatMessage {
                        role: ChatRole::System,
                        content: sp.clone(),
                        tool_call_id: None,
                        name: None,
                        tool_calls: vec![],
                        attachments: vec![],
                    },
                );
            }
        }

        let mut req = ChatRequest::new(input.model.clone(), messages);
        if let Some(t) = input.temperature {
            req.temperature = t;
        }
        if let Some(m) = input.max_tokens {
            req.max_tokens = m;
        }
        // Mirror system_prompt onto the legacy field too — some
        // factories (anthropic) prefer to pull from there even
        // when a `System` role message is present.
        if let Some(sp) = input.system_prompt {
            req.system_prompt = Some(sp);
        }

        let resp = client
            .chat(req)
            .await
            .map_err(|e| AdminRpcError::Internal(format!("llm chat call: {e}")))?;

        let content = match resp.content {
            ResponseContent::Text(s) => s,
            // Tool-call only response: surface as empty text.
            // Marketing's draft generator doesn't request tools
            // today; if a future caller does, they'll need a
            // dedicated path that consumes `tool_calls` directly.
            ResponseContent::ToolCalls(_) => String::new(),
        };
        let prompt = u64::from(resp.usage.prompt_tokens);
        let completion = u64::from(resp.usage.completion_tokens);
        Ok(LlmCompleteResponse {
            content,
            model: input.model,
            usage: LlmUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
            },
        })
    }
}
