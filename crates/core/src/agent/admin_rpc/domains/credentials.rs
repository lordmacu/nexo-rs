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
//!
//! Phase 82.10.n bridges the opaque [`CredentialStore`] write to
//! channel-specific runtime state (telegram.yaml accounts list,
//! email.yaml accounts list, …) via the
//! [`ChannelCredentialPersister`] trait. Channel plugins
//! implement the trait and register their persister at boot; the
//! dispatcher routes per `channel` field. Channels without a
//! registered persister fall back to opaque-only behavior
//! (back-compat with the pre-82.10.n flow).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use nexo_tool_meta::admin::credentials::{
    CredentialRegisterInput, CredentialRegisterResponse, CredentialSummary,
    CredentialValidationOutcome, CredentialsListFilter, CredentialsListResponse,
    CredentialsRevokeParams, CredentialsRevokeResponse,
};

use super::agents::YamlPatcher;
use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Phase 82.10.n — per-channel bridge from opaque admin-RPC
/// credential payloads to plugin-side runtime state (yaml
/// accounts list, secret file under `secrets/<…>/`, in-memory
/// store). Channel plugins implement this trait and register at
/// boot via [`crate::agent::admin_rpc::dispatcher::AdminRpcDispatcher::register_persister`].
///
/// Lifecycle inside `credentials/register`:
/// 1. [`Self::validate_shape`] — synchronous, network-free,
///    rejects bad shapes before anything touches disk.
/// 2. Opaque write (existing [`CredentialStore::write_credential`]).
/// 3. [`Self::persist`] — writes plugin-side runtime state.
/// 4. Agent bindings + reload signal (existing).
/// 5. [`Self::probe`] — best-effort connectivity check; result
///    flows back to the operator UI in
///    [`nexo_tool_meta::admin::credentials::CredentialRegisterResponse::validation`].
///
/// Lifecycle inside `credentials/revoke`: agent unbind →
/// [`Self::revoke`] (so persister can read the opaque blob if
/// needed) → opaque delete → reload signal.
#[async_trait]
pub trait ChannelCredentialPersister: Send + Sync {
    /// Channel id this persister handles (`"telegram"`,
    /// `"email"`, `"whatsapp"`, …). Must be unique across the
    /// dispatcher's persister registry — duplicate registration
    /// panics at boot wire-up time.
    fn channel(&self) -> &str;

    /// Validate `payload` + `metadata` shape WITHOUT touching the
    /// network or filesystem. Called before [`Self::persist`].
    /// Returning [`AdminRpcError::InvalidParams`] aborts
    /// `credentials/register` before the opaque write.
    fn validate_shape(
        &self,
        payload: &Value,
        metadata: &HashMap<String, Value>,
    ) -> Result<(), AdminRpcError>;

    /// Write channel-specific runtime state (yaml accounts list,
    /// secret file, in-memory entry). Called after the opaque
    /// blob lands on disk so the persister can read it back if
    /// it needs to (e.g. emit a `${file:./secrets/...}` ref into
    /// a yaml).
    ///
    /// Idempotent: re-registering the same `(channel, instance)`
    /// updates in place. Errors return [`AdminRpcError::Internal`]
    /// — the opaque blob remains so the operator can retry.
    async fn persist(
        &self,
        instance: Option<&str>,
        payload: &Value,
        metadata: &HashMap<String, Value>,
    ) -> Result<(), AdminRpcError>;

    /// Inverse of [`Self::persist`]. Idempotent — instance not
    /// present returns `Ok(false)`. Called BEFORE the opaque
    /// delete so the persister can still read the blob if it
    /// needs to.
    async fn revoke(&self, instance: Option<&str>) -> Result<bool, AdminRpcError>;

    /// Optional connectivity probe (telegram `getMe`, email IMAP
    /// `LOGIN + NOOP`). Default returns
    /// [`CredentialValidationOutcome::not_probed`] so persisters
    /// can opt out (e.g. whatsapp pairing handles its own probe
    /// flow).
    ///
    /// Errors here do NOT abort the register call — the outcome
    /// flows back to the caller via
    /// [`nexo_tool_meta::admin::credentials::CredentialRegisterResponse::validation`]
    /// and the operator UI surfaces a health badge.
    async fn probe(
        &self,
        _instance: Option<&str>,
        _payload: &Value,
        _metadata: &HashMap<String, Value>,
    ) -> CredentialValidationOutcome {
        CredentialValidationOutcome {
            probed: false,
            healthy: false,
            detail: None,
            reason_code: Some(
                nexo_tool_meta::admin::credentials::reason_code::NOT_PROBED.into(),
            ),
        }
    }
}

/// Phase 82.10.n — registry of per-channel persisters keyed by
/// channel id. Owned by [`crate::agent::admin_rpc::dispatcher::AdminRpcDispatcher`].
/// Uses `BTreeMap`-style insertion via `HashMap` since lookup is
/// always by exact channel id, never iteration in stable order.
pub type PersisterRegistry = HashMap<String, Arc<dyn ChannelCredentialPersister>>;

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
///
/// Phase 82.10.n adds the optional `persister` arg: when present
/// the call sequence becomes
/// `validate_shape → opaque write → persist → bind → reload → probe`.
/// Validation failures abort before the opaque write; persist
/// failures abort after (the opaque blob is left on disk so the
/// operator can retry); probe failures are non-fatal and flow
/// back via [`CredentialRegisterResponse::validation`].
///
/// `persister = None` keeps the pre-82.10.n opaque-only path
/// (validation = `None` in the response).
pub async fn register(
    store: &dyn CredentialStore,
    yaml: &dyn YamlPatcher,
    persister: Option<Arc<dyn ChannelCredentialPersister>>,
    params: Value,
    reload_signal: &(dyn Fn() + Send + Sync),
) -> AdminRpcResult {
    let input: CredentialRegisterInput = match serde_json::from_value(params) {
        Ok(i) => i,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };

    if input.channel.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("channel is empty".into()));
    }

    // Phase 82.10.n step 1 — persister shape validation BEFORE
    // the opaque write so a bad payload never lands on disk.
    if let Some(p) = persister.as_ref() {
        if let Err(e) = p.validate_shape(&input.payload, &input.metadata) {
            return AdminRpcResult::err(e);
        }
    }

    if let Err(e) =
        store.write_credential(&input.channel, input.instance.as_deref(), &input.payload)
    {
        return AdminRpcResult::err(AdminRpcError::Internal(format!(
            "credential write: {e}"
        )));
    }

    // Phase 82.10.n step 2 — bridge to plugin runtime state.
    // On error we leave the opaque blob in place so the operator
    // can retry (idempotent) without losing payload.
    let mut persister_persisted = false;
    if let Some(p) = persister.as_ref() {
        if let Err(e) = p
            .persist(input.instance.as_deref(), &input.payload, &input.metadata)
            .await
        {
            return AdminRpcResult::err(e);
        }
        persister_persisted = true;
    }

    // Append the binding to each requested agent.
    for agent_id in &input.agent_ids {
        if let Err(e) = bind_agent(yaml, agent_id, &input.channel, input.instance.as_deref()) {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "agent `{agent_id}` bind failed: {e}"
            )));
        }
    }

    // Reload once any of: agents bound (yaml inbound_bindings
    // changed) OR persister patched plugin state (yaml accounts
    // list changed). Pre-82.10.n behaviour preserved when both
    // are false.
    if !input.agent_ids.is_empty() || persister_persisted {
        reload_signal();
    }

    // Phase 82.10.n step 3 — best-effort probe. Errors here
    // never propagate — the worst case is `outcome.healthy = false`
    // which the operator UI surfaces as a health badge.
    let validation = match persister.as_ref() {
        Some(p) => Some(
            p.probe(input.instance.as_deref(), &input.payload, &input.metadata)
                .await,
        ),
        None => None,
    };

    let response = CredentialRegisterResponse {
        summary: CredentialSummary {
            channel: input.channel,
            instance: input.instance,
            agent_ids: input.agent_ids,
        },
        validation,
    };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/credentials/revoke` — delete the filesystem entry
/// AND remove the binding from every agent that was using it.
///
/// Phase 82.10.n: when `persister` is present, its `revoke` runs
/// BEFORE the opaque delete so the persister can still read the
/// blob if it needs to (e.g. to look up the secret-file path
/// referenced from a yaml entry). Persister revoke errors abort
/// the call.
pub async fn revoke(
    store: &dyn CredentialStore,
    yaml: &dyn YamlPatcher,
    persister: Option<Arc<dyn ChannelCredentialPersister>>,
    params: Value,
    reload_signal: &(dyn Fn() + Send + Sync),
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

    // Phase 82.10.n — persister revoke before opaque delete.
    let mut persister_revoked = false;
    if let Some(pp) = persister.as_ref() {
        match pp.revoke(p.instance.as_deref()).await {
            Ok(removed_in_persister) => persister_revoked = removed_in_persister,
            Err(e) => return AdminRpcResult::err(e),
        }
    }

    let removed = match store.delete_credential(&p.channel, p.instance.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "credential delete: {e}"
            )));
        }
    };

    if removed || !unbound.is_empty() || persister_revoked {
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

    #[tokio::test]
    async fn credentials_register_links_many_agents() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana", "carlos"]);
        let (count, reload) = reload_counter();

        let result = register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "whatsapp",
                "instance": "shared",
                "agent_ids": ["ana", "carlos"],
                "payload": { "token": "wa.X" }
            }),
            &reload,
        )
        .await;
        let response: CredentialRegisterResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(
            response.summary.agent_ids,
            vec!["ana".to_string(), "carlos".into()]
        );
        // No persister registered for this channel → validation
        // is None on the back-compat path.
        assert!(response.validation.is_none());

        // Both agents now carry the binding.
        for agent in ["ana", "carlos"] {
            let bindings = yaml.bindings(agent);
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0]["plugin"], "whatsapp");
            assert_eq!(bindings[0]["instance"], "shared");
        }
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn credentials_register_idempotent_skips_duplicate_binding() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (_count, reload) = reload_counter();

        let params = serde_json::json!({
            "channel": "whatsapp",
            "instance": "personal",
            "agent_ids": ["ana"],
            "payload": { "token": "v1" }
        });
        register(&*store, &*yaml, None, params.clone(), &reload).await;
        // Second call with same shape should not duplicate.
        register(&*store, &*yaml, None, params, &reload).await;

        let bindings = yaml.bindings("ana");
        assert_eq!(bindings.len(), 1);
    }

    #[tokio::test]
    async fn credentials_register_with_empty_agent_ids_writes_store_only() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (count, reload) = reload_counter();

        register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "whatsapp",
                "agent_ids": [],
                "payload": { "x": 1 }
            }),
            &reload,
        )
        .await;

        assert_eq!(yaml.bindings("ana").len(), 0);
        // No reload signal when no agents bound + no persister
        // (back-compat path).
        assert_eq!(count.load(Ordering::Relaxed), 0);
        // Credential exists on disk.
        assert_eq!(store.list_credentials().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn credentials_revoke_removes_bindings_from_all_agents() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana", "carlos", "bob"]);
        let (_, reload) = reload_counter();

        // Pre-register: ana + carlos use shared, bob uses personal.
        register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "whatsapp",
                "instance": "shared",
                "agent_ids": ["ana", "carlos"],
                "payload": {}
            }),
            &reload,
        )
        .await;
        register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "whatsapp",
                "instance": "personal",
                "agent_ids": ["bob"],
                "payload": {}
            }),
            &reload,
        )
        .await;

        // Revoke shared.
        let result = revoke(
            &*store,
            &*yaml,
            None,
            serde_json::json!({ "channel": "whatsapp", "instance": "shared" }),
            &reload,
        )
        .await;
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

    #[tokio::test]
    async fn credentials_revoke_unknown_credential_is_idempotent() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (_, reload) = reload_counter();

        let result = revoke(
            &*store,
            &*yaml,
            None,
            serde_json::json!({ "channel": "whatsapp", "instance": "ghost" }),
            &reload,
        )
        .await;
        let response: CredentialsRevokeResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(!response.removed);
        assert!(response.unbound_agents.is_empty());
    }

    #[tokio::test]
    async fn credentials_list_includes_only_filesystem_creds_with_agent_index() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana", "carlos"]);
        let (_, reload) = reload_counter();

        // Register one shared cred bound to both agents.
        register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "whatsapp",
                "instance": "shared",
                "agent_ids": ["ana", "carlos"],
                "payload": {}
            }),
            &reload,
        )
        .await;
        // And one orphan cred (no agents bound).
        register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "whatsapp",
                "instance": "orphan",
                "agent_ids": [],
                "payload": {}
            }),
            &reload,
        )
        .await;

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

    #[tokio::test]
    async fn credentials_list_channel_filter() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (_, reload) = reload_counter();

        register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "whatsapp",
                "agent_ids": ["ana"],
                "payload": {}
            }),
            &reload,
        )
        .await;
        register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "future_telegram",
                "agent_ids": [],
                "payload": {}
            }),
            &reload,
        )
        .await;

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

    // ── Phase 82.10.n — ChannelCredentialPersister registry tests ─

    use crate::agent::admin_rpc::dispatcher::AdminRpcDispatcher;
    use nexo_tool_meta::admin::credentials::reason_code;

    /// Mock persister that records call order + lets tests opt
    /// into injected failures via boolean flags. Failures are
    /// constructed inside the trait method (avoids needing
    /// [`AdminRpcError`] to be `Clone`).
    struct MockPersister {
        channel: String,
        calls: Mutex<Vec<String>>,
        fail_validate: bool,
        fail_persist: bool,
        fail_revoke: bool,
        probe_unhealthy: bool,
    }

    impl MockPersister {
        fn new(channel: &str) -> Self {
            Self {
                channel: channel.into(),
                calls: Mutex::new(Vec::new()),
                fail_validate: false,
                fail_persist: false,
                fail_revoke: false,
                probe_unhealthy: false,
            }
        }

        fn record(&self, call: &str) {
            self.calls.lock().unwrap().push(call.into());
        }
    }

    #[async_trait]
    impl ChannelCredentialPersister for MockPersister {
        fn channel(&self) -> &str {
            &self.channel
        }
        fn validate_shape(
            &self,
            _payload: &Value,
            _metadata: &HashMap<String, Value>,
        ) -> Result<(), AdminRpcError> {
            self.record("validate_shape");
            if self.fail_validate {
                return Err(AdminRpcError::InvalidParams("mock validate".into()));
            }
            Ok(())
        }
        async fn persist(
            &self,
            _instance: Option<&str>,
            _payload: &Value,
            _metadata: &HashMap<String, Value>,
        ) -> Result<(), AdminRpcError> {
            self.record("persist");
            if self.fail_persist {
                return Err(AdminRpcError::Internal("mock persist".into()));
            }
            Ok(())
        }
        async fn revoke(&self, _instance: Option<&str>) -> Result<bool, AdminRpcError> {
            self.record("revoke");
            if self.fail_revoke {
                return Err(AdminRpcError::Internal("mock revoke".into()));
            }
            Ok(true)
        }
        async fn probe(
            &self,
            _instance: Option<&str>,
            _payload: &Value,
            _metadata: &HashMap<String, Value>,
        ) -> CredentialValidationOutcome {
            self.record("probe");
            if self.probe_unhealthy {
                return CredentialValidationOutcome {
                    probed: true,
                    healthy: false,
                    detail: Some("mock unhealthy".into()),
                    reason_code: Some(reason_code::CONNECTIVITY_FAILED.into()),
                };
            }
            CredentialValidationOutcome {
                probed: true,
                healthy: true,
                detail: None,
                reason_code: Some(reason_code::OK.into()),
            }
        }
    }

    #[test]
    fn dispatcher_registry_starts_empty() {
        let d = AdminRpcDispatcher::new();
        assert!(d.persister_for("telegram").is_none());
        assert!(d.persister_for("email").is_none());
    }

    #[test]
    fn dispatcher_registry_registers_and_retrieves_persister() {
        let mut d = AdminRpcDispatcher::new();
        let mock = Arc::new(MockPersister::new("telegram"));
        d.register_persister(mock.clone());

        let fetched = d.persister_for("telegram").expect("registered");
        assert_eq!(fetched.channel(), "telegram");
        // Same Arc identity = same instance (Arc::ptr_eq across
        // dyn-trait pointers requires fattening the pointers
        // first, so just probe via a recorded call instead).
        let _ = futures::executor::block_on(fetched.persist(
            None,
            &Value::Null,
            &HashMap::new(),
        ));
        assert_eq!(mock.calls.lock().unwrap().as_slice(), &["persist".to_string()]);
    }

    #[test]
    fn dispatcher_registry_two_distinct_channels_coexist() {
        let mut d = AdminRpcDispatcher::new();
        d.register_persister(Arc::new(MockPersister::new("telegram")));
        d.register_persister(Arc::new(MockPersister::new("email")));

        assert!(d.persister_for("telegram").is_some());
        assert!(d.persister_for("email").is_some());
        assert!(d.persister_for("whatsapp").is_none());
    }

    #[test]
    #[should_panic(expected = "duplicate ChannelCredentialPersister registration")]
    fn dispatcher_registry_panics_on_duplicate_channel() {
        let mut d = AdminRpcDispatcher::new();
        d.register_persister(Arc::new(MockPersister::new("telegram")));
        d.register_persister(Arc::new(MockPersister::new("telegram")));
    }

    #[tokio::test]
    async fn persister_default_probe_returns_not_probed() {
        struct MinimalPersister;
        #[async_trait]
        impl ChannelCredentialPersister for MinimalPersister {
            fn channel(&self) -> &str {
                "minimal"
            }
            fn validate_shape(
                &self,
                _: &Value,
                _: &HashMap<String, Value>,
            ) -> Result<(), AdminRpcError> {
                Ok(())
            }
            async fn persist(
                &self,
                _: Option<&str>,
                _: &Value,
                _: &HashMap<String, Value>,
            ) -> Result<(), AdminRpcError> {
                Ok(())
            }
            async fn revoke(&self, _: Option<&str>) -> Result<bool, AdminRpcError> {
                Ok(false)
            }
            // No probe override — inherits trait default.
        }

        let p = MinimalPersister;
        let outcome = p.probe(None, &Value::Null, &HashMap::new()).await;
        assert!(!outcome.probed);
        assert!(!outcome.healthy);
        assert_eq!(outcome.reason_code.as_deref(), Some(reason_code::NOT_PROBED));
    }

    #[tokio::test]
    async fn credentials_register_empty_channel_returns_invalid_params() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&[]);
        let (_, reload) = reload_counter();

        let result = register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "",
                "agent_ids": [],
                "payload": {}
            }),
            &reload,
        )
        .await;
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    // ── Phase 82.10.n — register/revoke bridge tests ──────────────

    /// Asserts the persister methods fire in the spec's order
    /// `validate_shape → persist → probe` AND that the opaque
    /// store + yaml binder ran (we observe via store.list +
    /// yaml.bindings).
    #[tokio::test]
    async fn register_with_persister_runs_validate_persist_probe_in_order() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (count, reload) = reload_counter();
        let persister = Arc::new(MockPersister::new("telegram"));

        let result = register(
            &*store,
            &*yaml,
            Some(persister.clone()),
            serde_json::json!({
                "channel": "telegram",
                "instance": "kate",
                "agent_ids": ["ana"],
                "payload": { "token": "tg.X" },
                "metadata": { "polling": { "enabled": true } }
            }),
            &reload,
        )
        .await;

        let response: CredentialRegisterResponse =
            serde_json::from_value(result.result.expect("ok")).unwrap();
        assert_eq!(response.summary.channel, "telegram");
        // Persister probe ran → validation present + healthy.
        let v = response.validation.expect("validation present");
        assert!(v.probed && v.healthy);
        assert_eq!(v.reason_code.as_deref(), Some(reason_code::OK));
        // Order: validate_shape, persist, probe (bind happens
        // between persist and probe but the persister doesn't
        // see that).
        let calls = persister.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                "validate_shape".to_string(),
                "persist".into(),
                "probe".into()
            ]
        );
        // Opaque store wrote.
        assert_eq!(store.list_credentials().unwrap().len(), 1);
        // Agent bound + reload fired.
        assert_eq!(yaml.bindings("ana").len(), 1);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    /// `validate_shape` failure aborts BEFORE the opaque write
    /// — the credential never lands on disk.
    #[tokio::test]
    async fn register_aborts_when_validate_shape_fails_no_opaque_write() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (count, reload) = reload_counter();
        let mut p = MockPersister::new("telegram");
        p.fail_validate = true;
        let persister = Arc::new(p);

        let result = register(
            &*store,
            &*yaml,
            Some(persister.clone()),
            serde_json::json!({
                "channel": "telegram",
                "instance": "kate",
                "agent_ids": ["ana"],
                "payload": {}
            }),
            &reload,
        )
        .await;

        assert!(matches!(
            result.error.unwrap(),
            AdminRpcError::InvalidParams(_)
        ));
        // Opaque store untouched, no binding, no reload, no
        // persist call (only validate_shape ran).
        assert_eq!(store.list_credentials().unwrap().len(), 0);
        assert!(yaml.bindings("ana").is_empty());
        assert_eq!(count.load(Ordering::Relaxed), 0);
        assert_eq!(
            persister.calls.lock().unwrap().as_slice(),
            &["validate_shape".to_string()]
        );
    }

    /// `persist` failure leaves the opaque blob intact (operator
    /// can retry) but aborts before binding + reload.
    #[tokio::test]
    async fn register_persister_persist_error_keeps_opaque_blob_skips_bind() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (count, reload) = reload_counter();
        let mut p = MockPersister::new("telegram");
        p.fail_persist = true;
        let persister = Arc::new(p);

        let result = register(
            &*store,
            &*yaml,
            Some(persister.clone()),
            serde_json::json!({
                "channel": "telegram",
                "instance": "kate",
                "agent_ids": ["ana"],
                "payload": { "token": "x" }
            }),
            &reload,
        )
        .await;

        assert!(matches!(result.error.unwrap(), AdminRpcError::Internal(_)));
        // Opaque blob preserved (operator retry path).
        assert_eq!(store.list_credentials().unwrap().len(), 1);
        // No binding, no reload, no probe (only validate +
        // persist ran).
        assert!(yaml.bindings("ana").is_empty());
        assert_eq!(count.load(Ordering::Relaxed), 0);
        let calls = persister.calls.lock().unwrap().clone();
        assert_eq!(calls, vec!["validate_shape".to_string(), "persist".into()]);
    }

    /// Probe returning unhealthy outcome does NOT abort register;
    /// the credential still lands on disk + agents still bind +
    /// reload fires. Validation flows back to caller for UI.
    #[tokio::test]
    async fn register_unhealthy_probe_does_not_abort_register() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (count, reload) = reload_counter();
        let mut p = MockPersister::new("telegram");
        p.probe_unhealthy = true;
        let persister = Arc::new(p);

        let result = register(
            &*store,
            &*yaml,
            Some(persister.clone()),
            serde_json::json!({
                "channel": "telegram",
                "instance": "kate",
                "agent_ids": ["ana"],
                "payload": { "token": "x" }
            }),
            &reload,
        )
        .await;

        let response: CredentialRegisterResponse =
            serde_json::from_value(result.result.expect("ok")).unwrap();
        let v = response.validation.expect("validation present");
        assert!(v.probed);
        assert!(!v.healthy);
        assert_eq!(v.reason_code.as_deref(), Some(reason_code::CONNECTIVITY_FAILED));
        // Side-effects intact.
        assert_eq!(store.list_credentials().unwrap().len(), 1);
        assert_eq!(yaml.bindings("ana").len(), 1);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    /// No persister registered for the channel → opaque-only
    /// path; validation = None; back-compat preserved.
    #[tokio::test]
    async fn register_no_persister_falls_back_to_opaque_path() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (_count, reload) = reload_counter();

        let result = register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "ghost_channel",
                "agent_ids": ["ana"],
                "payload": {}
            }),
            &reload,
        )
        .await;

        let response: CredentialRegisterResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.validation.is_none());
        assert_eq!(store.list_credentials().unwrap().len(), 1);
    }

    /// Reload fires when persister patches plugin state even if
    /// no agent_ids are supplied (operator pre-registers a cred
    /// for a future agent).
    #[tokio::test]
    async fn register_with_persister_fires_reload_even_without_agent_ids() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (count, reload) = reload_counter();
        let persister = Arc::new(MockPersister::new("telegram"));

        register(
            &*store,
            &*yaml,
            Some(persister),
            serde_json::json!({
                "channel": "telegram",
                "instance": "future",
                "agent_ids": [],
                "payload": { "token": "x" }
            }),
            &reload,
        )
        .await;

        // Persister persist() ran → reload triggered for the
        // plugin to pick up the new yaml entry, even though no
        // agent yaml binding changed.
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    /// `revoke` invokes persister BEFORE the opaque delete.
    #[tokio::test]
    async fn revoke_calls_persister_before_opaque_delete() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (_, reload) = reload_counter();
        let persister = Arc::new(MockPersister::new("telegram"));

        // Pre-populate via opaque-only register (skips
        // persister) so the revoke path is the one under test.
        register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "telegram",
                "instance": "kate",
                "agent_ids": ["ana"],
                "payload": {}
            }),
            &reload,
        )
        .await;

        let result = revoke(
            &*store,
            &*yaml,
            Some(persister.clone()),
            serde_json::json!({ "channel": "telegram", "instance": "kate" }),
            &reload,
        )
        .await;

        let response: CredentialsRevokeResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.removed);
        // Persister revoke ran exactly once.
        assert_eq!(
            persister.calls.lock().unwrap().as_slice(),
            &["revoke".to_string()]
        );
    }

    /// Persister revoke error aborts before the opaque delete.
    #[tokio::test]
    async fn revoke_persister_revoke_error_propagates_internal() {
        let store = Arc::new(MockStore::default());
        let yaml = MockYaml::with_agents(&["ana"]);
        let (_, reload) = reload_counter();
        let mut p = MockPersister::new("telegram");
        p.fail_revoke = true;
        let persister = Arc::new(p);

        // Pre-populate.
        register(
            &*store,
            &*yaml,
            None,
            serde_json::json!({
                "channel": "telegram",
                "instance": "kate",
                "agent_ids": ["ana"],
                "payload": {}
            }),
            &reload,
        )
        .await;

        let result = revoke(
            &*store,
            &*yaml,
            Some(persister),
            serde_json::json!({ "channel": "telegram", "instance": "kate" }),
            &reload,
        )
        .await;

        assert!(matches!(result.error.unwrap(), AdminRpcError::Internal(_)));
        // Opaque blob preserved (we aborted before opaque delete)
        // — and yaml binding is GONE because unbind happens before
        // persister revoke; this is intentional (operator can
        // re-register to restore binding atomically with retry).
        assert_eq!(store.list_credentials().unwrap().len(), 1);
    }
}
