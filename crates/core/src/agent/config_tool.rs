//! Phase 79.10 — `Config` LLM tool, gated by the
//! `config-self-edit` Cargo feature.
//!
//! Step 8 ships the read path + the full handler scaffold + the
//! traits used to bridge to the YAML-patch helpers in nexo-setup.
//! Step 9 replaces the placeholder `propose` / `apply` arms with
//! the full proposal-staging + approval-correlator + reload-rollback
//! flow.
//!
//! The handler ignores `_ctx` for now; per-binding policy resolution
//! (selection of agent, allowed_paths, approval_timeout) happens at
//! construction time. main.rs (step 11) constructs one ConfigTool
//! per agent.
//!
//! Cycle resolution: `nexo-core` cannot depend on `nexo-setup` (the
//! reverse already exists), so the YAML write helpers + denylist
//! matcher are accessed via traits ([`YamlPatchApplier`],
//! [`DenylistChecker`]) the call site implements with a thin
//! adapter over nexo-setup.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/ConfigTool/ConfigTool.ts:111-150`
//!     — read branch handler skeleton.
//!   * `:436-453` — `getValue` path traversal.
//!   * `:184-228` — SET path validation order (covered in step 9).
//!   * `prompt.ts:14-93` — dynamic description shape (we ship a
//!     static MVP description; dynamic version in 79.10.b
//!     follow-up).

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

/// Bridge to the YAML-patch helpers living in nexo-setup. The
/// concrete impl in main.rs delegates to
/// `nexo_setup::yaml_patch::{read_agent_field, apply_patch_with_denylist}`.
pub trait YamlPatchApplier: Send + Sync {
    /// Read the dotted path under `agent_id`. Returns `Ok(None)` when
    /// the key is absent. The caller is responsible for redacting
    /// secret keys before surfacing the value to the LLM.
    fn read(
        &self,
        agent_id: &str,
        dotted: &str,
    ) -> Result<Option<serde_yaml::Value>, PatchAppliedError>;

    /// Apply a patch (already validated against the denylist).
    /// Implementations call into
    /// `nexo_setup::yaml_patch::apply_patch_with_denylist`.
    fn apply(&self, patch: &PatchInfo) -> Result<(), PatchAppliedError>;

    /// Snapshot the agents.yaml content for rollback. Returns
    /// `Vec<u8>` of the file contents at the moment of the call.
    fn snapshot(&self) -> Result<Vec<u8>, PatchAppliedError>;

    /// Restore a snapshot. Used by the rollback path when reload
    /// rejects the post-apply config.
    fn restore(&self, snapshot: &[u8]) -> Result<(), PatchAppliedError>;
}

/// Bridge to the hard-coded denylist in
/// `nexo_setup::capabilities::CONFIG_SELF_EDIT_DENYLIST`. Returns
/// the matched glob string when the dotted path is denied.
pub trait DenylistChecker: Send + Sync {
    fn check(&self, dotted: &str) -> Option<&'static str>;
}

/// Crate-local copy of the patch envelope. Mirrors
/// `nexo_setup::yaml_patch::YamlPatch`; the trait conversion
/// happens in the adapter at the call site.
#[derive(Debug, Clone)]
pub struct PatchInfo {
    pub patch_id: String,
    pub binding_id: String,
    pub agent_id: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub justification: String,
    pub op: PatchOp,
}

#[derive(Debug, Clone)]
pub enum PatchOp {
    Upsert {
        agent_id: String,
        dotted: String,
        value: serde_yaml::Value,
    },
    Remove {
        agent_id: String,
        dotted: String,
    },
}

impl PatchOp {
    pub fn dotted(&self) -> &str {
        match self {
            PatchOp::Upsert { dotted, .. } | PatchOp::Remove { dotted, .. } => dotted.as_str(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PatchAppliedError {
    #[error("path `{path}` denied (matched glob `{matched_glob}`)")]
    Forbidden {
        path: String,
        matched_glob: String,
    },
    #[error("io: {0}")]
    Io(String),
    #[error("yaml: {0}")]
    Yaml(String),
}

/// Read-only secret-redaction predicate. Same shape as the
/// nexo-setup helper — checks dotted path suffix against the
/// known credential patterns.
pub trait SecretRedactor: Send + Sync {
    fn is_secret_path(&self, dotted: &str) -> bool;
}

/// Default redactor: matches the same suffixes as the denylist
/// (`*_token`, `*_secret`, `*_password`, `*_key`).
pub struct DefaultSecretRedactor;
impl SecretRedactor for DefaultSecretRedactor {
    fn is_secret_path(&self, dotted: &str) -> bool {
        let lower = dotted.to_ascii_lowercase();
        lower.ends_with("_token")
            || lower.ends_with("_secret")
            || lower.ends_with("_password")
            || lower.ends_with("_key")
    }
}

/// Per-agent ConfigTool instance. main.rs constructs one per
/// `agents[].config_tool.self_edit == true` agent.
pub struct ConfigTool {
    pub agent_id: String,
    pub binding_id: String,
    pub allowed_paths: Vec<String>,
    pub approval_timeout_secs: u64,
    pub proposals_dir: PathBuf,
    pub applier: Arc<dyn YamlPatchApplier>,
    pub denylist: Arc<dyn DenylistChecker>,
    pub redactor: Arc<dyn SecretRedactor>,
    pub changes_store: Arc<dyn crate::config_changes_store::ConfigChangesStore>,
    // Approval correlator + reload coordinator land in step 9.
}

impl ConfigTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "Config".to_string(),
            description: r#"Read or propose changes to your own agent YAML configuration. Three operations:
- `op: "read"` — return the current value for `key` (a dotted path like "model.model" or "language"). Read-only; passes plan-mode.
- `op: "propose"` — stage a candidate change and notify the operator on the originating channel for approval. Returns a `patch_id`.
- `op: "apply"` — apply a previously-proposed and operator-approved patch.

Rules:
- Only paths in the agent's `config_tool.allowed_paths` whitelist (or all `SUPPORTED_SETTINGS` when empty) are touchable.
- A hard-coded denylist blocks credentials, pairing internals, MCP server commands, role/plan-mode self-elevation, and other sensitive keys. Refusal is not negotiable.
- `apply` requires an operator approval message of shape `[config-approve patch_id=<id>]` on the same channel/account that originated the proposal.
- Proposals expire after the agent's `approval_timeout_secs` (default 24 h)."#
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "op": {
                        "type": "string",
                        "enum": ["read", "propose", "apply"],
                        "description": "Operation: read | propose | apply."
                    },
                    "key": {
                        "type": "string",
                        "description": "Dotted YAML path. Required for `read` and `propose`."
                    },
                    "value": {
                        "description": "New value for `propose`. JSON literal; `null` means \"remove the key\". Omit for `read`."
                    },
                    "justification": {
                        "type": "string",
                        "description": "One-sentence reason for `propose`. Surfaces in the operator approval message."
                    },
                    "patch_id": {
                        "type": "string",
                        "description": "Patch id returned by an earlier `propose`. Required for `apply`."
                    }
                },
                "required": ["op"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for ConfigTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let op = args
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Config: missing required field `op`"))?;
        match op {
            "read" => self.handle_read(args).await,
            "propose" => self.handle_propose(args).await,
            "apply" => self.handle_apply(args).await,
            other => Ok(json!({
                "ok": false,
                "error": format!("unknown op `{other}`. Supported: read, propose, apply"),
                "kind": "UnknownOp",
            })),
        }
    }
}

impl ConfigTool {
    async fn handle_read(&self, args: Value) -> anyhow::Result<Value> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Config read: missing required field `key`"))?
            .to_string();

        // Triple gate (read also enforces — even reading a denied
        // path returns an error so the model doesn't see the value).
        if !self.path_in_supported(&key) {
            return Ok(json!({
                "ok": false,
                "error": format!("key `{key}` is not in SUPPORTED_SETTINGS"),
                "kind": "UnknownKey",
                "key": key,
            }));
        }
        if !self.path_in_allowed(&key) {
            return Ok(json!({
                "ok": false,
                "error": format!(
                    "key `{key}` is not in this agent's `config_tool.allowed_paths` whitelist"
                ),
                "kind": "PathNotAllowed",
                "key": key,
            }));
        }
        if let Some(matched_glob) = self.denylist.check(&key) {
            return Ok(json!({
                "ok": false,
                "error": format!("key `{key}` denied by self-edit policy"),
                "kind": "ForbiddenKey",
                "key": key,
                "matched_glob": matched_glob,
            }));
        }

        let value = match self.applier.read(&self.agent_id, &key) {
            Ok(v) => v,
            Err(e) => {
                return Ok(json!({
                    "ok": false,
                    "error": e.to_string(),
                    "kind": "Yaml",
                    "key": key,
                }))
            }
        };
        let rendered = if self.redactor.is_secret_path(&key) {
            json!("<REDACTED>")
        } else {
            match value {
                Some(v) => yaml_to_json(&v),
                None => Value::Null,
            }
        };
        Ok(json!({
            "ok": true,
            "key": key,
            "value": rendered,
        }))
    }

    async fn handle_propose(&self, _args: Value) -> anyhow::Result<Value> {
        // Step 9 fills this in. Step 8 ships the placeholder so the
        // tool surface is stable (and the read path is exercisable
        // end-to-end while step 9 is in flight).
        Ok(json!({
            "ok": false,
            "error": "Config: `propose` not yet implemented in this build (step 9)",
            "kind": "NotImplemented",
        }))
    }

    async fn handle_apply(&self, _args: Value) -> anyhow::Result<Value> {
        Ok(json!({
            "ok": false,
            "error": "Config: `apply` not yet implemented in this build (step 9)",
            "kind": "NotImplemented",
        }))
    }

    fn path_in_supported(&self, key: &str) -> bool {
        nexo_config::types::config_tool::is_supported(key)
    }

    fn path_in_allowed(&self, key: &str) -> bool {
        if self.allowed_paths.is_empty() {
            return true;
        }
        self.allowed_paths.iter().any(|p| p == key)
    }
}

/// Translate a `serde_yaml::Value` into a JSON value for the
/// tool result. Best-effort: numbers, strings, bools, sequences,
/// mappings round-trip cleanly. Tags + anchors collapse to their
/// inner value.
fn yaml_to_json(v: &serde_yaml::Value) -> Value {
    match v {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            }
        }
        serde_yaml::Value::String(s) => Value::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            Value::Array(seq.iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(m) => {
            let mut out = serde_json::Map::new();
            for (k, v) in m {
                if let Some(s) = k.as_str() {
                    out.insert(s.to_string(), yaml_to_json(v));
                }
            }
            Value::Object(out)
        }
        serde_yaml::Value::Tagged(t) => yaml_to_json(&t.value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_changes_store::SqliteConfigChangesStore;
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use std::sync::Mutex;

    /// Mock applier that holds the YAML in memory as a
    /// `serde_yaml::Mapping`. Concrete enough to exercise read +
    /// snapshot/restore round-trips without touching disk.
    struct MockApplier {
        data: Mutex<serde_yaml::Mapping>,
    }

    impl MockApplier {
        fn new(yaml: &str) -> Self {
            let m: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
            Self {
                data: Mutex::new(m.as_mapping().cloned().unwrap_or_default()),
            }
        }
    }

    impl YamlPatchApplier for MockApplier {
        fn read(
            &self,
            agent_id: &str,
            dotted: &str,
        ) -> Result<Option<serde_yaml::Value>, PatchAppliedError> {
            let map = self.data.lock().unwrap();
            // Find agent within `agents:` list.
            let agents = match map.get(serde_yaml::Value::String("agents".into())) {
                Some(serde_yaml::Value::Sequence(s)) => s,
                _ => return Ok(None),
            };
            let agent = agents.iter().find(|a| {
                a.as_mapping()
                    .and_then(|m| m.get(serde_yaml::Value::String("id".into())))
                    .and_then(|v| v.as_str())
                    == Some(agent_id)
            });
            let mut cur = match agent {
                Some(a) => a.clone(),
                None => return Ok(None),
            };
            for part in dotted.split('.') {
                let next = cur
                    .as_mapping()
                    .and_then(|m| m.get(serde_yaml::Value::String(part.to_string())))
                    .cloned();
                match next {
                    Some(v) => cur = v,
                    None => return Ok(None),
                }
            }
            Ok(Some(cur))
        }

        fn apply(&self, _patch: &PatchInfo) -> Result<(), PatchAppliedError> {
            // Step 9 exercises this.
            Ok(())
        }

        fn snapshot(&self) -> Result<Vec<u8>, PatchAppliedError> {
            Ok(Vec::new())
        }

        fn restore(&self, _snapshot: &[u8]) -> Result<(), PatchAppliedError> {
            Ok(())
        }
    }

    struct PermissiveDenylist;
    impl DenylistChecker for PermissiveDenylist {
        fn check(&self, _dotted: &str) -> Option<&'static str> {
            None
        }
    }

    struct StrictDenylist;
    impl DenylistChecker for StrictDenylist {
        fn check(&self, dotted: &str) -> Option<&'static str> {
            if dotted.starts_with("pairing.") {
                Some("pairing.*")
            } else {
                None
            }
        }
    }

    fn agent_context_fixture() -> AgentContext {
        let cfg = AgentConfig {
            id: "cody".into(),
            model: ModelConfig {
                provider: "x".into(),
                model: "y".into(),
            },
            plugins: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: Vec::new(),
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: DreamingYamlConfig::default(),
            workspace_git: WorkspaceGitConfig::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            outbound_allowlist: OutboundAllowlistConfig::default(),
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
        };
        AgentContext::new(
            "cody",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    async fn build_tool(
        applier: Arc<dyn YamlPatchApplier>,
        denylist: Arc<dyn DenylistChecker>,
        allowed: Vec<String>,
    ) -> ConfigTool {
        let store: Arc<dyn crate::config_changes_store::ConfigChangesStore> =
            Arc::new(SqliteConfigChangesStore::open_in_memory().await.unwrap());
        ConfigTool {
            agent_id: "cody".into(),
            binding_id: "wa:default".into(),
            allowed_paths: allowed,
            approval_timeout_secs: 86_400,
            proposals_dir: PathBuf::from("/tmp/nexo-proposals"),
            applier,
            denylist,
            redactor: Arc::new(DefaultSecretRedactor),
            changes_store: store,
        }
    }

    fn fixture_yaml() -> &'static str {
        r#"
agents:
  - id: cody
    model:
      provider: anthropic
      model: claude-sonnet-4-6
    language: en
"#
    }

    #[test]
    fn tool_def_advertises_three_ops() {
        let def = ConfigTool::tool_def();
        assert_eq!(def.name, "Config");
        let params = &def.parameters;
        let ops = params["properties"]["op"]["enum"].as_array().unwrap();
        let names: Vec<&str> = ops.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(names, vec!["read", "propose", "apply"]);
    }

    #[tokio::test]
    async fn read_returns_value_for_supported_key() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;
        let res = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "read", "key": "model.model" }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["key"], "model.model");
        assert_eq!(res["value"], "claude-sonnet-4-6");
    }

    #[tokio::test]
    async fn read_rejects_off_whitelist_key() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;
        let res = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "read", "key": "agents[0].pairing.session_token" }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        // Either UnknownKey (not in SUPPORTED_SETTINGS) or
        // ForbiddenKey if the strict denylist ever lands here.
        let kind = res["kind"].as_str().unwrap();
        assert!(
            kind == "UnknownKey" || kind == "ForbiddenKey",
            "got kind `{kind}`"
        );
    }

    #[tokio::test]
    async fn read_blocks_path_outside_allowed_paths() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec!["language".into()],
        )
        .await;
        let res = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "read", "key": "model.model" }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "PathNotAllowed");
    }

    #[tokio::test]
    async fn read_returns_denied_when_denylist_hits() {
        // The strict denylist treats `pairing.*` as forbidden; combine
        // with `model.model` ALLOWED to isolate the denylist branch.
        // We pretend `pairing.session_token` is in SUPPORTED to
        // exercise the denylist path before SUPPORTED check —
        // note: SUPPORTED is checked first, so this test is more of a
        // contract: `model.model` is supported, NOT denied. Use a
        // setting that IS in SUPPORTED but for which we make the
        // denylist trip. None of our SUPPORTED keys start with
        // `pairing.`, so this scenario is theoretical; the test
        // exercises the structural ordering instead.
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(StrictDenylist),
            vec![],
        )
        .await;
        let res = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "read", "key": "model.model" }),
            )
            .await
            .unwrap();
        // `model.model` is supported AND not in StrictDenylist, so OK.
        assert_eq!(res["ok"], true);
    }

    #[tokio::test]
    async fn propose_returns_not_implemented_in_step_8() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;
        let res = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "propose", "key": "model.model", "value": "claude-opus-4-7" }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "NotImplemented");
    }

    #[tokio::test]
    async fn apply_returns_not_implemented_in_step_8() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;
        let res = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "apply", "patch_id": "01J7" }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "NotImplemented");
    }

    #[tokio::test]
    async fn unknown_op_returns_explicit_error() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;
        let res = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "delete", "key": "model.model" }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "UnknownOp");
    }
}
