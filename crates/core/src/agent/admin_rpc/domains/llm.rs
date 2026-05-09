//! Phase 82.10.t.x — `nexo/admin/llm/complete` runtime handler.
//!
//! Lets a microapp / extension delegate an LLM completion to the
//! daemon, which holds the provider configs (`llm.yaml`) +
//! credentials (`secrets/<NAME>.txt`). The caller chooses the
//! provider id (matching `llm.yaml.providers.<id>`) + model;
//! daemon picks the right factory, builds the request with the
//! authenticated key, and returns the assembled body.
//!
//! Capability: `llm_complete`. The handler does no provider
//! discovery itself — it delegates to the registered
//! [`LlmCompleter`] adapter (built at boot in `nexo-setup`,
//! reads from the daemon's `LlmRegistry` + `LlmConfig`). Tests
//! plug a mock adapter that captures the input + returns a
//! canned response without touching the network.

use async_trait::async_trait;
use serde_json::Value;

#[cfg(test)]
use nexo_tool_meta::admin::llm::LlmUsage;
use nexo_tool_meta::admin::llm::{LlmCompleteInput, LlmCompleteResponse};

use super::super::dispatcher::{AdminRpcError, AdminRpcResult};

/// Pluggable adapter that runs a single-turn LLM completion
/// against one of the daemon's configured providers. The
/// production impl lives in
/// `nexo_setup::llm_completer::RegistryLlmCompleter`; tests
/// replace it with a mock.
#[async_trait]
pub trait LlmCompleter: Send + Sync {
    /// Run the completion. `Err(_)` covers structural problems
    /// (unknown provider, factory build failure, transport
    /// error); `Ok(_)` is a successful round-trip even when the
    /// model returned an unhelpful answer.
    async fn complete(&self, input: LlmCompleteInput)
        -> Result<LlmCompleteResponse, AdminRpcError>;
}

/// Dispatcher entry point. Validates the JSON-RPC params shape,
/// then forwards to the configured completer impl. The handler
/// does no LLM logic itself — keeps testability tight + lets
/// the binary replace the adapter without changing this file.
pub async fn complete(completer: &dyn LlmCompleter, raw_params: Value) -> AdminRpcResult {
    let input: LlmCompleteInput = match serde_json::from_value(raw_params) {
        Ok(i) => i,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                "llm/complete params: {e}"
            )));
        }
    };
    if input.provider.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("provider is empty".into()));
    }
    if input.model.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("model is empty".into()));
    }
    if input.messages.is_empty() && input.system_prompt.is_none() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "messages cannot be empty when system_prompt is also missing".into(),
        ));
    }
    for (idx, m) in input.messages.iter().enumerate() {
        if !matches!(m.role.as_str(), "system" | "user" | "assistant" | "tool") {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                "messages[{idx}].role must be one of system|user|assistant|tool, got `{}`",
                m.role
            )));
        }
    }
    if let Some(t) = input.temperature {
        if !(0.0..=2.0).contains(&t) || t.is_nan() {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(
                "temperature must be in [0, 2]".into(),
            ));
        }
    }
    match completer.complete(input).await {
        Ok(r) => AdminRpcResult::ok(serde_json::to_value(r).unwrap_or(Value::Null)),
        Err(e) => AdminRpcResult::err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::admin::llm::LlmChatMessage;
    use std::sync::Arc;

    struct MockCompleter {
        captured: Arc<tokio::sync::Mutex<Option<LlmCompleteInput>>>,
        reply: String,
    }

    #[async_trait]
    impl LlmCompleter for MockCompleter {
        async fn complete(
            &self,
            input: LlmCompleteInput,
        ) -> Result<LlmCompleteResponse, AdminRpcError> {
            *self.captured.lock().await = Some(input.clone());
            Ok(LlmCompleteResponse {
                content: self.reply.clone(),
                model: input.model,
                usage: LlmUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                },
            })
        }
    }

    fn ok_input() -> Value {
        serde_json::json!({
            "provider": "deepseek",
            "model": "deepseek-v4-flash",
            "messages": [
                { "role": "user", "content": "hola" }
            ]
        })
    }

    #[tokio::test]
    async fn rejects_empty_provider() {
        let mock = MockCompleter {
            captured: Arc::new(Default::default()),
            reply: "x".into(),
        };
        let mut p = ok_input();
        p["provider"] = serde_json::json!("");
        let r = complete(&mock, p).await;
        assert!(r.error.is_some());
    }

    #[tokio::test]
    async fn rejects_unknown_role() {
        let mock = MockCompleter {
            captured: Arc::new(Default::default()),
            reply: "x".into(),
        };
        let mut p = ok_input();
        p["messages"][0]["role"] = serde_json::json!("nope");
        let r = complete(&mock, p).await;
        assert!(r.error.is_some());
    }

    #[tokio::test]
    async fn rejects_temperature_out_of_range() {
        let mock = MockCompleter {
            captured: Arc::new(Default::default()),
            reply: "x".into(),
        };
        let mut p = ok_input();
        p["temperature"] = serde_json::json!(5.0);
        let r = complete(&mock, p).await;
        assert!(r.error.is_some());
    }

    #[tokio::test]
    async fn forwards_to_completer_on_happy_path() {
        let captured = Arc::new(tokio::sync::Mutex::new(None));
        let mock = MockCompleter {
            captured: Arc::clone(&captured),
            reply: "respuesta".into(),
        };
        let r = complete(&mock, ok_input()).await;
        let v = r.result.expect("expected ok result");
        assert_eq!(v["content"], "respuesta");
        assert_eq!(v["model"], "deepseek-v4-flash");
        let cap = captured.lock().await.clone().unwrap();
        assert_eq!(cap.provider, "deepseek");
        assert_eq!(cap.messages.len(), 1);
        assert_eq!(cap.messages[0].role, "user");
        assert_eq!(cap.messages[0].content, "hola");
    }

    /// `messages` empty AND `system_prompt` empty ⇒ reject.
    /// `messages` empty BUT `system_prompt` present ⇒ ok (the
    /// system-only path the daemon uses to seed an empty
    /// completion is rare but legal — used by warm-up / health
    /// probes that just exercise the auth path).
    #[tokio::test]
    async fn allows_system_only_when_messages_empty() {
        let mock = MockCompleter {
            captured: Arc::new(Default::default()),
            reply: "y".into(),
        };
        let p = serde_json::json!({
            "provider": "deepseek",
            "model": "x",
            "messages": [],
            "system_prompt": "you are a helpful assistant"
        });
        let r = complete(&mock, p).await;
        assert!(r.result.is_some());
    }

    /// Suppress unused warning when running this file's tests
    /// in isolation — `LlmChatMessage` is only used through
    /// JSON serialisation in the dispatcher path.
    #[test]
    fn _msg_type_is_publicly_accessible() {
        let _ = LlmChatMessage {
            role: "user".into(),
            content: "ping".into(),
        };
    }
}
