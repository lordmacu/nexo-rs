//! Phase 81.31 — `nexo/admin/persona/*` handlers.
//!
//! Writes localised persona content (system_prompt +
//! IDENTITY/SOUL/USER/AGENTS) for one agent across one BCP-47
//! locale. Reads the catalog of available locales + per-locale
//! snapshots back through [`PersonaSnapshotReader`] (consumed by
//! `agents/get` to populate `AgentDetail::persona_locales`).
//!
//! Production impls live in `nexo-setup::admin_adapters`.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use nexo_tool_meta::admin::persona::{
    PersonaLocales, PersonaSaveLocalizedRequest, PersonaSaveLocalizedResponse,
};

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Errors a [`PersonaStore`] can surface. Each variant maps to a
/// specific operator-facing message; the dispatcher converts them
/// to `-32603 Internal` (or `-32602 InvalidParams` for bad input).
#[derive(Debug, Error)]
pub enum PersonaStoreError {
    /// Locale didn't pass BCP-47 validation. Wire mapping:
    /// `-32602 invalid_params`.
    #[error("invalid locale: {0}")]
    InvalidLocale(String),
    /// Agent id not found in any loaded `agents.yaml` / persona
    /// `agents.d/*.yaml`.
    #[error("agent {0:?} not found")]
    NotFound(String),
    /// Filesystem / YAML mutation failed. Wire mapping:
    /// `-32603 internal`.
    #[error("io: {0}")]
    Io(String),
}

/// Read-only catalog of localised persona variants. Consumed by
/// `agents/get` to populate [`PersonaLocales`].
#[async_trait]
pub trait PersonaSnapshotReader: Send + Sync + std::fmt::Debug {
    /// Build the locale catalog for `agent_id`. `None` = agent
    /// has no workspace dir + no `locale_prompts` map (legacy);
    /// the admin then renders the single-locale wizard branch.
    async fn read_locales(&self, agent_id: &str) -> Option<PersonaLocales>;
}

/// Write side of the persona file CRUD. Accepts a complete locale
/// snapshot + optional `patch_yaml` flag controlling whether the
/// daemon also updates `agents.d/<id>.yaml::locale_prompts`.
#[async_trait]
pub trait PersonaStore: Send + Sync + std::fmt::Debug {
    /// Persist `req` atomically (per-file temp+rename) under the
    /// agent's workspace dir + (optionally) patch the YAML map.
    /// Returns the refreshed locale catalog so the admin can
    /// update its dropdown without a second roundtrip.
    async fn save_localized(
        &self,
        req: PersonaSaveLocalizedRequest,
    ) -> Result<PersonaSaveLocalizedResponse, PersonaStoreError>;
}

/// `nexo/admin/persona/save_localized` handler.
pub async fn save_localized(store: &dyn PersonaStore, params: Value) -> AdminRpcResult {
    let req: PersonaSaveLocalizedRequest = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    match store.save_localized(req).await {
        Ok(resp) => AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null)),
        Err(PersonaStoreError::InvalidLocale(msg)) => {
            AdminRpcResult::err(AdminRpcError::InvalidParams(msg))
        }
        Err(PersonaStoreError::NotFound(id)) => {
            AdminRpcResult::err(AdminRpcError::Internal(format!("agent {id:?} not found")))
        }
        Err(PersonaStoreError::Io(msg)) => {
            AdminRpcResult::err(AdminRpcError::Internal(format!("io error: {msg}")))
        }
    }
}

/// One installed persona surfaced as an agent-creation template.
/// The admin wizard renders these as the "use as template"
/// dropdown; selecting one fetches the full payload via
/// [`PersonaCatalogReader::template_for`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersonaCatalogEntry {
    /// Persona id (matches the template filename stem).
    pub id: String,
    /// Installed version (`"0.2.1"`).
    pub version: String,
    /// On-disk install root, useful to the admin for debugging
    /// install collisions.
    pub install_root: String,
    /// `<owner>/<repo>` the pack was sourced from.
    pub source_repo: String,
}

/// Read-only catalog of installed personas. Distinct from
/// `cfg.agents`: these are TEMPLATES the wizard offers when the
/// operator creates a new agent, not live instances. The daemon
/// must never auto-merge templates into the running agent set.
#[async_trait]
pub trait PersonaCatalogReader: Send + Sync + std::fmt::Debug {
    /// Enumerate installed personas. Order is not guaranteed;
    /// callers needing a stable order sort client-side. Returns
    /// an empty vec when no personas are installed.
    async fn list(&self) -> Vec<PersonaCatalogEntry>;

    /// Return the agent yaml template shipped by persona `id` as
    /// a JSON object (deserialised from `<install_root>/agents.d/<id>.yaml`).
    /// `None` when the id is unknown or the persona ships no
    /// template file. Wizard merges this into the form scaffold
    /// before letting the operator override fields.
    async fn template_for(&self, id: &str) -> Option<Value>;
}

/// `nexo/admin/personas/list` handler.
pub async fn list_personas(reader: &dyn PersonaCatalogReader) -> AdminRpcResult {
    let entries = reader.list().await;
    AdminRpcResult::ok(serde_json::json!({ "personas": entries }))
}

/// `nexo/admin/personas/template_get` handler. Params shape:
/// `{ "persona_id": "cody" }`. Returns the template payload as
/// `{ "template": {...} }` when found, or `{ "template": null }`
/// otherwise so the admin can render an empty-state without an
/// error path.
pub async fn get_persona_template(
    reader: &dyn PersonaCatalogReader,
    params: Value,
) -> AdminRpcResult {
    let id = match params.get("persona_id").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(
                "persona_id is required and must be a non-empty string".into(),
            ));
        }
    };
    let tpl = reader.template_for(&id).await;
    AdminRpcResult::ok(serde_json::json!({ "template": tpl }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::admin::persona::PersonaLocales;

    #[derive(Debug)]
    struct StubStore;

    #[async_trait]
    impl PersonaStore for StubStore {
        async fn save_localized(
            &self,
            req: PersonaSaveLocalizedRequest,
        ) -> Result<PersonaSaveLocalizedResponse, PersonaStoreError> {
            assert_eq!(req.agent_id, "cody");
            Ok(PersonaSaveLocalizedResponse {
                written_paths: vec!["/tmp/cody/IDENTITY.es.md".into()],
                persona_locales: PersonaLocales {
                    available: vec!["es".into(), "en".into()],
                    snapshots: vec![],
                },
            })
        }
    }

    #[derive(Debug)]
    struct ErrStore;

    #[async_trait]
    impl PersonaStore for ErrStore {
        async fn save_localized(
            &self,
            _req: PersonaSaveLocalizedRequest,
        ) -> Result<PersonaSaveLocalizedResponse, PersonaStoreError> {
            Err(PersonaStoreError::InvalidLocale("klingon".into()))
        }
    }

    #[tokio::test]
    async fn save_localized_ok_emits_response() {
        let params = serde_json::json!({
            "agent_id": "cody",
            "locale": "es",
            "system_prompt": "p",
            "identity": "i",
            "soul": "s",
            "user": "u",
            "agents": "a",
        });
        let res = save_localized(&StubStore, params).await;
        let v = res.result.expect("ok");
        assert_eq!(v["persona_locales"]["available"][0], "es");
    }

    #[tokio::test]
    async fn save_localized_invalid_locale_maps_to_invalid_params() {
        let params = serde_json::json!({
            "agent_id": "x",
            "locale": "klingon",
            "system_prompt": "",
            "identity": "",
            "soul": "",
            "user": "",
            "agents": "",
        });
        let res = save_localized(&ErrStore, params).await;
        let err = res.error.unwrap();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }
}
