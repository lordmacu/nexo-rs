//! Phase 82.10.f — `nexo/admin/llm_providers/*` handlers.
//!
//! Operates on `llm.yaml.providers.<id>`. Abstracted via
//! [`LlmYamlPatcher`] so this crate stays cycle-free vs
//! `nexo-setup`.

use serde_json::Value;

use nexo_tool_meta::admin::llm_providers::{
    LlmProviderSummary, LlmProviderUpsertInput, LlmProvidersDeleteParams,
    LlmProvidersDeleteResponse, LlmProvidersListResponse,
};

use super::agents::YamlPatcher;
use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Yaml mutation surface for `llm.yaml`. Production wraps an
/// `nexo_setup::yaml_patch`-style adapter pointed at the LLM
/// config file.
pub trait LlmYamlPatcher: Send + Sync {
    /// List provider ids in source order.
    fn list_provider_ids(&self) -> anyhow::Result<Vec<String>>;
    /// Read a dotted field under `providers.<id>.*`.
    fn read_provider_field(
        &self,
        provider_id: &str,
        dotted: &str,
    ) -> anyhow::Result<Option<Value>>;
    /// Upsert a dotted field under `providers.<id>.*`.
    fn upsert_provider_field(
        &self,
        provider_id: &str,
        dotted: &str,
        value: Value,
    ) -> anyhow::Result<()>;
    /// Remove the entire `providers.<id>` block.
    fn remove_provider(&self, provider_id: &str) -> anyhow::Result<()>;
}

/// `nexo/admin/llm_providers/list` — return all providers from
/// `llm.yaml`.
pub fn list(patcher: &dyn LlmYamlPatcher) -> AdminRpcResult {
    let ids = match patcher.list_provider_ids() {
        Ok(i) => i,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "llm.yaml read: {e}"
            )));
        }
    };
    let mut providers: Vec<LlmProviderSummary> = ids
        .into_iter()
        .filter_map(|id| read_summary(patcher, &id).ok().flatten())
        .collect();
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    AdminRpcResult::ok(
        serde_json::to_value(LlmProvidersListResponse { providers })
            .unwrap_or(Value::Null),
    )
}

/// `nexo/admin/llm_providers/upsert` — create or update a
/// provider block. Validates that `api_key_env` resolves to an
/// env var that exists at call time (operator already exported
/// the secret).
pub fn upsert(
    patcher: &dyn LlmYamlPatcher,
    params: Value,
    reload_signal: &dyn Fn(),
) -> AdminRpcResult {
    let input: LlmProviderUpsertInput = match serde_json::from_value(params) {
        Ok(i) => i,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if input.id.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("id is empty".into()));
    }
    if input.base_url.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("base_url is empty".into()));
    }
    if std::env::var(&input.api_key_env).is_err() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
            "api_key_env `{}` is not set in process env",
            input.api_key_env
        )));
    }

    if let Err(e) = patcher.upsert_provider_field(
        &input.id,
        "base_url",
        Value::String(input.base_url.clone()),
    ) {
        return AdminRpcResult::err(AdminRpcError::Internal(format!(
            "yaml write: {e}"
        )));
    }
    if let Err(e) = patcher.upsert_provider_field(
        &input.id,
        "api_key_env",
        Value::String(input.api_key_env.clone()),
    ) {
        return AdminRpcResult::err(AdminRpcError::Internal(format!(
            "yaml write: {e}"
        )));
    }
    if !input.headers.is_empty() {
        let map: serde_json::Map<String, Value> = input
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        if let Err(e) =
            patcher.upsert_provider_field(&input.id, "headers", Value::Object(map))
        {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml write: {e}"
            )));
        }
    }
    reload_signal();

    let summary = LlmProviderSummary {
        id: input.id,
        base_url: input.base_url,
        api_key_env: input.api_key_env,
    };
    AdminRpcResult::ok(serde_json::to_value(summary).unwrap_or(Value::Null))
}

/// `nexo/admin/llm_providers/delete` — remove a provider block.
/// Reject when any agent in `agents.yaml` still references this
/// provider (caller must `agents/upsert` to swap providers first).
pub fn delete(
    llm: &dyn LlmYamlPatcher,
    agents: &dyn YamlPatcher,
    params: Value,
    reload_signal: &dyn Fn(),
) -> AdminRpcResult {
    let p: LlmProvidersDeleteParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };

    // Refuse when any agent uses this provider.
    let agent_ids = match agents.list_agent_ids() {
        Ok(ids) => ids,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "agents.yaml read: {e}"
            )));
        }
    };
    for aid in &agent_ids {
        if let Some(Value::String(provider)) = agents
            .read_agent_field(aid, "model.provider")
            .ok()
            .flatten()
        {
            if provider == p.provider_id {
                return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                    "provider `{}` still in use by agent `{aid}`; \
                     swap providers via agents/upsert before deleting",
                    p.provider_id
                )));
            }
        }
    }

    let existed = matches!(
        llm.list_provider_ids(),
        Ok(ids) if ids.iter().any(|id| id == &p.provider_id)
    );
    if !existed {
        return AdminRpcResult::ok(
            serde_json::to_value(LlmProvidersDeleteResponse { removed: false })
                .unwrap_or(Value::Null),
        );
    }
    match llm.remove_provider(&p.provider_id) {
        Ok(()) => {
            reload_signal();
            AdminRpcResult::ok(
                serde_json::to_value(LlmProvidersDeleteResponse { removed: true })
                    .unwrap_or(Value::Null),
            )
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "yaml remove: {e}"
        ))),
    }
}

fn read_summary(
    patcher: &dyn LlmYamlPatcher,
    provider_id: &str,
) -> anyhow::Result<Option<LlmProviderSummary>> {
    let base_url = match patcher.read_provider_field(provider_id, "base_url")? {
        Some(Value::String(s)) => s,
        _ => return Ok(None),
    };
    let api_key_env = match patcher.read_provider_field(provider_id, "api_key_env")? {
        Some(Value::String(s)) => s,
        _ => String::new(),
    };
    Ok(Some(LlmProviderSummary {
        id: provider_id.to_string(),
        base_url,
        api_key_env,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockLlm {
        providers: Mutex<HashMap<String, HashMap<String, Value>>>,
    }
    impl MockLlm {
        fn with(provs: &[(&str, &str, &str)]) -> Self {
            let me = Self::default();
            for (id, base_url, env) in provs {
                me.providers
                    .lock()
                    .unwrap()
                    .entry(id.to_string())
                    .or_default()
                    .insert("base_url".into(), Value::String((*base_url).into()));
                me.providers
                    .lock()
                    .unwrap()
                    .entry(id.to_string())
                    .or_default()
                    .insert("api_key_env".into(), Value::String((*env).into()));
            }
            me
        }
    }
    impl LlmYamlPatcher for MockLlm {
        fn list_provider_ids(&self) -> anyhow::Result<Vec<String>> {
            let mut v: Vec<String> = self.providers.lock().unwrap().keys().cloned().collect();
            v.sort();
            Ok(v)
        }
        fn read_provider_field(
            &self,
            id: &str,
            dotted: &str,
        ) -> anyhow::Result<Option<Value>> {
            Ok(self
                .providers
                .lock()
                .unwrap()
                .get(id)
                .and_then(|m| m.get(dotted).cloned()))
        }
        fn upsert_provider_field(
            &self,
            id: &str,
            dotted: &str,
            value: Value,
        ) -> anyhow::Result<()> {
            self.providers
                .lock()
                .unwrap()
                .entry(id.to_string())
                .or_default()
                .insert(dotted.to_string(), value);
            Ok(())
        }
        fn remove_provider(&self, id: &str) -> anyhow::Result<()> {
            self.providers.lock().unwrap().remove(id);
            Ok(())
        }
    }

    #[derive(Default)]
    struct MockAgents {
        agents: Mutex<HashMap<String, HashMap<String, Value>>>,
    }
    impl MockAgents {
        fn with_provider(provider: &str) -> Self {
            let me = Self::default();
            me.agents
                .lock()
                .unwrap()
                .entry("ana".into())
                .or_default()
                .insert("model.provider".into(), Value::String(provider.into()));
            me
        }
    }
    impl YamlPatcher for MockAgents {
        fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> {
            Ok(self.agents.lock().unwrap().keys().cloned().collect())
        }
        fn read_agent_field(
            &self,
            id: &str,
            dotted: &str,
        ) -> anyhow::Result<Option<Value>> {
            Ok(self
                .agents
                .lock()
                .unwrap()
                .get(id)
                .and_then(|m| m.get(dotted).cloned()))
        }
        fn upsert_agent_field(
            &self,
            _id: &str,
            _dotted: &str,
            _value: Value,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        fn remove_agent(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn llm_providers_list_returns_alpha_order() {
        let llm = MockLlm::with(&[
            ("zphi", "u1", "K1"),
            ("anthropic", "u2", "K2"),
            ("minimax", "u3", "K3"),
        ]);
        let result = list(&llm);
        let response: LlmProvidersListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.providers.len(), 3);
        assert_eq!(response.providers[0].id, "anthropic");
        assert_eq!(response.providers[1].id, "minimax");
        assert_eq!(response.providers[2].id, "zphi");
    }

    #[test]
    fn llm_providers_upsert_validates_env_var_exists() {
        let llm = MockLlm::default();
        // Use an env var guaranteed not present.
        let result = upsert(
            &llm,
            serde_json::json!({
                "id": "newp",
                "base_url": "https://x",
                "api_key_env": "NEXO_DEFINITELY_NOT_SET_VAR_42"
            }),
            &|| {},
        );
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn llm_providers_upsert_writes_when_env_var_present() {
        let llm = MockLlm::default();
        // PATH is always set.
        let result = upsert(
            &llm,
            serde_json::json!({
                "id": "newp",
                "base_url": "https://x",
                "api_key_env": "PATH"
            }),
            &|| {},
        );
        assert!(result.result.is_some());
        // Stored.
        assert_eq!(llm.list_provider_ids().unwrap(), vec!["newp".to_string()]);
    }

    #[test]
    fn llm_providers_delete_rejects_when_agent_uses_provider() {
        let llm = MockLlm::with(&[("minimax", "u", "K")]);
        let agents = MockAgents::with_provider("minimax");
        let result = delete(
            &llm,
            &agents,
            serde_json::json!({ "provider_id": "minimax" }),
            &|| {},
        );
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
        // Provider still present.
        assert!(llm.list_provider_ids().unwrap().contains(&"minimax".into()));
    }

    #[test]
    fn llm_providers_delete_removes_unused_provider() {
        let llm = MockLlm::with(&[("retired", "u", "K")]);
        let agents = MockAgents::with_provider("minimax"); // not retired
        let result = delete(
            &llm,
            &agents,
            serde_json::json!({ "provider_id": "retired" }),
            &|| {},
        );
        let response: LlmProvidersDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.removed);
        assert!(llm.list_provider_ids().unwrap().is_empty());
    }

    #[test]
    fn llm_providers_delete_unknown_id_idempotent() {
        let llm = MockLlm::default();
        let agents = MockAgents::default();
        let result = delete(
            &llm,
            &agents,
            serde_json::json!({ "provider_id": "ghost" }),
            &|| {},
        );
        let response: LlmProvidersDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(!response.removed);
    }
}
