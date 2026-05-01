//! Phase 82.10.d — `nexo/admin/credentials/*` handlers.
//!
//! Many-to-many: 1 channel credential → N agents. The credential
//! payload lives in a per-`(channel, instance)` filesystem entry
//! abstracted via [`CredentialStore`]; the agent-side bindings
//! live in `agents.yaml.<id>.inbound_bindings` and are mutated
//! via the existing [`super::agents::YamlPatcher`] trait.
//!
//! Production wires:
//! - `CredentialStore`: filesystem adapter under
//!   `secrets/<channel>/<instance>/`.
//! - `YamlPatcher`: same adapter as `agents` domain (wraps
//!   `nexo_setup::yaml_patch::*`).

use serde_json::Value;

use nexo_tool_meta::admin::credentials::{
    CredentialRegisterInput, CredentialSummary, CredentialsListFilter, CredentialsListResponse,
    CredentialsRevokeParams, CredentialsRevokeResponse,
};

use super::agents::YamlPatcher;
use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Filesystem-side credential store. Production impl writes
/// under `secrets/<channel>/<instance>/`. Tests use an in-memory
/// HashMap.
pub trait CredentialStore: Send + Sync {
    /// List every credential currently stored. Stable ordering
    /// is the caller's responsibility.
    fn list_credentials(&self) -> anyhow::Result<Vec<(String, Option<String>)>>;
    /// Atomic write of the credential payload. Overwrites if
    /// `(channel, instance)` already exists.
    fn write_credential(
        &self,
        channel: &str,
        instance: Option<&str>,
        payload: &Value,
    ) -> anyhow::Result<()>;
    /// Idempotent delete. Returns `true` when an entry was
    /// removed; `false` when it didn't exist.
    fn delete_credential(
        &self,
        channel: &str,
        instance: Option<&str>,
    ) -> anyhow::Result<bool>;
}

/// `nexo/admin/credentials/list` — return all credentials with
/// the agent ids currently bound to each.
pub fn list(
    store: &dyn CredentialStore,
    yaml: &dyn YamlPatcher,
    params: Value,
) -> AdminRpcResult {
    let filter: CredentialsListFilter = serde_json::from_value(params).unwrap_or_default();

    let creds = match store.list_credentials() {
        Ok(c) => c,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "credential store read: {e}"
            )));
        }
    };

    let agent_ids = match yaml.list_agent_ids() {
        Ok(a) => a,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml read: {e}"
            )));
        }
    };

    // Build (channel, instance) → Vec<agent_id> reverse index by
    // walking each agent's inbound_bindings.
    let mut summaries: Vec<CredentialSummary> = creds
        .into_iter()
        .filter(|(c, _)| match &filter.channel_filter {
            Some(f) => c == f,
            None => true,
        })
        .map(|(channel, instance)| {
            let mut agents_using: Vec<String> = agent_ids
                .iter()
                .filter(|aid| agent_has_binding(yaml, aid, &channel, instance.as_deref()))
                .cloned()
                .collect();
            agents_using.sort();
            CredentialSummary {
                channel,
                instance,
                agent_ids: agents_using,
            }
        })
        .collect();
    // Stable order: alpha by channel, then instance.
    summaries.sort_by(|a, b| {
        a.channel
            .cmp(&b.channel)
            .then_with(|| a.instance.cmp(&b.instance))
    });

    let response = CredentialsListResponse {
        credentials: summaries,
    };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/credentials/register` — write the credential
/// payload to the filesystem and append the binding to each
/// agent in `agent_ids` (idempotent — duplicate bindings skipped).
pub fn register(
    store: &dyn CredentialStore,
    yaml: &dyn YamlPatcher,
    params: Value,
    reload_signal: &dyn Fn(),
) -> AdminRpcResult {
    let input: CredentialRegisterInput = match serde_json::from_value(params) {
        Ok(i) => i,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };

    if input.channel.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("channel is empty".into()));
    }

    if let Err(e) =
        store.write_credential(&input.channel, input.instance.as_deref(), &input.payload)
    {
        return AdminRpcResult::err(AdminRpcError::Internal(format!(
            "credential write: {e}"
        )));
    }

    // Append the binding to each requested agent.
    for agent_id in &input.agent_ids {
        if let Err(e) = bind_agent(yaml, agent_id, &input.channel, input.instance.as_deref()) {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "agent `{agent_id}` bind failed: {e}"
            )));
        }
    }

    if !input.agent_ids.is_empty() {
        reload_signal();
    }

    let summary = CredentialSummary {
        channel: input.channel,
        instance: input.instance,
        agent_ids: input.agent_ids,
    };
    AdminRpcResult::ok(serde_json::to_value(summary).unwrap_or(Value::Null))
}

/// `nexo/admin/credentials/revoke` — delete the filesystem entry
/// AND remove the binding from every agent that was using it.
pub fn revoke(
    store: &dyn CredentialStore,
    yaml: &dyn YamlPatcher,
    params: Value,
    reload_signal: &dyn Fn(),
) -> AdminRpcResult {
    let p: CredentialsRevokeParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };

    let agent_ids = match yaml.list_agent_ids() {
        Ok(a) => a,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml read: {e}"
            )));
        }
    };

    let mut unbound = Vec::new();
    for agent_id in &agent_ids {
        if !agent_has_binding(yaml, agent_id, &p.channel, p.instance.as_deref()) {
            continue;
        }
        if let Err(e) = unbind_agent(yaml, agent_id, &p.channel, p.instance.as_deref()) {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "agent `{agent_id}` unbind failed: {e}"
            )));
        }
        unbound.push(agent_id.clone());
    }

    let removed = match store.delete_credential(&p.channel, p.instance.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "credential delete: {e}"
            )));
        }
    };

    if removed || !unbound.is_empty() {
        reload_signal();
    }

    let response = CredentialsRevokeResponse {
        removed,
        unbound_agents: unbound,
    };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

// ── Internal helpers ────────────────────────────────────────────

fn agent_has_binding(
    yaml: &dyn YamlPatcher,
    agent_id: &str,
    channel: &str,
    instance: Option<&str>,
) -> bool {
    let Some(Value::Array(bindings)) = yaml
        .read_agent_field(agent_id, "inbound_bindings")
        .ok()
        .flatten()
    else {
        return false;
    };
    bindings.iter().any(|b| binding_matches(b, channel, instance))
}

fn binding_matches(b: &Value, channel: &str, instance: Option<&str>) -> bool {
    let Some(plugin) = b.get("plugin").and_then(Value::as_str) else {
        return false;
    };
    if plugin != channel {
        return false;
    }
    let bind_instance = b.get("instance").and_then(Value::as_str);
    bind_instance == instance
}

fn bind_agent(
    yaml: &dyn YamlPatcher,
    agent_id: &str,
    channel: &str,
    instance: Option<&str>,
) -> anyhow::Result<()> {
    let mut bindings: Vec<Value> = match yaml.read_agent_field(agent_id, "inbound_bindings")? {
        Some(Value::Array(arr)) => arr,
        _ => Vec::new(),
    };
    if bindings.iter().any(|b| binding_matches(b, channel, instance)) {
        return Ok(());
    }
    let mut new_binding = serde_json::Map::new();
    new_binding.insert("plugin".into(), Value::String(channel.into()));
    if let Some(i) = instance {
        new_binding.insert("instance".into(), Value::String(i.into()));
    }
    bindings.push(Value::Object(new_binding));
    yaml.upsert_agent_field(agent_id, "inbound_bindings", Value::Array(bindings))?;
    Ok(())
}

fn unbind_agent(
    yaml: &dyn YamlPatcher,
    agent_id: &str,
    channel: &str,
    instance: Option<&str>,
) -> anyhow::Result<()> {
    let bindings: Vec<Value> = match yaml.read_agent_field(agent_id, "inbound_bindings")? {
        Some(Value::Array(arr)) => arr,
        _ => return Ok(()),
    };
    let filtered: Vec<Value> = bindings
        .into_iter()
        .filter(|b| !binding_matches(b, channel, instance))
        .collect();
    yaml.upsert_agent_field(agent_id, "inbound_bindings", Value::Array(filtered))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// In-memory `CredentialStore` for tests.
    #[derive(Debug, Default)]
    struct MockStore {
        creds: Mutex<HashMap<(String, Option<String>), Value>>,
    }

    impl CredentialStore for MockStore {
        fn list_credentials(&self) -> anyhow::Result<Vec<(String, Option<String>)>> {
            Ok(self.creds.lock().unwrap().keys().cloned().collect())
        }
        fn write_credential(
            &self,
            channel: &str,
            instance: Option<&str>,
            payload: &Value,
        ) -> anyhow::Result<()> {
            self.creds.lock().unwrap().insert(
                (channel.to_string(), instance.map(String::from)),
                payload.clone(),
            );
            Ok(())
        }
        fn delete_credential(
            &self,
            channel: &str,
            instance: Option<&str>,
        ) -> anyhow::Result<bool> {
            Ok(self
                .creds
                .lock()
                .unwrap()
                .remove(&(channel.to_string(), instance.map(String::from)))
                .is_some())
        }
    }

    /// In-memory `YamlPatcher` mirroring agents-domain test pattern.
    #[derive(Debug, Default)]
    struct MockYaml {
        agents: Mutex<HashMap<String, HashMap<String, Value>>>,
    }

    impl MockYaml {
        fn with_agents(ids: &[&str]) -> Arc<Self> {
            let me = Arc::new(Self::default());
            for id in ids {
                me.set(id, "model.provider", Value::String("minimax".into()));
                me.set(id, "inbound_bindings", Value::Array(vec![]));
            }
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
        fn bindings(&self, agent_id: &str) -> Vec<Value> {
            self.agents
                .lock()
                .unwrap()
                .get(agent_id)
                .and_then(|m| m.get("inbound_bindings").cloned())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
        }
    }

    impl YamlPatcher for MockYaml {
        fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> {
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
    fn credentials_register_links_many_agents() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana", "carlos"]);
        let (count, reload) = reload_counter();

        let result = register(
            &*store,
            &*yaml,
            serde_json::json!({
                "channel": "whatsapp",
                "instance": "shared",
                "agent_ids": ["ana", "carlos"],
                "payload": { "token": "wa.X" }
            }),
            &reload,
        );
        let summary: CredentialSummary =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(summary.agent_ids, vec!["ana".to_string(), "carlos".into()]);

        // Both agents now carry the binding.
        for agent in ["ana", "carlos"] {
            let bindings = yaml.bindings(agent);
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0]["plugin"], "whatsapp");
            assert_eq!(bindings[0]["instance"], "shared");
        }
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn credentials_register_idempotent_skips_duplicate_binding() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (_count, reload) = reload_counter();

        let params = serde_json::json!({
            "channel": "whatsapp",
            "instance": "personal",
            "agent_ids": ["ana"],
            "payload": { "token": "v1" }
        });
        register(&*store, &*yaml, params.clone(), &reload);
        // Second call with same shape should not duplicate.
        register(&*store, &*yaml, params, &reload);

        let bindings = yaml.bindings("ana");
        assert_eq!(bindings.len(), 1);
    }

    #[test]
    fn credentials_register_with_empty_agent_ids_writes_store_only() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (count, reload) = reload_counter();

        register(
            &*store,
            &*yaml,
            serde_json::json!({
                "channel": "whatsapp",
                "agent_ids": [],
                "payload": { "x": 1 }
            }),
            &reload,
        );

        assert_eq!(yaml.bindings("ana").len(), 0);
        // No reload signal when no agents bound.
        assert_eq!(count.load(Ordering::Relaxed), 0);
        // Credential exists on disk.
        assert_eq!(store.list_credentials().unwrap().len(), 1);
    }

    #[test]
    fn credentials_revoke_removes_bindings_from_all_agents() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana", "carlos", "bob"]);
        let (_, reload) = reload_counter();

        // Pre-register: ana + carlos use shared, bob uses personal.
        register(
            &*store,
            &*yaml,
            serde_json::json!({
                "channel": "whatsapp",
                "instance": "shared",
                "agent_ids": ["ana", "carlos"],
                "payload": {}
            }),
            &reload,
        );
        register(
            &*store,
            &*yaml,
            serde_json::json!({
                "channel": "whatsapp",
                "instance": "personal",
                "agent_ids": ["bob"],
                "payload": {}
            }),
            &reload,
        );

        // Revoke shared.
        let result = revoke(
            &*store,
            &*yaml,
            serde_json::json!({ "channel": "whatsapp", "instance": "shared" }),
            &reload,
        );
        let response: CredentialsRevokeResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.removed);
        let mut unbound = response.unbound_agents.clone();
        unbound.sort();
        assert_eq!(unbound, vec!["ana".to_string(), "carlos".into()]);

        // Bob's binding intact.
        assert_eq!(yaml.bindings("bob").len(), 1);
        // ana + carlos have no bindings now.
        assert_eq!(yaml.bindings("ana").len(), 0);
        assert_eq!(yaml.bindings("carlos").len(), 0);
    }

    #[test]
    fn credentials_revoke_unknown_credential_is_idempotent() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (_, reload) = reload_counter();

        let result = revoke(
            &*store,
            &*yaml,
            serde_json::json!({ "channel": "whatsapp", "instance": "ghost" }),
            &reload,
        );
        let response: CredentialsRevokeResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(!response.removed);
        assert!(response.unbound_agents.is_empty());
    }

    #[test]
    fn credentials_list_includes_only_filesystem_creds_with_agent_index() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana", "carlos"]);
        let (_, reload) = reload_counter();

        // Register one shared cred bound to both agents.
        register(
            &*store,
            &*yaml,
            serde_json::json!({
                "channel": "whatsapp",
                "instance": "shared",
                "agent_ids": ["ana", "carlos"],
                "payload": {}
            }),
            &reload,
        );
        // And one orphan cred (no agents bound).
        register(
            &*store,
            &*yaml,
            serde_json::json!({
                "channel": "whatsapp",
                "instance": "orphan",
                "agent_ids": [],
                "payload": {}
            }),
            &reload,
        );

        let result = list(&*store, &*yaml, Value::Null);
        let response: CredentialsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.credentials.len(), 2);

        let shared = response.credentials.iter().find(|c| {
            c.instance.as_deref() == Some("shared")
        }).unwrap();
        let mut shared_agents = shared.agent_ids.clone();
        shared_agents.sort();
        assert_eq!(shared_agents, vec!["ana".to_string(), "carlos".into()]);

        let orphan = response.credentials.iter().find(|c| {
            c.instance.as_deref() == Some("orphan")
        }).unwrap();
        assert!(orphan.agent_ids.is_empty());
    }

    #[test]
    fn credentials_list_channel_filter() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (_, reload) = reload_counter();

        register(
            &*store,
            &*yaml,
            serde_json::json!({
                "channel": "whatsapp",
                "agent_ids": ["ana"],
                "payload": {}
            }),
            &reload,
        );
        register(
            &*store,
            &*yaml,
            serde_json::json!({
                "channel": "future_telegram",
                "agent_ids": [],
                "payload": {}
            }),
            &reload,
        );

        let result = list(
            &*store,
            &*yaml,
            serde_json::json!({ "channel_filter": "whatsapp" }),
        );
        let response: CredentialsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.credentials.len(), 1);
        assert_eq!(response.credentials[0].channel, "whatsapp");
    }

    #[test]
    fn credentials_register_empty_channel_returns_invalid_params() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&[]);
        let (_, reload) = reload_counter();

        let result = register(
            &*store,
            &*yaml,
            serde_json::json!({
                "channel": "",
                "agent_ids": [],
                "payload": {}
            }),
            &reload,
        );
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }
}
