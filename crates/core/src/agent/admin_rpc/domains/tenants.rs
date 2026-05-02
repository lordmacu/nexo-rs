//! Phase 83.8.12 — `nexo/admin/tenants/*` handlers.
//!
//! CRUD surface for multi-tenant empresa records. The runtime
//! itself does not consume tenants directly — it only reads
//! `BindingContext.tenant_id` populated upstream by the
//! producer side. This domain exists for the operator UI +
//! microapp layer to manage the tenancy registry.
//!
//! Backed by an [`EmpresaStore`] trait so this crate stays
//! cycle-free vs `nexo-setup` (which holds the concrete
//! `EmpresasYamlPatcher` adapter introduced in 83.8.12.3).

use async_trait::async_trait;
use serde_json::Value;

use nexo_tool_meta::admin::tenants::{
    EmpresaDetail, EmpresaSummary, EmpresasDeleteParams, EmpresasDeleteResponse,
    EmpresasGetParams, EmpresasGetResponse, EmpresasListFilter, EmpresasListResponse,
    EmpresasUpsertInput, EmpresasUpsertResponse,
};

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Validation cap on `display_name` length. Operators can rename
/// tenants freely but a runaway value has no place in a
/// dropdown. 128 chars matches the typical UI label cap.
pub const MAX_DISPLAY_NAME_CHARS: usize = 128;

/// Storage abstraction for the empresa registry. Production
/// adapter `nexo_setup::admin_adapters::EmpresasYamlPatcher`
/// reads/writes `config/tenants.yaml`. Tests inject in-memory
/// mocks.
#[async_trait]
pub trait EmpresaStore: Send + Sync + std::fmt::Debug {
    /// List tenants matching `filter`. Empty registry returns
    /// an empty `Vec` (NOT an error).
    async fn list(
        &self,
        filter: &EmpresasListFilter,
    ) -> anyhow::Result<Vec<EmpresaSummary>>;

    /// Read one empresa. Unknown id returns `Ok(None)` — daemon
    /// does NOT surface a not-found error so callers can probe.
    async fn get(&self, tenant_id: &str) -> anyhow::Result<Option<EmpresaDetail>>;

    /// Create or update one empresa. The boolean is `true` when
    /// this call created a new record, `false` on idempotent
    /// retry / update.
    async fn upsert(
        &self,
        params: EmpresasUpsertInput,
    ) -> anyhow::Result<(EmpresaDetail, bool)>;

    /// Delete one empresa.
    ///
    /// Returns `(removed, orphaned_agents)`:
    ///
    /// - `purge: false` → returns `(false, [agent ids])` when
    ///   one or more agents still reference the empresa. The
    ///   delete is rejected. UI shows the orphan list and
    ///   confirms before retrying with `purge: true`.
    /// - `purge: false` AND no orphans → cascade is unnecessary,
    ///   delete proceeds, returns `(true, [])`.
    /// - `purge: true` → cascade-deletes every orphan agent,
    ///   then removes the empresa, returns `(true, [])`.
    /// - Unknown empresa id → `(false, [])` (idempotent).
    async fn delete(
        &self,
        tenant_id: &str,
        purge: bool,
    ) -> anyhow::Result<(bool, Vec<String>)>;
}

/// Validate the empresa id matches the kebab-case regex
/// `^[a-z0-9][a-z0-9-]{0,63}$` so the name is safe for use as
/// a directory under `skills/<tenant_id>/` and as a yaml key
/// under `llm.yaml.tenants.<tenant_id>`.
pub fn validate_empresa_id(id: &str) -> Result<(), &'static str> {
    if id.is_empty() {
        return Err("id is empty");
    }
    if id.len() > 64 {
        return Err("id longer than 64 chars");
    }
    let bytes = id.as_bytes();
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err("id must start with [a-z0-9]");
    }
    for &b in bytes {
        let ok = b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
        if !ok {
            return Err("id must match [a-z0-9-]");
        }
    }
    Ok(())
}

fn validate_display_name(name: &str) -> Result<(), &'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("display_name is empty");
    }
    if name.chars().count() > MAX_DISPLAY_NAME_CHARS {
        return Err("display_name exceeds 128 chars");
    }
    Ok(())
}

/// `nexo/admin/tenants/list` — list tenants with optional
/// filters (active_only, prefix).
pub async fn list(store: &dyn EmpresaStore, params: Value) -> AdminRpcResult {
    let filter: EmpresasListFilter = match serde_json::from_value(params) {
        Ok(f) => f,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    let tenants = match store.list(&filter).await {
        Ok(v) => v,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "empresa_store.list: {e}"
            )))
        }
    };
    let response = EmpresasListResponse { tenants };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/tenants/get` — read one empresa. Unknown id
/// returns `{ "empresa": null }`, NOT an error.
pub async fn get(store: &dyn EmpresaStore, params: Value) -> AdminRpcResult {
    let p: EmpresasGetParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if let Err(msg) = validate_empresa_id(&p.tenant_id) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(msg.into()));
    }
    let empresa = match store.get(&p.tenant_id).await {
        Ok(e) => e,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "empresa_store.get: {e}"
            )))
        }
    };
    let response = EmpresasGetResponse { empresa };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/tenants/upsert` — create or update one empresa.
pub async fn upsert(store: &dyn EmpresaStore, params: Value) -> AdminRpcResult {
    let p: EmpresasUpsertInput = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if let Err(msg) = validate_empresa_id(&p.id) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(msg.into()));
    }
    if let Err(msg) = validate_display_name(&p.display_name) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(msg.into()));
    }
    let (empresa, created) = match store.upsert(p).await {
        Ok(pair) => pair,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "empresa_store.upsert: {e}"
            )))
        }
    };
    let response = EmpresasUpsertResponse { empresa, created };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/tenants/delete` — remove one empresa.
/// Idempotent: a missing id returns `{ removed: false }`.
/// Orphan agent handling per the [`EmpresaStore::delete`]
/// contract.
pub async fn delete(store: &dyn EmpresaStore, params: Value) -> AdminRpcResult {
    let p: EmpresasDeleteParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if let Err(msg) = validate_empresa_id(&p.tenant_id) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(msg.into()));
    }
    let (removed, orphaned_agents) = match store.delete(&p.tenant_id, p.purge).await {
        Ok(pair) => pair,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "empresa_store.delete: {e}"
            )))
        }
    };
    let response = EmpresasDeleteResponse {
        removed,
        orphaned_agents,
    };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct InMemoryEmpresaStore {
        rows: Mutex<BTreeMap<String, EmpresaDetail>>,
        /// Configurable: simulate `agents.yaml` rows for orphan
        /// detection. Maps tenant_id → agent_ids.
        agent_index: Mutex<BTreeMap<String, Vec<String>>>,
    }

    impl InMemoryEmpresaStore {
        fn with_agents(&self, tenant_id: &str, agents: &[&str]) {
            self.agent_index.lock().unwrap().insert(
                tenant_id.into(),
                agents.iter().map(|s| s.to_string()).collect(),
            );
        }
    }

    #[async_trait]
    impl EmpresaStore for InMemoryEmpresaStore {
        async fn list(
            &self,
            filter: &EmpresasListFilter,
        ) -> anyhow::Result<Vec<EmpresaSummary>> {
            let rows = self.rows.lock().unwrap();
            let agent_index = self.agent_index.lock().unwrap();
            Ok(rows
                .values()
                .filter(|d| !filter.active_only || d.active)
                .filter(|d| match &filter.prefix {
                    Some(p) => d.id.starts_with(p),
                    None => true,
                })
                .map(|d| EmpresaSummary {
                    id: d.id.clone(),
                    display_name: d.display_name.clone(),
                    active: d.active,
                    agent_count: agent_index.get(&d.id).map(|v| v.len()).unwrap_or(0),
                    created_at: d.created_at,
                })
                .collect())
        }
        async fn get(&self, tenant_id: &str) -> anyhow::Result<Option<EmpresaDetail>> {
            Ok(self.rows.lock().unwrap().get(tenant_id).cloned())
        }
        async fn upsert(
            &self,
            params: EmpresasUpsertInput,
        ) -> anyhow::Result<(EmpresaDetail, bool)> {
            let mut rows = self.rows.lock().unwrap();
            let created = !rows.contains_key(&params.id);
            let existing = rows.get(&params.id).cloned();
            let detail = EmpresaDetail {
                id: params.id.clone(),
                display_name: params.display_name,
                active: params
                    .active
                    .or(existing.as_ref().map(|e| e.active))
                    .unwrap_or(true),
                created_at: existing
                    .as_ref()
                    .map(|e| e.created_at)
                    .unwrap_or_else(|| Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap()),
                llm_provider_refs: params
                    .llm_provider_refs
                    .or_else(|| existing.as_ref().map(|e| e.llm_provider_refs.clone()))
                    .unwrap_or_default(),
                metadata: params
                    .metadata
                    .or_else(|| existing.as_ref().map(|e| e.metadata.clone()))
                    .unwrap_or_default(),
            };
            rows.insert(params.id, detail.clone());
            Ok((detail, created))
        }
        async fn delete(
            &self,
            tenant_id: &str,
            purge: bool,
        ) -> anyhow::Result<(bool, Vec<String>)> {
            let mut rows = self.rows.lock().unwrap();
            if !rows.contains_key(tenant_id) {
                return Ok((false, vec![]));
            }
            let mut agent_index = self.agent_index.lock().unwrap();
            let orphans = agent_index
                .get(tenant_id)
                .cloned()
                .unwrap_or_default();
            if !orphans.is_empty() && !purge {
                return Ok((false, orphans));
            }
            // Either no orphans, or purge=true → cascade.
            agent_index.remove(tenant_id);
            rows.remove(tenant_id);
            Ok((true, vec![]))
        }
    }

    fn store() -> std::sync::Arc<InMemoryEmpresaStore> {
        std::sync::Arc::new(InMemoryEmpresaStore::default())
    }

    #[tokio::test]
    async fn list_empty_returns_empty_vec() {
        let s = store();
        let r = list(s.as_ref(), json!({})).await;
        assert!(r.error.is_none());
        assert_eq!(r.result.unwrap()["tenants"], json!([]));
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips() {
        let s = store();
        let up = upsert(
            s.as_ref(),
            json!({
                "id": "acme-corp",
                "display_name": "Acme Corp.",
                "llm_provider_refs": ["acme-claude"]
            }),
        )
        .await;
        assert!(up.error.is_none());
        assert_eq!(up.result.unwrap()["created"], json!(true));

        let g = get(s.as_ref(), json!({ "tenant_id": "acme-corp" })).await;
        let v = g.result.unwrap();
        assert_eq!(v["empresa"]["id"], json!("acme-corp"));
        assert_eq!(v["empresa"]["display_name"], json!("Acme Corp."));
        assert_eq!(v["empresa"]["active"], json!(true));
    }

    #[tokio::test]
    async fn upsert_twice_reports_created_false_second() {
        let s = store();
        let _ = upsert(s.as_ref(), json!({ "id": "x", "display_name": "X" })).await;
        let up2 = upsert(
            s.as_ref(),
            json!({ "id": "x", "display_name": "X v2" }),
        )
        .await;
        assert_eq!(up2.result.unwrap()["created"], json!(false));
    }

    #[tokio::test]
    async fn get_missing_returns_null_not_error() {
        let s = store();
        let g = get(s.as_ref(), json!({ "tenant_id": "absent" })).await;
        assert!(g.error.is_none());
        assert_eq!(g.result.unwrap(), json!({ "empresa": null }));
    }

    #[tokio::test]
    async fn delete_missing_returns_false_not_error() {
        let s = store();
        let d = delete(s.as_ref(), json!({ "tenant_id": "absent" })).await;
        assert!(d.error.is_none());
        let v = d.result.unwrap();
        assert_eq!(v["removed"], json!(false));
    }

    #[tokio::test]
    async fn delete_with_orphans_purge_false_returns_orphans() {
        let s = store();
        let _ = upsert(
            s.as_ref(),
            json!({ "id": "globex", "display_name": "Globex" }),
        )
        .await;
        s.with_agents("globex", &["g-001", "g-002", "g-003"]);
        let d = delete(
            s.as_ref(),
            json!({ "tenant_id": "globex", "purge": false }),
        )
        .await;
        let v = d.result.unwrap();
        assert_eq!(v["removed"], json!(false));
        assert_eq!(v["orphaned_agents"], json!(["g-001", "g-002", "g-003"]));
    }

    #[tokio::test]
    async fn delete_with_orphans_purge_true_cascades() {
        let s = store();
        let _ = upsert(
            s.as_ref(),
            json!({ "id": "globex", "display_name": "Globex" }),
        )
        .await;
        s.with_agents("globex", &["g-001"]);
        let d = delete(
            s.as_ref(),
            json!({ "tenant_id": "globex", "purge": true }),
        )
        .await;
        let v = d.result.unwrap();
        assert_eq!(v["removed"], json!(true));
        assert!(
            v.get("orphaned_agents").map(|x| x.as_array().unwrap().is_empty()).unwrap_or(true),
            "purge cascade clears orphan list"
        );
    }

    #[tokio::test]
    async fn invalid_id_rejected_with_invalid_params() {
        let s = store();
        let cases = [
            ("", "empty"),
            ("ALL_CAPS", "uppercase"),
            ("foo bar", "space"),
            ("../escape", "traversal"),
            ("foo/bar", "slash"),
            ("-leading-dash", "leading dash"),
            ("foo_bar", "underscore"),
        ];
        for (id, why) in cases {
            let r = get(s.as_ref(), json!({ "tenant_id": id })).await;
            let e = r.error.unwrap_or_else(|| panic!("{why} should reject"));
            assert_eq!(e.code(), -32602, "{why} should be invalid_params");
        }
    }

    #[tokio::test]
    async fn display_name_too_long_rejected() {
        let s = store();
        let long = "x".repeat(MAX_DISPLAY_NAME_CHARS + 1);
        let r = upsert(
            s.as_ref(),
            json!({ "id": "x", "display_name": long }),
        )
        .await;
        assert_eq!(r.error.unwrap().code(), -32602);
    }

    #[tokio::test]
    async fn list_filter_active_only_drops_inactive() {
        let s = store();
        let _ = upsert(
            s.as_ref(),
            json!({ "id": "live", "display_name": "Live", "active": true }),
        )
        .await;
        let _ = upsert(
            s.as_ref(),
            json!({ "id": "frozen", "display_name": "Frozen", "active": false }),
        )
        .await;
        let r = list(s.as_ref(), json!({ "active_only": true })).await;
        let v = r.result.unwrap();
        let names: Vec<&str> = v["tenants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["id"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["live"]);
    }

    #[tokio::test]
    async fn list_filter_prefix() {
        let s = store();
        let _ = upsert(s.as_ref(), json!({ "id": "alpha", "display_name": "A" })).await;
        let _ = upsert(s.as_ref(), json!({ "id": "alpine", "display_name": "B" })).await;
        let _ = upsert(s.as_ref(), json!({ "id": "beta", "display_name": "C" })).await;
        let r = list(s.as_ref(), json!({ "prefix": "alp" })).await;
        let v = r.result.unwrap();
        let ids: Vec<&str> = v["tenants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["alpha", "alpine"]);
    }

    #[test]
    fn validate_empresa_id_accepts_kebab_lowercase() {
        for id in ["acme-corp", "globex", "v1", "0test", "x"] {
            assert!(validate_empresa_id(id).is_ok(), "{id} should be valid");
        }
    }
}
