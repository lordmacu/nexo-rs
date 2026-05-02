//! Phase 83.8.12 — `nexo/admin/empresas/*` wire types.
//!
//! An *empresa* (company / tenant) is the top-level multi-tenant
//! key for the SaaS deployment of `agent-creator`. One daemon
//! hosts N empresas; each empresa owns its own agents, skills,
//! and LLM provider keys. The wire shapes live here so SDK
//! consumers (microapps, operator UIs) and the daemon's
//! [`nexo_core::agent::admin_rpc::domains::empresas`] handlers
//! agree on the params + result shapes without either crate
//! pulling in the other.
//!
//! Storage on the daemon side is `config/empresas.yaml`
//! (see `nexo_setup::admin_adapters::EmpresasYamlPatcher`); the
//! wire shapes intentionally do NOT mention the file format.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Compact summary used by `empresas/list`. Excludes the LLM
/// provider list + metadata so list calls stay cheap on a daemon
/// that hosts hundreds of empresas.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmpresaSummary {
    /// Stable kebab-case id (`acme-corp`, `globex`, …). Matches
    /// the regex `^[a-z0-9][a-z0-9-]{0,63}$`.
    pub id: String,
    /// Operator-facing display name (1..=128 chars after trim).
    pub display_name: String,
    /// Whether the empresa is currently serving traffic.
    pub active: bool,
    /// How many agents in `agents.yaml` reference this empresa.
    /// Computed at list time by the production adapter.
    pub agent_count: usize,
    /// Empresa creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// Full empresa record returned by `empresas/get` and
/// `empresas/upsert`. Adds the LLM provider refs and metadata
/// the summary omits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmpresaDetail {
    /// Stable kebab-case id.
    pub id: String,
    /// Operator-facing display name.
    pub display_name: String,
    /// Active flag.
    pub active: bool,
    /// Empresa creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Provider names (from the `llm.yaml.empresas.<id>.providers.*`
    /// or `llm.yaml.providers.*` namespace) the empresa is
    /// allowed to use. Empty list = none granted yet.
    #[serde(default)]
    pub llm_provider_refs: Vec<String>,
    /// Free-form metadata the operator UI surfaces (contact info,
    /// billing tier, support tags, etc.). Daemon does not
    /// interpret it — pass-through storage.
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// Params for `nexo/admin/empresas/list`. All filters optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EmpresasListFilter {
    /// When `true`, omit empresas whose `active` flag is `false`.
    pub active_only: bool,
    /// Filter by id prefix (case-sensitive). `None` returns
    /// every empresa.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// Result of `nexo/admin/empresas/list`. Sorted alpha by id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmpresasListResponse {
    /// Matching empresas.
    pub empresas: Vec<EmpresaSummary>,
}

/// Params for `nexo/admin/empresas/get`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmpresasGetParams {
    /// Stable id.
    pub empresa_id: String,
}

/// Result of `nexo/admin/empresas/get`. `empresa = None` is the
/// not-found case (daemon does NOT return an error so callers
/// can probe).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmpresasGetResponse {
    /// Matching empresa, or `None` when the id is unknown.
    pub empresa: Option<EmpresaDetail>,
}

/// Params for `nexo/admin/empresas/upsert`. Create-or-update —
/// the daemon decides based on whether the id already exists.
/// `None`-valued optional fields preserve the existing value
/// (or default to `true`/`[]`/`{}` for new empresas).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmpresasUpsertInput {
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

/// Result of `nexo/admin/empresas/upsert`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmpresasUpsertResponse {
    /// Final empresa record after the write.
    pub empresa: EmpresaDetail,
    /// `true` when this call created a new empresa, `false` when
    /// it updated an existing one (idempotent retry).
    pub created: bool,
}

/// Params for `nexo/admin/empresas/delete`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmpresasDeleteParams {
    /// Stable id.
    pub empresa_id: String,
    /// `true` cascades the delete to every agent owned by the
    /// empresa. `false` (default) succeeds only when no agents
    /// reference it — the response carries the orphan list so
    /// the UI can confirm before retrying with `purge: true`.
    #[serde(default)]
    pub purge: bool,
}

/// Result of `nexo/admin/empresas/delete`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmpresasDeleteResponse {
    /// `true` when the empresa entry was removed, `false`
    /// when the delete was rejected (orphan agents present
    /// AND `purge: false`) OR the id had no record.
    pub removed: bool,
    /// Agent ids that still reference the empresa. Populated
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
        let s = EmpresaSummary {
            id: "acme-corp".into(),
            display_name: "Acme Corp.".into(),
            active: true,
            agent_count: 3,
            created_at: fixed_ts(),
        };
        let v = to_value(&s).unwrap();
        let back: EmpresaSummary = from_value(v).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn empresa_detail_round_trips_with_metadata() {
        let mut metadata = BTreeMap::new();
        metadata.insert("contact".into(), json!("support@acme.example"));
        metadata.insert("tier".into(), json!("pro"));
        let d = EmpresaDetail {
            id: "acme-corp".into(),
            display_name: "Acme Corp.".into(),
            active: true,
            created_at: fixed_ts(),
            llm_provider_refs: vec!["acme-claude".into(), "acme-minimax".into()],
            metadata,
        };
        let v = to_value(&d).unwrap();
        let back: EmpresaDetail = from_value(v).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn empresas_upsert_input_omits_optional_fields_when_none() {
        let p = EmpresasUpsertInput {
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
        let r = EmpresasDeleteResponse {
            removed: true,
            orphaned_agents: vec![],
        };
        let v = to_value(&r).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("orphaned_agents"));
    }

    #[test]
    fn empresas_delete_response_includes_orphans_when_present() {
        let r = EmpresasDeleteResponse {
            removed: false,
            orphaned_agents: vec!["a-1".into(), "a-2".into()],
        };
        let v = to_value(&r).unwrap();
        assert_eq!(v["removed"], json!(false));
        assert_eq!(v["orphaned_agents"], json!(["a-1", "a-2"]));
    }

    #[test]
    fn empresas_list_filter_default_serializes_compact() {
        let f = EmpresasListFilter::default();
        let v = to_value(&f).unwrap();
        assert_eq!(v, json!({ "active_only": false }));
    }

    #[test]
    fn empresas_get_response_serializes_none_explicitly() {
        let r = EmpresasGetResponse { empresa: None };
        let v = to_value(&r).unwrap();
        assert_eq!(v, json!({ "empresa": null }));
    }

    #[test]
    fn empresas_upsert_response_round_trips_created_flag() {
        let r = EmpresasUpsertResponse {
            empresa: EmpresaDetail {
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
        let back: EmpresasUpsertResponse = from_value(v).unwrap();
        assert_eq!(r, back);
    }
}
