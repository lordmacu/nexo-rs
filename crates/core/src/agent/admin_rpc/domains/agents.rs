//! Phase 82.10.c — `nexo/admin/agents/*` handlers.
//!
//! Yaml mutation is delegated to a [`YamlPatcher`] trait so this
//! crate stays cycle-free vs `nexo-setup` (which holds the
//! concrete `yaml_patch` impl + already depends on `nexo-core`).
//! Production wiring in `src/main.rs` constructs an adapter that
//! forwards each method to `nexo_setup::yaml_patch::*`.
//!
//! After successful mutation the handler calls the dispatcher's
//! reload signal to trigger Phase 18 hot-reload so the running
//! runtime picks up the change without restart.

use serde_json::Value;

use nexo_tool_meta::admin::agents::{
    AgentDetail, AgentSummary, AgentUpsertInput, AgentsDeleteParams, AgentsDeleteResponse,
    AgentsGetParams, AgentsListFilter, AgentsListResponse, BindingSummary, ModelRef,
};

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Phase 82.10.c — yaml mutation surface the agents domain
/// handlers consume. Production impl wraps
/// `nexo_setup::yaml_patch`. Tests provide an in-memory mock.
pub trait YamlPatcher: Send + Sync {
    /// List every `agents.yaml.<id>` block in source order.
    fn list_agent_ids(&self) -> anyhow::Result<Vec<String>>;
    /// Read one dotted field (`model.provider`,
    /// `inbound_bindings`, …). `None` when the field is absent.
    fn read_agent_field(
        &self,
        agent_id: &str,
        dotted: &str,
    ) -> anyhow::Result<Option<Value>>;
    /// Upsert one dotted field. Atomic via temp+rename in the
    /// production impl.
    fn upsert_agent_field(
        &self,
        agent_id: &str,
        dotted: &str,
        value: Value,
    ) -> anyhow::Result<()>;
    /// Remove the entire `agents.yaml.<id>` block.
    fn remove_agent(&self, agent_id: &str) -> anyhow::Result<()>;
}

/// `nexo/admin/agents/list` — return agents matching `filter`.
pub fn list(patcher: &dyn YamlPatcher, params: Value) -> AdminRpcResult {
    let filter: AgentsListFilter = parse_or_default(params);
    let ids = match patcher.list_agent_ids() {
        Ok(ids) => ids,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml read failed: {e}"
            )));
        }
    };

    let mut summaries: Vec<AgentSummary> = ids
        .into_iter()
        .filter_map(|id| read_summary(patcher, &id).ok().flatten())
        .filter(|s| !filter.active_only || s.active)
        .filter(|s| match &filter.plugin_filter {
            Some(p) => has_plugin_binding(patcher, &s.id, p),
            None => true,
        })
        .filter(|s| match &filter.tenant_id {
            // Phase 83.8.12 — multi-tenant filter. Defense-in-
            // depth: an agent without `tenant_id` is treated
            // as `None` and filtered out when caller requests
            // a specific tenant. Cross-tenant returns empty
            // (no leak of existence).
            Some(want) => agent_tenant_id(patcher, &s.id)
                .as_deref()
                .map(|got| got == want)
                .unwrap_or(false),
            None => true,
        })
        .collect();
    // Stable alpha order — operator UIs rely on it for diff
    // displays.
    summaries.sort_by(|a, b| a.id.cmp(&b.id));

    let response = AgentsListResponse { agents: summaries };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/agents/get` — return full detail for one agent.
pub fn get(patcher: &dyn YamlPatcher, params: Value) -> AdminRpcResult {
    let p: AgentsGetParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string()));
        }
    };
    match read_detail(patcher, &p.agent_id) {
        Ok(Some(detail)) => AdminRpcResult::ok(serde_json::to_value(detail).unwrap_or(Value::Null)),
        Ok(None) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "not_found: agent `{}` not in yaml",
            p.agent_id
        ))),
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "yaml read failed: {e}"
        ))),
    }
}

/// `nexo/admin/agents/upsert` — create or update an agent block.
pub fn upsert(
    patcher: &dyn YamlPatcher,
    params: Value,
    reload_signal: &dyn Fn(),
) -> AdminRpcResult {
    let input: AgentUpsertInput = match serde_json::from_value(params) {
        Ok(i) => i,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string()));
        }
    };

    if let Err(e) = upsert_yaml(patcher, &input) {
        return AdminRpcResult::err(AdminRpcError::Internal(format!(
            "yaml write failed: {e}"
        )));
    }
    reload_signal();

    match read_detail(patcher, &input.id) {
        Ok(Some(d)) => AdminRpcResult::ok(serde_json::to_value(d).unwrap_or(Value::Null)),
        Ok(None) => AdminRpcResult::err(AdminRpcError::Internal(
            "post-upsert read returned None".into(),
        )),
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "post-upsert read failed: {e}"
        ))),
    }
}

/// `nexo/admin/agents/delete` — soft-delete then remove yaml block.
pub fn delete(
    patcher: &dyn YamlPatcher,
    params: Value,
    reload_signal: &dyn Fn(),
) -> AdminRpcResult {
    let p: AgentsDeleteParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string()));
        }
    };

    let existed = matches!(
        patcher.list_agent_ids(),
        Ok(ids) if ids.iter().any(|id| id == &p.agent_id)
    );
    if !existed {
        // Idempotent — return removed=false, NOT an error.
        return AdminRpcResult::ok(
            serde_json::to_value(AgentsDeleteResponse { removed: false })
                .unwrap_or(Value::Null),
        );
    }

    match patcher.remove_agent(&p.agent_id) {
        Ok(()) => {
            reload_signal();
            AdminRpcResult::ok(
                serde_json::to_value(AgentsDeleteResponse { removed: true })
                    .unwrap_or(Value::Null),
            )
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "yaml remove failed: {e}"
        ))),
    }
}

// ── Internal helpers ────────────────────────────────────────────

fn parse_or_default<T: for<'de> serde::Deserialize<'de> + Default>(v: Value) -> T {
    serde_json::from_value(v).unwrap_or_default()
}

/// Phase 83.8.12 — read `agents.yaml.<id>.tenant_id`. Returns
/// `None` for legacy agents (no field) and on any read error
/// (defense-in-depth: fail closed for cross-tenant filters).
fn agent_tenant_id(patcher: &dyn YamlPatcher, agent_id: &str) -> Option<String> {
    match patcher.read_agent_field(agent_id, "tenant_id").ok().flatten() {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

fn read_summary(
    patcher: &dyn YamlPatcher,
    agent_id: &str,
) -> anyhow::Result<Option<AgentSummary>> {
    let provider = match patcher.read_agent_field(agent_id, "model.provider")? {
        Some(Value::String(s)) => s,
        _ => return Ok(None),
    };
    let active = match patcher.read_agent_field(agent_id, "active")? {
        Some(Value::Bool(b)) => b,
        // Default active=true when field absent (legacy yaml).
        _ => true,
    };
    let bindings_count = patcher
        .read_agent_field(agent_id, "inbound_bindings")?
        .and_then(|v| v.as_array().map(|a| a.len()))
        .unwrap_or(0);

    Ok(Some(AgentSummary {
        id: agent_id.to_string(),
        active,
        model_provider: provider,
        bindings_count,
    }))
}

fn has_plugin_binding(patcher: &dyn YamlPatcher, agent_id: &str, plugin: &str) -> bool {
    let Some(Value::Array(bindings)) = patcher
        .read_agent_field(agent_id, "inbound_bindings")
        .ok()
        .flatten()
    else {
        return false;
    };
    bindings.iter().any(|b| {
        b.get("plugin")
            .and_then(Value::as_str)
            .is_some_and(|p| p == plugin)
    })
}

fn read_detail(
    patcher: &dyn YamlPatcher,
    agent_id: &str,
) -> anyhow::Result<Option<AgentDetail>> {
    let Some(Value::String(provider)) = patcher.read_agent_field(agent_id, "model.provider")?
    else {
        return Ok(None);
    };
    let model = match patcher.read_agent_field(agent_id, "model.model")? {
        Some(Value::String(s)) => s,
        _ => String::new(),
    };
    let active = match patcher.read_agent_field(agent_id, "active")? {
        Some(Value::Bool(b)) => b,
        _ => true,
    };
    let allowed_tools: Vec<String> = match patcher.read_agent_field(agent_id, "allowed_tools")? {
        Some(Value::Array(arr)) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    let system_prompt = match patcher.read_agent_field(agent_id, "system_prompt")? {
        Some(Value::String(s)) => s,
        _ => String::new(),
    };
    let language = match patcher.read_agent_field(agent_id, "language")? {
        Some(Value::String(s)) => Some(s),
        _ => None,
    };
    let inbound_bindings: Vec<BindingSummary> =
        match patcher.read_agent_field(agent_id, "inbound_bindings")? {
            Some(Value::Array(arr)) => arr
                .into_iter()
                .filter_map(|b| {
                    let plugin = b.get("plugin")?.as_str()?.to_string();
                    let instance = b
                        .get("instance")
                        .and_then(Value::as_str)
                        .map(String::from);
                    Some(BindingSummary { plugin, instance })
                })
                .collect(),
            _ => Vec::new(),
        };

    Ok(Some(AgentDetail {
        id: agent_id.to_string(),
        model: ModelRef { provider, model },
        active,
        allowed_tools,
        inbound_bindings,
        system_prompt,
        language,
    }))
}

fn upsert_yaml(patcher: &dyn YamlPatcher, input: &AgentUpsertInput) -> anyhow::Result<()> {
    patcher.upsert_agent_field(
        &input.id,
        "model.provider",
        Value::String(input.model.provider.clone()),
    )?;
    patcher.upsert_agent_field(
        &input.id,
        "model.model",
        Value::String(input.model.model.clone()),
    )?;
    if let Some(active) = input.active {
        patcher.upsert_agent_field(&input.id, "active", Value::Bool(active))?;
    }
    if let Some(tools) = &input.allowed_tools {
        let arr = Value::Array(tools.iter().map(|s| Value::String(s.clone())).collect());
        patcher.upsert_agent_field(&input.id, "allowed_tools", arr)?;
    }
    if let Some(prompt) = &input.system_prompt {
        patcher.upsert_agent_field(
            &input.id,
            "system_prompt",
            Value::String(prompt.clone()),
        )?;
    }
    if let Some(language) = &input.language {
        patcher.upsert_agent_field(
            &input.id,
            "language",
            Value::String(language.clone()),
        )?;
    }
    if let Some(bindings) = &input.inbound_bindings {
        let arr: Vec<Value> = bindings
            .iter()
            .map(|b| {
                let mut map = serde_json::Map::new();
                map.insert("plugin".into(), Value::String(b.plugin.clone()));
                if let Some(i) = &b.instance {
                    map.insert("instance".into(), Value::String(i.clone()));
                }
                Value::Object(map)
            })
            .collect();
        patcher.upsert_agent_field(&input.id, "inbound_bindings", Value::Array(arr))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Test-only in-memory `YamlPatcher`. Stores agents as
    /// `agent_id → field_path → Value`. Sufficient for the
    /// admin-domain tests; real yaml-on-disk semantics
    /// (atomicity, ordering) are covered by `nexo-setup`'s own
    /// tests for `yaml_patch::*`.
    #[derive(Debug, Default)]
    struct MockYaml {
        agents: Mutex<HashMap<String, HashMap<String, Value>>>,
    }

    impl MockYaml {
        fn with_fixture() -> Arc<Self> {
            let me = Arc::new(Self::default());
            // ana: active, whatsapp:personal, system_prompt, language=es
            me.set("ana", "model.provider", Value::String("minimax".into()));
            me.set("ana", "model.model", Value::String("MiniMax-M2.5".into()));
            me.set("ana", "active", Value::Bool(true));
            me.set(
                "ana",
                "allowed_tools",
                Value::Array(vec![Value::String("*".into())]),
            );
            me.set(
                "ana",
                "inbound_bindings",
                serde_json::json!([{ "plugin": "whatsapp", "instance": "personal" }]),
            );
            me.set(
                "ana",
                "system_prompt",
                Value::String("You are Ana.".into()),
            );
            me.set("ana", "language", Value::String("es".into()));
            // bob: inactive, no bindings
            me.set("bob", "model.provider", Value::String("anthropic".into()));
            me.set(
                "bob",
                "model.model",
                Value::String("claude-opus-4-7".into()),
            );
            me.set("bob", "active", Value::Bool(false));
            me.set("bob", "inbound_bindings", Value::Array(vec![]));
            me
        }

        fn set(&self, agent_id: &str, dotted: &str, value: Value) {
            self.agents
                .lock()
                .unwrap()
                .entry(agent_id.to_string())
                .or_default()
                .insert(dotted.to_string(), value);
        }
    }

    impl YamlPatcher for MockYaml {
        fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> {
            // Source order = insertion order. Use a stable
            // alphabetic sort here so test fixture order matches
            // production yaml read order (which preserves source
            // order for `mapping_for_agent`).
            let mut ids: Vec<String> = self.agents.lock().unwrap().keys().cloned().collect();
            ids.sort();
            Ok(ids)
        }

        fn read_agent_field(
            &self,
            agent_id: &str,
            dotted: &str,
        ) -> anyhow::Result<Option<Value>> {
            Ok(self
                .agents
                .lock()
                .unwrap()
                .get(agent_id)
                .and_then(|m| m.get(dotted).cloned()))
        }

        fn upsert_agent_field(
            &self,
            agent_id: &str,
            dotted: &str,
            value: Value,
        ) -> anyhow::Result<()> {
            self.set(agent_id, dotted, value);
            Ok(())
        }

        fn remove_agent(&self, agent_id: &str) -> anyhow::Result<()> {
            self.agents.lock().unwrap().remove(agent_id);
            Ok(())
        }
    }

    fn reload_counter() -> (Arc<AtomicUsize>, impl Fn()) {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        (count, move || {
            counter.fetch_add(1, Ordering::Relaxed);
        })
    }

    #[test]
    fn agents_list_returns_yaml_summary_in_alpha_order() {
        let yaml = MockYaml::with_fixture();
        let result = list(&*yaml, Value::Null);
        let response: AgentsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.agents.len(), 2);
        assert_eq!(response.agents[0].id, "ana");
        assert!(response.agents[0].active);
        assert_eq!(response.agents[0].model_provider, "minimax");
        assert_eq!(response.agents[0].bindings_count, 1);
        assert_eq!(response.agents[1].id, "bob");
        assert!(!response.agents[1].active);
    }

    #[test]
    fn agents_list_active_only_filters_inactive() {
        let yaml = MockYaml::with_fixture();
        let result = list(&*yaml, serde_json::json!({ "active_only": true }));
        let response: AgentsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.agents.len(), 1);
        assert_eq!(response.agents[0].id, "ana");
    }

    #[test]
    fn agents_list_plugin_filter_only_returns_matching_bindings() {
        let yaml = MockYaml::with_fixture();
        let result = list(&*yaml, serde_json::json!({ "plugin_filter": "whatsapp" }));
        let response: AgentsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.agents.len(), 1);
        assert_eq!(response.agents[0].id, "ana");
    }

    #[test]
    fn agents_get_returns_full_detail() {
        let yaml = MockYaml::with_fixture();
        let result = get(&*yaml, serde_json::json!({ "agent_id": "ana" }));
        let detail: AgentDetail = serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(detail.id, "ana");
        assert_eq!(detail.model.provider, "minimax");
        assert_eq!(detail.allowed_tools, vec!["*".to_string()]);
        assert_eq!(detail.inbound_bindings.len(), 1);
        assert_eq!(detail.inbound_bindings[0].plugin, "whatsapp");
        assert_eq!(detail.language.as_deref(), Some("es"));
    }

    #[test]
    fn agents_get_returns_not_found_for_unknown_id() {
        let yaml = MockYaml::with_fixture();
        let result = get(&*yaml, serde_json::json!({ "agent_id": "ghost" }));
        let err = result.error.expect("error");
        match err {
            AdminRpcError::Internal(m) => assert!(m.contains("not_found")),
            other => panic!("expected Internal/not_found, got {other:?}"),
        }
    }

    #[test]
    fn agents_upsert_writes_yaml_and_triggers_reload() {
        let yaml = MockYaml::with_fixture();
        let (count, reload) = reload_counter();
        let input = AgentUpsertInput {
            id: "ana".into(),
            model: ModelRef {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            },
            active: None,
            allowed_tools: None,
            inbound_bindings: None,
            system_prompt: None,
            language: Some("en".into()),
        };
        let result = upsert(
            &*yaml,
            serde_json::to_value(&input).unwrap(),
            &reload,
        );
        let detail: AgentDetail = serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(detail.language.as_deref(), Some("en"));
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn agents_delete_removes_yaml_block_and_triggers_reload() {
        let yaml = MockYaml::with_fixture();
        let (count, reload) = reload_counter();
        let result = delete(
            &*yaml,
            serde_json::json!({ "agent_id": "bob" }),
            &reload,
        );
        let response: AgentsDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.removed);
        assert_eq!(count.load(Ordering::Relaxed), 1);

        let listed = list(&*yaml, Value::Null);
        let listed_response: AgentsListResponse =
            serde_json::from_value(listed.result.unwrap()).unwrap();
        assert_eq!(listed_response.agents.len(), 1);
        assert_eq!(listed_response.agents[0].id, "ana");
    }

    #[test]
    fn agents_delete_unknown_id_is_idempotent() {
        let yaml = MockYaml::with_fixture();
        let (count, reload) = reload_counter();
        let result = delete(
            &*yaml,
            serde_json::json!({ "agent_id": "ghost" }),
            &reload,
        );
        let response: AgentsDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(!response.removed);
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }
}
