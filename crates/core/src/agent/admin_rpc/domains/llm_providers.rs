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

    /// Phase 83.8.12.5.c.b — list provider ids under
    /// `tenants.<tenant_id>.providers`. Empty when the tenant
    /// has no providers block (or the tenant doesn't exist).
    /// Default impl returns empty so legacy patchers without
    /// tenant support compile cleanly while behaving as if no
    /// tenant overrides existed.
    fn list_tenant_provider_ids(
        &self,
        _tenant_id: &str,
    ) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Phase 83.8.12.5.c.b — read a dotted field under
    /// `tenants.<tenant_id>.providers.<provider_id>.*`.
    /// Default impl returns `None`.
    fn read_tenant_provider_field(
        &self,
        _tenant_id: &str,
        _provider_id: &str,
        _dotted: &str,
    ) -> anyhow::Result<Option<Value>> {
        Ok(None)
    }

    /// Phase 83.8.12.5.c.b — upsert a dotted field under
    /// `tenants.<tenant_id>.providers.<provider_id>.*`.
    /// Default impl errors so unimplemented adapters surface
    /// the gap explicitly rather than silently no-op.
    fn upsert_tenant_provider_field(
        &self,
        _tenant_id: &str,
        _provider_id: &str,
        _dotted: &str,
        _value: Value,
    ) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(
            "upsert_tenant_provider_field not implemented for this LlmYamlPatcher"
        ))
    }

    /// Phase 83.8.12.5.c.b — remove the
    /// `tenants.<tenant_id>.providers.<provider_id>` block.
    fn remove_tenant_provider(
        &self,
        _tenant_id: &str,
        _provider_id: &str,
    ) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(
            "remove_tenant_provider not implemented for this LlmYamlPatcher"
        ))
    }
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

    // Phase 83.8.12.5.c.b — split the write path: tenant-scoped
    // upserts call `upsert_tenant_provider_field` (writes under
    // `tenants.<id>.providers.<provider_id>.*`); global upserts
    // call the legacy `upsert_provider_field` exactly as before.
    // Tenant id "" treated as None for defense-in-depth (caller
    // bug should not silently write to `tenants..providers.*`).
    let tenant_id = input
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let write_field = |dotted: &str, value: Value| -> anyhow::Result<()> {
        match tenant_id.as_deref() {
            Some(tid) => patcher.upsert_tenant_provider_field(tid, &input.id, dotted, value),
            None => patcher.upsert_provider_field(&input.id, dotted, value),
        }
    };

    if let Err(e) = write_field("base_url", Value::String(input.base_url.clone())) {
        return AdminRpcResult::err(AdminRpcError::Internal(format!(
            "yaml write: {e}"
        )));
    }
    if let Err(e) =
        write_field("api_key_env", Value::String(input.api_key_env.clone()))
    {
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
        if let Err(e) = write_field("headers", Value::Object(map)) {
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
        tenant_scope: tenant_id,
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
    let tenant_id = p
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Refuse when any agent of the SAME scope uses this
    // provider. Cross-scope agents are unaffected — a global
    // delete doesn't break tenant-scoped agents pointing at a
    // tenant-scoped provider with the same id, and vice versa.
    let agent_ids = match agents.list_agent_ids() {
        Ok(ids) => ids,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "agents.yaml read: {e}"
            )));
        }
    };
    for aid in &agent_ids {
        let Some(Value::String(provider)) = agents
            .read_agent_field(aid, "model.provider")
            .ok()
            .flatten()
        else {
            continue;
        };
        if provider != p.provider_id {
            continue;
        }
        // Match agent's tenant_id against the delete scope.
        let agent_tenant = agents
            .read_agent_field(aid, "tenant_id")
            .ok()
            .flatten()
            .and_then(|v| match v {
                Value::String(s) => Some(s),
                _ => None,
            });
        if agent_tenant.as_deref() == tenant_id.as_deref() {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                "provider `{}` still in use by agent `{aid}` (scope: {}); \
                 swap providers via agents/upsert before deleting",
                p.provider_id,
                tenant_id.as_deref().unwrap_or("global"),
            )));
        }
    }

    // Phase 83.8.12.5.c.b — split the delete path mirroring
    // upsert. Tenant scope checks the tenant's provider list +
    // calls `remove_tenant_provider`; global scope keeps the
    // legacy path.
    let existed = match tenant_id.as_deref() {
        Some(tid) => matches!(
            llm.list_tenant_provider_ids(tid),
            Ok(ids) if ids.iter().any(|id| id == &p.provider_id)
        ),
        None => matches!(
            llm.list_provider_ids(),
            Ok(ids) if ids.iter().any(|id| id == &p.provider_id)
        ),
    };
    if !existed {
        return AdminRpcResult::ok(
            serde_json::to_value(LlmProvidersDeleteResponse { removed: false })
                .unwrap_or(Value::Null),
        );
    }
    let removed = match tenant_id.as_deref() {
        Some(tid) => llm.remove_tenant_provider(tid, &p.provider_id),
        None => llm.remove_provider(&p.provider_id),
    };
    match removed {
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
        // Phase 83.8.12.5.c — global-scope reads stamp `None`.
        // Tenant-scoped reads (when the handler honours
        // `tenant_id` filter) populate this field with the
        // owning tenant id. Wire-up of the tenant read path
        // lands as a follow-up; today the reader is global
        // only so emitting None here is correct.
        tenant_scope: None,
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
        /// Phase 83.8.12.5.c.b — tenant providers keyed by
        /// `(tenant_id, provider_id)`.
        tenant_providers: Mutex<
            HashMap<(String, String), HashMap<String, Value>>,
        >,
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
        fn list_tenant_provider_ids(
            &self,
            tenant_id: &str,
        ) -> anyhow::Result<Vec<String>> {
            let mut v: Vec<String> = self
                .tenant_providers
                .lock()
                .unwrap()
                .keys()
                .filter(|(t, _)| t == tenant_id)
                .map(|(_, p)| p.clone())
                .collect();
            v.sort();
            Ok(v)
        }
        fn read_tenant_provider_field(
            &self,
            tenant_id: &str,
            provider_id: &str,
            dotted: &str,
        ) -> anyhow::Result<Option<Value>> {
            Ok(self
                .tenant_providers
                .lock()
                .unwrap()
                .get(&(tenant_id.to_string(), provider_id.to_string()))
                .and_then(|m| m.get(dotted).cloned()))
        }
        fn upsert_tenant_provider_field(
            &self,
            tenant_id: &str,
            provider_id: &str,
            dotted: &str,
            value: Value,
        ) -> anyhow::Result<()> {
            self.tenant_providers
                .lock()
                .unwrap()
                .entry((tenant_id.to_string(), provider_id.to_string()))
                .or_default()
                .insert(dotted.to_string(), value);
            Ok(())
        }
        fn remove_tenant_provider(
            &self,
            tenant_id: &str,
            provider_id: &str,
        ) -> anyhow::Result<()> {
            self.tenant_providers
                .lock()
                .unwrap()
                .remove(&(tenant_id.to_string(), provider_id.to_string()));
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

    // ── Phase 83.8.12.5.c.b — tenant-scoped CRUD ──

    /// `tenant_id: Some` upsert writes under tenant namespace
    /// + leaves the global table untouched. Resulting summary
    /// echoes `tenant_scope`.
    #[tokio::test]
    async fn llm_providers_upsert_with_tenant_id_writes_under_tenant_namespace() {
        // Make sure the env var the handler validates exists.
        std::env::set_var("MINIMAX_KEY_ACME_TEST", "value");
        let llm = MockLlm::default();
        let result = upsert(
            &llm,
            serde_json::json!({
                "id": "minimax",
                "base_url": "https://api.minimax.io",
                "api_key_env": "MINIMAX_KEY_ACME_TEST",
                "tenant_id": "acme",
            }),
            &|| {},
        );
        assert!(result.error.is_none(), "{result:?}");
        let summary: LlmProviderSummary =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(summary.tenant_scope.as_deref(), Some("acme"));
        // Global table untouched.
        assert!(llm.list_provider_ids().unwrap().is_empty());
        // Tenant namespace populated.
        assert_eq!(
            llm.list_tenant_provider_ids("acme").unwrap(),
            vec!["minimax".to_string()]
        );
        let v = llm
            .read_tenant_provider_field("acme", "minimax", "base_url")
            .unwrap();
        assert_eq!(v, Some(Value::String("https://api.minimax.io".into())));
    }

    /// Tenant delete removes ONLY the tenant-scoped block. A
    /// global provider with the same id stays intact (the two
    /// scopes are independent).
    #[tokio::test]
    async fn llm_providers_delete_with_tenant_id_isolates_from_global() {
        std::env::set_var("MINIMAX_KEY", "v");
        // Seed both: a global "minimax" + a tenant "acme.minimax".
        let llm = MockLlm::with(&[("minimax", "https://global", "MINIMAX_KEY")]);
        llm.upsert_tenant_provider_field(
            "acme",
            "minimax",
            "base_url",
            Value::String("https://acme".into()),
        )
        .unwrap();
        let agents = MockAgents::default(); // no agents reference these
        let result = delete(
            &llm,
            &agents,
            serde_json::json!({ "provider_id": "minimax", "tenant_id": "acme" }),
            &|| {},
        );
        let response: LlmProvidersDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.removed);
        // Global still there.
        assert!(llm.list_provider_ids().unwrap().contains(&"minimax".into()));
        // Tenant scope cleared.
        assert!(llm.list_tenant_provider_ids("acme").unwrap().is_empty());
    }

    /// Tenant delete refuses when an agent OF THE SAME tenant
    /// scope still uses the provider.
    #[tokio::test]
    async fn llm_providers_delete_tenant_rejects_when_same_scope_agent_uses_it() {
        let llm = MockLlm::default();
        llm.upsert_tenant_provider_field(
            "acme",
            "minimax",
            "base_url",
            Value::String("https://acme".into()),
        )
        .unwrap();
        let agents = MockAgents::default();
        // Seed an agent in tenant `acme` using minimax.
        agents
            .agents
            .lock()
            .unwrap()
            .entry("ana".into())
            .or_default()
            .insert("model.provider".into(), Value::String("minimax".into()));
        agents
            .agents
            .lock()
            .unwrap()
            .entry("ana".into())
            .or_default()
            .insert("tenant_id".into(), Value::String("acme".into()));
        let result = delete(
            &llm,
            &agents,
            serde_json::json!({ "provider_id": "minimax", "tenant_id": "acme" }),
            &|| {},
        );
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
        // Tenant block still present.
        assert_eq!(
            llm.list_tenant_provider_ids("acme").unwrap(),
            vec!["minimax".to_string()]
        );
    }

    /// Cross-scope delete: a global delete does NOT block on
    /// tenant-scoped agents using a same-named tenant
    /// provider. Tenant agent still has its own provider; the
    /// global delete is allowed.
    #[tokio::test]
    async fn llm_providers_delete_global_unaffected_by_tenant_agent_using_same_id() {
        let llm = MockLlm::with(&[("minimax", "u", "K")]);
        llm.upsert_tenant_provider_field(
            "acme",
            "minimax",
            "base_url",
            Value::String("https://acme".into()),
        )
        .unwrap();
        let agents = MockAgents::default();
        // Tenant `acme` agent uses minimax — but the delete
        // targets the GLOBAL minimax, not acme's.
        agents
            .agents
            .lock()
            .unwrap()
            .entry("ana".into())
            .or_default()
            .insert("model.provider".into(), Value::String("minimax".into()));
        agents
            .agents
            .lock()
            .unwrap()
            .entry("ana".into())
            .or_default()
            .insert("tenant_id".into(), Value::String("acme".into()));
        let result = delete(
            &llm,
            &agents,
            // No tenant_id → global scope.
            serde_json::json!({ "provider_id": "minimax" }),
            &|| {},
        );
        let response: LlmProvidersDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.removed);
        // Global minimax gone, tenant minimax intact.
        assert!(llm.list_provider_ids().unwrap().is_empty());
        assert_eq!(
            llm.list_tenant_provider_ids("acme").unwrap(),
            vec!["minimax".to_string()]
        );
    }
}
