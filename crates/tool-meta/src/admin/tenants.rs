//! Phase 83.8.12 — `nexo/admin/tenants/*` wire types.
//!
//! An *tenant* (company / tenant) is the top-level multi-tenant
//! key for the SaaS deployment of `agent-creator`. One daemon
//! hosts N tenants; each tenant owns its own agents, skills,
//! and LLM provider keys. The wire shapes live here so SDK
//! consumers (microapps, operator UIs) and the daemon's
//! [`nexo_core::agent::admin_rpc::domains::tenants`] handlers
//! agree on the params + result shapes without either crate
//! pulling in the other.
//!
//! Storage on the daemon side is `config/tenants.yaml`
//! (see `nexo_setup::admin_adapters::TenantsYamlPatcher`); the
//! wire shapes intentionally do NOT mention the file format.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Compact summary used by `tenants/list`. Excludes the LLM
/// provider list + metadata so list calls stay cheap on a daemon
/// that hosts hundreds of tenants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantSummary {
    /// Stable kebab-case id (`acme-corp`, `globex`, …). Matches
    /// the regex `^[a-z0-9][a-z0-9-]{0,63}$`.
    pub id: String,
    /// Operator-facing display name (1..=128 chars after trim).
    pub display_name: String,
    /// Whether the tenant is currently serving traffic.
    pub active: bool,
    /// How many agents in `agents.yaml` reference this tenant.
    /// Computed at list time by the production adapter.
    pub agent_count: usize,
    /// Tenant creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// Full tenant record returned by `tenants/get` and
/// `tenants/upsert`. Adds the LLM provider refs and metadata
/// the summary omits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenantDetail {
    /// Stable kebab-case id.
    pub id: String,
    /// Operator-facing display name.
    pub display_name: String,
    /// Active flag.
    pub active: bool,
    /// Tenant creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Provider names (from the `llm.yaml.tenants.<id>.providers.*`
    /// or `llm.yaml.providers.*` namespace) the tenant is
    /// allowed to use. Empty list = none granted yet.
    #[serde(default)]
    pub llm_provider_refs: Vec<String>,
    /// Free-form metadata the operator UI surfaces (contact info,
    /// billing tier, support tags, etc.). Daemon does not
    /// interpret it — pass-through storage.
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// Params for `nexo/admin/tenants/list`. All filters optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TenantsListFilter {
    /// When `true`, omit tenants whose `active` flag is `false`.
    pub active_only: bool,
    /// Filter by id prefix (case-sensitive). `None` returns
    /// every tenant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// Result of `nexo/admin/tenants/list`. Sorted alpha by id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantsListResponse {
    /// Matching tenants.
    pub tenants: Vec<TenantSummary>,
}

/// Params for `nexo/admin/tenants/get`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantsGetParams {
    /// Stable id.
    pub tenant_id: String,
}

/// Result of `nexo/admin/tenants/get`. `tenant = None` is the
/// not-found case (daemon does NOT return an error so callers
/// can probe).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenantsGetResponse {
    /// Matching tenant, or `None` when the id is unknown.
    pub tenant: Option<TenantDetail>,
}

/// Params for `nexo/admin/tenants/upsert`. Create-or-update —
/// the daemon decides based on whether the id already exists.
/// `None`-valued optional fields preserve the existing value
/// (or default to `true`/`[]`/`{}` for new tenants).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenantsUpsertInput {
    /// Stable kebab-case id. Must match
    /// `^[a-z0-9][a-z0-9-]{0,63}$`.
    pub id: String,
    /// Operator-facing display name.
    pub display_name: String,
    /// `None` keeps existing (or defaults to `true` for new).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// `None` keeps existing (or defaults to empty for new).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_provider_refs: Option<Vec<String>>,
    /// `None` keeps existing (or defaults to empty for new).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, serde_json::Value>>,
}

/// Result of `nexo/admin/tenants/upsert`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenantsUpsertResponse {
    /// Final tenant record after the write.
    pub tenant: TenantDetail,
    /// `true` when this call created a new tenant, `false` when
    /// it updated an existing one (idempotent retry).
    pub created: bool,
}

/// Params for `nexo/admin/tenants/delete`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantsDeleteParams {
    /// Stable id.
    pub tenant_id: String,
    /// `true` cascades the delete to every agent owned by the
    /// tenant. `false` (default) succeeds only when no agents
    /// reference it — the response carries the orphan list so
    /// the UI can confirm before retrying with `purge: true`.
    #[serde(default)]
    pub purge: bool,
}

/// Result of `nexo/admin/tenants/delete`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantsDeleteResponse {
    /// `true` when the tenant entry was removed, `false`
    /// when the delete was rejected (orphan agents present
    /// AND `purge: false`) OR the id had no record.
    pub removed: bool,
    /// Agent ids that still reference the tenant. Populated
    /// when `purge: false` rejected the delete; empty
    /// otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orphaned_agents: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::{from_value, json, to_value};

    fn fixed_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap()
    }

    #[test]
    fn empresa_summary_round_trips() {
        let s = TenantSummary {
            id: "acme-corp".into(),
            display_name: "Acme Corp.".into(),
            active: true,
            agent_count: 3,
            created_at: fixed_ts(),
        };
        let v = to_value(&s).unwrap();
        let back: TenantSummary = from_value(v).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn empresa_detail_round_trips_with_metadata() {
        let mut metadata = BTreeMap::new();
        metadata.insert("contact".into(), json!("support@acme.example"));
        metadata.insert("tier".into(), json!("pro"));
        let d = TenantDetail {
            id: "acme-corp".into(),
            display_name: "Acme Corp.".into(),
            active: true,
            created_at: fixed_ts(),
            llm_provider_refs: vec!["acme-claude".into(), "acme-minimax".into()],
            metadata,
        };
        let v = to_value(&d).unwrap();
        let back: TenantDetail = from_value(v).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn empresas_upsert_input_omits_optional_fields_when_none() {
        let p = TenantsUpsertInput {
            id: "globex".into(),
            display_name: "Globex".into(),
            active: None,
            llm_provider_refs: None,
            metadata: None,
        };
        let v = to_value(&p).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("active"));
        assert!(!obj.contains_key("llm_provider_refs"));
        assert!(!obj.contains_key("metadata"));
    }

    #[test]
    fn empresas_delete_response_omits_orphans_when_empty() {
        let r = TenantsDeleteResponse {
            removed: true,
            orphaned_agents: vec![],
        };
        let v = to_value(&r).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("orphaned_agents"));
    }

    #[test]
    fn empresas_delete_response_includes_orphans_when_present() {
        let r = TenantsDeleteResponse {
            removed: false,
            orphaned_agents: vec!["a-1".into(), "a-2".into()],
        };
        let v = to_value(&r).unwrap();
        assert_eq!(v["removed"], json!(false));
        assert_eq!(v["orphaned_agents"], json!(["a-1", "a-2"]));
    }

    #[test]
    fn empresas_list_filter_default_serializes_compact() {
        let f = TenantsListFilter::default();
        let v = to_value(&f).unwrap();
        assert_eq!(v, json!({ "active_only": false }));
    }

    #[test]
    fn empresas_get_response_serializes_none_explicitly() {
        let r = TenantsGetResponse { tenant: None };
        let v = to_value(&r).unwrap();
        assert_eq!(v, json!({ "tenant": null }));
    }

    #[test]
    fn empresas_upsert_response_round_trips_created_flag() {
        let r = TenantsUpsertResponse {
            tenant: TenantDetail {
                id: "x".into(),
                display_name: "X".into(),
                active: true,
                created_at: fixed_ts(),
                llm_provider_refs: vec![],
                metadata: BTreeMap::new(),
            },
            created: true,
        };
        let v = to_value(&r).unwrap();
        assert_eq!(v["created"], json!(true));
        let back: TenantsUpsertResponse = from_value(v).unwrap();
        assert_eq!(r, back);
    }
}
