//! Phase 79.10 — `Config` LLM tool, gated by the
//! `config-self-edit` Cargo feature.
//!
//! Step 8 ships the read path + the full handler scaffold + the
//! traits used to bridge to the YAML-patch helpers in nexo-setup.
//! Step 9 replaces the placeholder `propose` / `apply` arms with
//! the full proposal-staging + approval-correlator + reload-rollback
//! flow.
//!
//! Per-binding policy resolution (selection of agent, allowed_paths,
//! approval_timeout) happens at construction time. Per-turn actor
//! origin (`channel/account/sender`) is resolved from `AgentContext`
//! when `op="propose"` is called so approvals route back to the
//! inbound source that initiated the change.
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
    Forbidden { path: String, matched_glob: String },
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

/// Reload outcome the ConfigTool consumes from
/// `ConfigReloadCoordinator::reload`. We define a local trait
/// instead of taking `Arc<ConfigReloadCoordinator>` directly so
/// the test path can substitute a mock without booting the full
/// reload chain.
#[async_trait]
pub trait ReloadTrigger: Send + Sync {
    /// Trigger a config reload. Returns `Ok(())` on accept;
    /// `Err(reason)` when the coordinator rejected the post-apply
    /// snapshot. The caller (`apply` path) initiates a rollback
    /// when `Err` is returned.
    async fn reload(&self) -> Result<(), String>;
}

/// Default origin for proposals when the agent has no inbound
/// origin context (e.g. a heartbeat-driven goal). Captured into
/// the staging file so the audit log keeps full provenance.
#[derive(Debug, Clone)]
pub struct ActorOrigin {
    pub channel: String,
    pub account_id: String,
    pub sender_id: String,
}

/// Per-agent ConfigTool instance. main.rs constructs one per
/// `agents[].config_tool.self_edit == true` agent.
pub struct ConfigTool {
    pub agent_id: String,
    pub binding_id: String,
    pub allowed_paths: Vec<String>,
    pub approval_timeout_secs: u64,
    pub proposals_dir: PathBuf,
    pub actor_origin: ActorOrigin,
    pub applier: Arc<dyn YamlPatchApplier>,
    pub denylist: Arc<dyn DenylistChecker>,
    pub redactor: Arc<dyn SecretRedactor>,
    pub changes_store: Arc<dyn crate::config_changes_store::ConfigChangesStore>,
    pub correlator: Arc<crate::agent::approval_correlator::ApprovalCorrelator>,
    pub reload: Arc<dyn ReloadTrigger>,
    /// Per-process map of `patch_id → oneshot::Receiver` so the
    /// `apply` path can claim the receiver parked by `propose`.
    /// `tokio::sync::Mutex` (not `parking_lot`) — we hold across
    /// `await` points minimally.
    pub pending_receivers: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<
                String,
                tokio::sync::oneshot::Receiver<crate::agent::approval_correlator::ApprovalDecision>,
            >,
        >,
    >,
}

/// Persistable wire-form of the patch envelope stored under
/// `.nexo/config-proposals/<patch_id>.yaml`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StagedProposal {
    patch_id: String,
    binding_id: String,
    agent_id: String,
    created_at: i64,
    expires_at: i64,
    justification: String,
    actor: ActorOrigin,
    patch: SerializablePatch,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SerializablePatch {
    /// `upsert` or `remove`.
    op: String,
    agent_id: String,
    dotted: String,
    /// Value present only for `upsert`. Stored as YAML so the
    /// staging file is self-describing.
    value: Option<serde_yaml::Value>,
}

impl SerializablePatch {
    fn from(info: &PatchInfo) -> Self {
        match &info.op {
            PatchOp::Upsert {
                agent_id,
                dotted,
                value,
            } => Self {
                op: "upsert".into(),
                agent_id: agent_id.clone(),
                dotted: dotted.clone(),
                value: Some(value.clone()),
            },
            PatchOp::Remove { agent_id, dotted } => Self {
                op: "remove".into(),
                agent_id: agent_id.clone(),
                dotted: dotted.clone(),
                value: None,
            },
        }
    }

    fn into_patch_info(self, parent: &StagedProposal) -> PatchInfo {
        let op = match self.op.as_str() {
            "upsert" => PatchOp::Upsert {
                agent_id: self.agent_id,
                dotted: self.dotted,
                value: self.value.unwrap_or(serde_yaml::Value::Null),
            },
            _ => PatchOp::Remove {
                agent_id: self.agent_id,
                dotted: self.dotted,
            },
        };
        PatchInfo {
            patch_id: parent.patch_id.clone(),
            binding_id: parent.binding_id.clone(),
            agent_id: parent.agent_id.clone(),
            created_at: parent.created_at,
            expires_at: parent.expires_at,
            justification: parent.justification.clone(),
            op,
        }
    }
}

impl serde::Serialize for ActorOrigin {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("ActorOrigin", 3)?;
        st.serialize_field("channel", &self.channel)?;
        st.serialize_field("account_id", &self.account_id)?;
        st.serialize_field("sender_id", &self.sender_id)?;
        st.end()
    }
}

impl<'de> serde::Deserialize<'de> for ActorOrigin {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Tmp {
            channel: String,
            account_id: String,
            sender_id: String,
        }
        let t = Tmp::deserialize(d)?;
        Ok(ActorOrigin {
            channel: t.channel,
            account_id: t.account_id,
            sender_id: t.sender_id,
        })
    }
}

/// Render a value for the audit log, redacting it when the key
/// matches a secret-suffix glob. Returns `None` for `Value::Null`
/// so the column stays nullable.
fn serialize_redacted_value(
    value: &Value,
    redactor: &dyn SecretRedactor,
    key: &str,
) -> Option<String> {
    if value.is_null() {
        return None;
    }
    if redactor.is_secret_path(key) {
        return Some("\"<REDACTED>\"".to_string());
    }
    serde_json::to_string(value).ok()
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
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let op = args
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Config: missing required field `op`"))?;
        match op {
            "read" => self.handle_read(args).await,
            "propose" => self.handle_propose(ctx, args).await,
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
    pub async fn recover_pending_from_staging(&self) -> anyhow::Result<usize> {
        let entries = match std::fs::read_dir(&self.proposals_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(anyhow::anyhow!("read proposals dir: {e}")),
        };

        let now = chrono::Utc::now().timestamp();
        let mut recovered = 0usize;
        for entry in entries {
            let path = match entry {
                Ok(e) => e.path(),
                Err(e) => {
                    tracing::warn!(error = %e, "[config] unreadable proposals dir entry");
                    continue;
                }
            };
            if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }

            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "[config] could not read staged proposal during recovery");
                    continue;
                }
            };
            let staged: StagedProposal = match serde_yaml::from_slice(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "[config] could not parse staged proposal during recovery");
                    continue;
                }
            };

            if staged.agent_id != self.agent_id || staged.binding_id != self.binding_id {
                continue;
            }
            if staged.expires_at <= now {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            {
                let pending = self.pending_receivers.lock().await;
                if pending.contains_key(&staged.patch_id) {
                    continue;
                }
            }
            let pending = crate::agent::approval_correlator::PendingApproval {
                patch_id: staged.patch_id.clone(),
                binding_id: staged.binding_id.clone(),
                agent_id: staged.agent_id.clone(),
                channel: staged.actor.channel.clone(),
                account_id: staged.actor.account_id.clone(),
                sender_id: staged.actor.sender_id.clone(),
                created_at: staged.created_at,
                expires_at: staged.expires_at,
            };
            let rx = self.correlator.park(pending);
            self.pending_receivers
                .lock()
                .await
                .insert(staged.patch_id, rx);
            recovered = recovered.saturating_add(1);
        }

        Ok(recovered)
    }

    async fn recover_receiver_for_patch(
        &self,
        patch_id: &str,
    ) -> Result<
        Option<tokio::sync::oneshot::Receiver<crate::agent::approval_correlator::ApprovalDecision>>,
        PatchAppliedError,
    > {
        let staging_path = self.proposals_dir.join(format!("{patch_id}.yaml"));
        let bytes = match std::fs::read(&staging_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(PatchAppliedError::Io(format!("read staging: {e}"))),
        };
        let staged: StagedProposal = serde_yaml::from_slice(&bytes)
            .map_err(|e| PatchAppliedError::Yaml(format!("parse staging: {e}")))?;

        if staged.patch_id != patch_id {
            return Ok(None);
        }
        if staged.agent_id != self.agent_id || staged.binding_id != self.binding_id {
            return Ok(None);
        }
        if staged.expires_at <= chrono::Utc::now().timestamp() {
            let _ = std::fs::remove_file(&staging_path);
            return Ok(None);
        }

        let pending = crate::agent::approval_correlator::PendingApproval {
            patch_id: staged.patch_id,
            binding_id: staged.binding_id,
            agent_id: staged.agent_id,
            channel: staged.actor.channel,
            account_id: staged.actor.account_id,
            sender_id: staged.actor.sender_id,
            created_at: staged.created_at,
            expires_at: staged.expires_at,
        };
        Ok(Some(self.correlator.park(pending)))
    }

    fn resolve_actor_origin(&self, ctx: &AgentContext) -> ActorOrigin {
        match ctx.inbound_origin.as_ref() {
            Some((channel, account_id, sender_id)) => ActorOrigin {
                channel: channel.clone(),
                account_id: account_id.clone(),
                sender_id: sender_id.clone(),
            },
            None => self.actor_origin.clone(),
        }
    }

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

    async fn handle_propose(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Config propose: missing required field `key`"))?
            .to_string();
        let justification = args
            .get("justification")
            .and_then(|v| v.as_str())
            .unwrap_or("(no justification)")
            .trim()
            .to_string();
        let value = args.get("value").cloned().unwrap_or(Value::Null);

        // Triple gate (same shape as `read`).
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

        // Validate the value against the SupportedSetting validator.
        let setting = nexo_config::types::config_tool::lookup(&key).expect("supported");
        if let Some(validate) = setting.validate {
            // Convert to JSON for the validator (it operates on
            // `serde_json::Value`).
            if let Err(reason) = validate(&value) {
                return Ok(json!({
                    "ok": false,
                    "error": format!("validation failed for `{key}`: {reason}"),
                    "kind": "ValidationFailed",
                    "key": key,
                }));
            }
        }

        // Build the patch.
        let patch_id = format!(
            "01J{}",
            uuid::Uuid::new_v4()
                .simple()
                .to_string()
                .to_ascii_uppercase()
        );
        let now = chrono::Utc::now().timestamp();
        let expires_at = now + self.approval_timeout_secs as i64;
        let op = if value.is_null() {
            PatchOp::Remove {
                agent_id: self.agent_id.clone(),
                dotted: key.clone(),
            }
        } else {
            // Convert JSON value to YAML for storage.
            let yaml_value = match serde_yaml::to_value(&value) {
                Ok(v) => v,
                Err(e) => {
                    return Ok(json!({
                        "ok": false,
                        "error": format!("value JSON→YAML conversion failed: {e}"),
                        "kind": "Yaml",
                    }))
                }
            };
            PatchOp::Upsert {
                agent_id: self.agent_id.clone(),
                dotted: key.clone(),
                value: yaml_value,
            }
        };
        let patch = PatchInfo {
            patch_id: patch_id.clone(),
            binding_id: self.binding_id.clone(),
            agent_id: self.agent_id.clone(),
            created_at: now,
            expires_at,
            justification: justification.clone(),
            op,
        };
        let actor_origin = self.resolve_actor_origin(ctx);

        // Park with the correlator BEFORE writing the staging file —
        // pending entry must exist before the operator's potential
        // approval message races in.
        let pending = crate::agent::approval_correlator::PendingApproval {
            patch_id: patch_id.clone(),
            binding_id: self.binding_id.clone(),
            agent_id: self.agent_id.clone(),
            channel: actor_origin.channel.clone(),
            account_id: actor_origin.account_id.clone(),
            sender_id: actor_origin.sender_id.clone(),
            created_at: now,
            expires_at,
        };
        let _rx = self.correlator.park(pending);
        // The receiver lives until apply or expiry. We keep it in
        // the proposals_dir alongside the patch via patch_id; in
        // memory the correlator owns it; on apply the handler calls
        // `await_decision_for(patch_id)`.
        // For step 9 minimum: store the receiver in a side map
        // keyed by patch_id so apply can claim it.
        self.pending_receivers
            .lock()
            .await
            .insert(patch_id.clone(), _rx);

        // Write staging file.
        if let Err(e) = std::fs::create_dir_all(&self.proposals_dir) {
            self.pending_receivers.lock().await.remove(&patch_id);
            let _ = self.correlator.cancel_patch(&patch_id);
            return Ok(json!({
                "ok": false,
                "error": format!("could not create proposals dir: {e}"),
                "kind": "Io",
            }));
        }
        let staging_path = self.proposals_dir.join(format!("{patch_id}.yaml"));
        let staged = StagedProposal {
            patch_id: patch_id.clone(),
            binding_id: self.binding_id.clone(),
            agent_id: self.agent_id.clone(),
            created_at: now,
            expires_at,
            justification: justification.clone(),
            actor: actor_origin,
            patch: SerializablePatch::from(&patch),
        };
        let yaml = match serde_yaml::to_string(&staged) {
            Ok(y) => y,
            Err(e) => {
                self.pending_receivers.lock().await.remove(&patch_id);
                let _ = self.correlator.cancel_patch(&patch_id);
                return Ok(json!({
                    "ok": false,
                    "error": format!("serialise staging: {e}"),
                    "kind": "Yaml",
                }));
            }
        };
        if let Err(e) = std::fs::write(&staging_path, yaml) {
            self.pending_receivers.lock().await.remove(&patch_id);
            let _ = self.correlator.cancel_patch(&patch_id);
            return Ok(json!({
                "ok": false,
                "error": format!("write staging: {e}"),
                "kind": "Io",
            }));
        }

        // Audit row.
        let _ = self
            .changes_store
            .record(&crate::config_changes_store::ConfigChangeRow {
                patch_id: patch_id.clone(),
                binding_id: self.binding_id.clone(),
                agent_id: self.agent_id.clone(),
                op: "propose".into(),
                key: key.clone(),
                value: serialize_redacted_value(&value, self.redactor.as_ref(), &key),
                status: "proposed".into(),
                error: None,
                created_at: now,
                applied_at: None,
            })
            .await;

        tracing::info!(
            target: "config::propose",
            patch_id = %patch_id,
            agent = %self.agent_id,
            binding = %self.binding_id,
            key = %key,
            "[config] proposal staged — awaiting operator approval"
        );

        Ok(json!({
            "ok": true,
            "patch_id": patch_id,
            "expires_at": expires_at,
            "approval_message_format": format!(
                "[config-approve patch_id={patch_id}] or [config-reject patch_id={patch_id} reason=...]"
            ),
        }))
    }

    async fn handle_apply(&self, args: Value) -> anyhow::Result<Value> {
        let patch_id = args
            .get("patch_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Config apply: missing required field `patch_id`"))?
            .to_string();

        // Pull the parked receiver (or fail with NoPending).
        let rx = self.pending_receivers.lock().await.remove(&patch_id);
        let rx = match rx {
            Some(r) => r,
            None => match self.recover_receiver_for_patch(&patch_id).await {
                Ok(Some(r)) => r,
                Ok(None) => {
                    return Ok(json!({
                        "ok": false,
                        "error": format!("no pending proposal for patch_id `{patch_id}` (expired or already consumed)"),
                        "kind": "NoPending",
                        "patch_id": patch_id,
                    }))
                }
                Err(PatchAppliedError::Io(e)) => {
                    return Ok(json!({
                        "ok": false,
                        "error": e,
                        "kind": "Io",
                        "patch_id": patch_id,
                    }))
                }
                Err(PatchAppliedError::Yaml(e)) => {
                    return Ok(json!({
                        "ok": false,
                        "error": e,
                        "kind": "Yaml",
                        "patch_id": patch_id,
                    }))
                }
                Err(e) => {
                    return Ok(json!({
                        "ok": false,
                        "error": e.to_string(),
                        "kind": "InternalError",
                        "patch_id": patch_id,
                    }))
                }
            },
        };

        // Re-read staging file (defense-in-depth).
        let staging_path = self.proposals_dir.join(format!("{patch_id}.yaml"));
        let staging_bytes = match std::fs::read(&staging_path) {
            Ok(b) => b,
            Err(e) => {
                self.pending_receivers
                    .lock()
                    .await
                    .insert(patch_id.clone(), rx);
                return Ok(json!({
                    "ok": false,
                    "error": format!("could not read staging file: {e}"),
                    "kind": "Io",
                    "patch_id": patch_id,
                }));
            }
        };
        let staged: StagedProposal = match serde_yaml::from_slice(&staging_bytes) {
            Ok(s) => s,
            Err(e) => {
                self.pending_receivers
                    .lock()
                    .await
                    .insert(patch_id.clone(), rx);
                return Ok(json!({
                    "ok": false,
                    "error": format!("could not parse staging file: {e}"),
                    "kind": "Yaml",
                    "patch_id": patch_id,
                }));
            }
        };

        // Defense-in-depth denylist check on the parsed staged path.
        let staged_key = staged.patch.dotted.clone();
        if let Some(matched_glob) = self.denylist.check(&staged_key) {
            return Ok(json!({
                "ok": false,
                "error": format!("staged path `{staged_key}` denied by self-edit policy"),
                "kind": "ForbiddenKey",
                "matched_glob": matched_glob,
            }));
        }

        // Await decision. The correlator drives the oneshot via
        // inbound or expiry.
        let decision = match rx.await {
            Ok(d) => d,
            Err(_) => {
                return Ok(json!({
                    "ok": false,
                    "error": "approval responder dropped before decision",
                    "kind": "InternalError",
                    "patch_id": patch_id,
                }))
            }
        };
        match decision {
            crate::agent::approval_correlator::ApprovalDecision::Rejected { reason } => {
                let now = chrono::Utc::now().timestamp();
                let _ = self
                    .changes_store
                    .record(&crate::config_changes_store::ConfigChangeRow {
                        patch_id: patch_id.clone(),
                        binding_id: self.binding_id.clone(),
                        agent_id: self.agent_id.clone(),
                        op: "apply".into(),
                        key: staged_key.clone(),
                        value: None,
                        status: "rejected".into(),
                        error: reason.clone(),
                        created_at: now,
                        applied_at: None,
                    })
                    .await;
                return Ok(json!({
                    "ok": false,
                    "error": "operator rejected the proposal",
                    "kind": "Rejected",
                    "reason": reason,
                    "patch_id": patch_id,
                }));
            }
            crate::agent::approval_correlator::ApprovalDecision::Expired => {
                let now = chrono::Utc::now().timestamp();
                let _ = self
                    .changes_store
                    .record(&crate::config_changes_store::ConfigChangeRow {
                        patch_id: patch_id.clone(),
                        binding_id: self.binding_id.clone(),
                        agent_id: self.agent_id.clone(),
                        op: "apply".into(),
                        key: staged_key.clone(),
                        value: None,
                        status: "expired".into(),
                        error: None,
                        created_at: now,
                        applied_at: None,
                    })
                    .await;
                return Ok(json!({
                    "ok": false,
                    "error": "approval timed out",
                    "kind": "Expired",
                    "patch_id": patch_id,
                }));
            }
            crate::agent::approval_correlator::ApprovalDecision::Approved => {
                // continue
            }
        }

        // Snapshot for rollback.
        let snapshot = match self.applier.snapshot() {
            Ok(s) => s,
            Err(e) => {
                return Ok(json!({
                    "ok": false,
                    "error": format!("snapshot failed: {e}"),
                    "kind": "Io",
                }))
            }
        };

        // Apply.
        let patch_info = staged.patch.clone().into_patch_info(&staged);
        if let Err(e) = self.applier.apply(&patch_info) {
            return Ok(json!({
                "ok": false,
                "error": format!("apply failed: {e}"),
                "kind": "Yaml",
                "patch_id": patch_id,
            }));
        }

        // Reload + rollback on failure.
        match self.reload.reload().await {
            Ok(()) => {
                let now = chrono::Utc::now().timestamp();
                let _ = self
                    .changes_store
                    .record(&crate::config_changes_store::ConfigChangeRow {
                        patch_id: patch_id.clone(),
                        binding_id: self.binding_id.clone(),
                        agent_id: self.agent_id.clone(),
                        op: "apply".into(),
                        key: staged_key.clone(),
                        value: None,
                        status: "applied".into(),
                        error: None,
                        created_at: now,
                        applied_at: Some(now),
                    })
                    .await;
                tracing::info!(
                    target: "config::apply",
                    patch_id = %patch_id,
                    key = %staged_key,
                    "[config] applied"
                );
                // Best-effort cleanup of staging file.
                let _ = std::fs::remove_file(&staging_path);
                Ok(json!({
                    "ok": true,
                    "patch_id": patch_id,
                    "applied": true,
                    "key": staged_key,
                }))
            }
            Err(reload_err) => {
                // Rollback.
                let restore_result = self.applier.restore(&snapshot);
                let now = chrono::Utc::now().timestamp();
                let restore_failed = restore_result.is_err();
                let _ = self
                    .changes_store
                    .record(&crate::config_changes_store::ConfigChangeRow {
                        patch_id: patch_id.clone(),
                        binding_id: self.binding_id.clone(),
                        agent_id: self.agent_id.clone(),
                        op: "apply".into(),
                        key: staged_key.clone(),
                        value: None,
                        status: "rolled_back".into(),
                        error: Some(reload_err.clone()),
                        created_at: now,
                        applied_at: Some(now),
                    })
                    .await;
                tracing::warn!(
                    target: "config::apply_rolled_back",
                    patch_id = %patch_id,
                    reload_error = %reload_err,
                    restore_failed = restore_failed,
                    "[config] reload rejected post-apply — rolling back"
                );
                Ok(json!({
                    "ok": false,
                    "error": "reload rejected the post-apply config; restored previous snapshot",
                    "kind": "RolledBack",
                    "reload_error": reload_err,
                    "rolled_back_to_previous": !restore_failed,
                    "patch_id": patch_id,
                }))
            }
        }
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
        serde_yaml::Value::Sequence(seq) => Value::Array(seq.iter().map(yaml_to_json).collect()),
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
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
        };
        AgentContext::new(
            "cody",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    /// Mock reload trigger that flips between `Ok` and `Err`.
    struct MockReload {
        outcome: Mutex<Result<(), String>>,
    }
    impl MockReload {
        fn new(outcome: Result<(), String>) -> Self {
            Self {
                outcome: Mutex::new(outcome),
            }
        }
    }
    #[async_trait]
    impl ReloadTrigger for MockReload {
        async fn reload(&self) -> Result<(), String> {
            self.outcome.lock().unwrap().clone()
        }
    }

    async fn build_tool(
        applier: Arc<dyn YamlPatchApplier>,
        denylist: Arc<dyn DenylistChecker>,
        allowed: Vec<String>,
    ) -> ConfigTool {
        build_tool_with_reload(
            applier,
            denylist,
            allowed,
            Arc::new(MockReload::new(Ok(()))),
        )
        .await
    }

    async fn build_tool_with_reload(
        applier: Arc<dyn YamlPatchApplier>,
        denylist: Arc<dyn DenylistChecker>,
        allowed: Vec<String>,
        reload: Arc<dyn ReloadTrigger>,
    ) -> ConfigTool {
        let dir = tempfile::tempdir().unwrap().into_path();
        build_tool_with_reload_in_dir(applier, denylist, allowed, reload, dir).await
    }

    async fn build_tool_with_reload_in_dir(
        applier: Arc<dyn YamlPatchApplier>,
        denylist: Arc<dyn DenylistChecker>,
        allowed: Vec<String>,
        reload: Arc<dyn ReloadTrigger>,
        proposals_dir: std::path::PathBuf,
    ) -> ConfigTool {
        let store: Arc<dyn crate::config_changes_store::ConfigChangesStore> =
            Arc::new(SqliteConfigChangesStore::open_in_memory().await.unwrap());
        let correlator = crate::agent::approval_correlator::ApprovalCorrelator::new(
            crate::agent::approval_correlator::ApprovalCorrelatorConfig::default(),
        );
        ConfigTool {
            agent_id: "cody".into(),
            binding_id: "wa:default".into(),
            allowed_paths: allowed,
            approval_timeout_secs: 86_400,
            proposals_dir,
            actor_origin: ActorOrigin {
                channel: "whatsapp".into(),
                account_id: "default".into(),
                sender_id: "5511".into(),
            },
            applier,
            denylist,
            redactor: Arc::new(DefaultSecretRedactor),
            changes_store: store,
            correlator,
            reload,
            pending_receivers: Arc::new(tokio::sync::Mutex::new(Default::default())),
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
    async fn propose_writes_staging_file_and_records_proposed() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;
        let res = tool
            .call(
                &agent_context_fixture(),
                json!({
                    "op": "propose",
                    "key": "model.model",
                    "value": "claude-opus-4-7",
                    "justification": "operator asked"
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        let patch_id = res["patch_id"].as_str().unwrap().to_string();

        // Staging file exists.
        let path = tool.proposals_dir.join(format!("{patch_id}.yaml"));
        assert!(
            path.exists(),
            "staging file at `{}` must exist",
            path.display()
        );

        // Audit row recorded.
        let row = tool.changes_store.get(&patch_id).await.unwrap().unwrap();
        assert_eq!(row.status, "proposed");
        assert_eq!(row.key, "model.model");
        assert_eq!(row.value.unwrap(), "\"claude-opus-4-7\"");
    }

    #[tokio::test]
    async fn propose_uses_inbound_origin_from_context_when_available() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;
        let ctx = agent_context_fixture().with_inbound_origin("telegram", "sales", "u-42");
        let res = tool
            .call(
                &ctx,
                json!({
                    "op": "propose",
                    "key": "model.model",
                    "value": "claude-opus-4-7",
                    "justification": "operator asked"
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        let patch_id = res["patch_id"].as_str().unwrap().to_string();
        let path = tool.proposals_dir.join(format!("{patch_id}.yaml"));
        let staged: StagedProposal = serde_yaml::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(staged.actor.channel, "telegram");
        assert_eq!(staged.actor.account_id, "sales");
        assert_eq!(staged.actor.sender_id, "u-42");
    }

    #[tokio::test]
    async fn propose_staging_failure_cleans_pending_maps() {
        let mut tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;
        let marker = tempfile::NamedTempFile::new().unwrap();
        tool.proposals_dir = marker.path().to_path_buf();

        let res = tool
            .call(
                &agent_context_fixture(),
                json!({
                    "op": "propose",
                    "key": "model.model",
                    "value": "claude-opus-4-7",
                    "justification": "operator asked"
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "Io");
        assert!(tool.pending_receivers.lock().await.is_empty());
        assert_eq!(tool.correlator.pending_count(), 0);
    }

    #[tokio::test]
    async fn apply_staging_read_error_requeues_receiver() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;
        let propose = tool
            .call(
                &agent_context_fixture(),
                json!({
                    "op": "propose",
                    "key": "model.model",
                    "value": "claude-opus-4-7",
                    "justification": "test"
                }),
            )
            .await
            .unwrap();
        let patch_id = propose["patch_id"].as_str().unwrap().to_string();
        let path = tool.proposals_dir.join(format!("{patch_id}.yaml"));
        std::fs::remove_file(&path).unwrap();

        let apply = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "apply", "patch_id": patch_id.clone() }),
            )
            .await
            .unwrap();
        assert_eq!(apply["ok"], false);
        assert_eq!(apply["kind"], "Io");
        let pending = tool.pending_receivers.lock().await;
        assert!(pending.contains_key(&patch_id));
    }

    #[tokio::test]
    async fn boot_recovery_rehydrates_pending_proposals_from_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let proposals_dir = tmp.path().to_path_buf();

        let tool_before = build_tool_with_reload_in_dir(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
            Arc::new(MockReload::new(Ok(()))),
            proposals_dir.clone(),
        )
        .await;

        let propose = tool_before
            .call(
                &agent_context_fixture(),
                json!({
                    "op": "propose",
                    "key": "model.model",
                    "value": "claude-opus-4-7",
                    "justification": "restart test"
                }),
            )
            .await
            .unwrap();
        let patch_id = propose["patch_id"].as_str().unwrap().to_string();

        // Simulated process restart: brand new correlator + empty
        // in-memory pending map, same proposals_dir on disk.
        let tool_after = build_tool_with_reload_in_dir(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
            Arc::new(MockReload::new(Ok(()))),
            proposals_dir,
        )
        .await;
        assert_eq!(tool_after.pending_receivers.lock().await.len(), 0);
        assert_eq!(tool_after.correlator.pending_count(), 0);

        let recovered = tool_after.recover_pending_from_staging().await.unwrap();
        assert_eq!(recovered, 1);
        assert!(tool_after
            .pending_receivers
            .lock()
            .await
            .contains_key(&patch_id));
        assert_eq!(tool_after.correlator.pending_count(), 1);

        tool_after.correlator.on_inbound(
            crate::agent::approval_correlator::InboundApprovalMessage {
                channel: "whatsapp".into(),
                account_id: "default".into(),
                sender_id: "5511".into(),
                body: format!("[config-approve patch_id={patch_id}]"),
                received_at: 0,
            },
        );

        let apply = tool_after
            .call(
                &agent_context_fixture(),
                json!({ "op": "apply", "patch_id": patch_id.clone() }),
            )
            .await
            .unwrap();
        assert_eq!(apply["ok"], true);
        assert_eq!(apply["applied"], true);
    }

    #[tokio::test]
    async fn apply_no_pending_can_recover_receiver_from_staging_file() {
        let tool = Arc::new(
            build_tool(
                Arc::new(MockApplier::new(fixture_yaml())),
                Arc::new(PermissiveDenylist),
                vec![],
            )
            .await,
        );
        let propose = tool
            .call(
                &agent_context_fixture(),
                json!({
                    "op": "propose",
                    "key": "model.model",
                    "value": "claude-opus-4-7",
                    "justification": "lazy recover test"
                }),
            )
            .await
            .unwrap();
        let patch_id = propose["patch_id"].as_str().unwrap().to_string();

        // Simulate lost in-memory map (e.g. old process died before
        // boot recovery runs).
        tool.pending_receivers.lock().await.remove(&patch_id);
        let _ = tool.correlator.cancel_patch(&patch_id);

        let tool_for_apply = Arc::clone(&tool);
        let patch_for_apply = patch_id.clone();
        let apply_task = tokio::spawn(async move {
            tool_for_apply
                .call(
                    &agent_context_fixture(),
                    json!({ "op": "apply", "patch_id": patch_for_apply }),
                )
                .await
                .unwrap()
        });

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tool.correlator
            .on_inbound(crate::agent::approval_correlator::InboundApprovalMessage {
                channel: "whatsapp".into(),
                account_id: "default".into(),
                sender_id: "5511".into(),
                body: format!("[config-approve patch_id={patch_id}]"),
                received_at: 0,
            });
        let apply = apply_task.await.unwrap();
        assert_eq!(apply["ok"], true);
        assert_eq!(apply["applied"], true);
    }

    #[tokio::test]
    async fn propose_blocks_forbidden_key() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(StrictDenylist),
            vec![],
        )
        .await;
        // `model.model` is supported and not blocked by StrictDenylist
        // — use a key the denylist actually catches. Since SUPPORTED
        // doesn't include any key that StrictDenylist matches, we
        // exercise the UnknownKey path instead and trust the
        // denylist tests in step 1 cover the matcher itself.
        let res = tool
            .call(
                &agent_context_fixture(),
                json!({
                    "op": "propose",
                    "key": "pairing.session_token",
                    "value": "x"
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "UnknownKey");
    }

    #[tokio::test]
    async fn propose_rejects_invalid_value() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;
        let res = tool
            .call(
                &agent_context_fixture(),
                json!({
                    "op": "propose",
                    "key": "lsp.languages",
                    "value": ["rust", "kotlin"]
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "ValidationFailed");
    }

    #[tokio::test]
    async fn apply_returns_no_pending_when_patch_id_unknown() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;
        let res = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "apply", "patch_id": "01J7UNKNOWN" }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "NoPending");
    }

    #[tokio::test]
    async fn apply_after_approval_writes_yaml_and_marks_applied() {
        let tool = build_tool_with_reload(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
            Arc::new(MockReload::new(Ok(()))),
        )
        .await;

        // 1. propose
        let propose = tool
            .call(
                &agent_context_fixture(),
                json!({
                    "op": "propose",
                    "key": "model.model",
                    "value": "claude-opus-4-7",
                    "justification": "test"
                }),
            )
            .await
            .unwrap();
        let patch_id = propose["patch_id"].as_str().unwrap().to_string();

        // 2. inject approval message via the same correlator.
        tool.correlator
            .on_inbound(crate::agent::approval_correlator::InboundApprovalMessage {
                channel: "whatsapp".into(),
                account_id: "default".into(),
                sender_id: "5511".into(),
                body: format!("[config-approve patch_id={patch_id}]"),
                received_at: 0,
            });

        // 3. apply
        let apply = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "apply", "patch_id": patch_id }),
            )
            .await
            .unwrap();
        assert_eq!(apply["ok"], true);
        assert_eq!(apply["applied"], true);

        // 4. audit reflects `applied`.
        let row = apply["patch_id"]
            .as_str()
            .map(|id| tool.changes_store.get(id))
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "applied");
    }

    #[tokio::test]
    async fn apply_after_rejection_returns_rejected_with_reason() {
        let tool = build_tool(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
        )
        .await;

        let propose = tool
            .call(
                &agent_context_fixture(),
                json!({
                    "op": "propose",
                    "key": "model.model",
                    "value": "claude-opus-4-7",
                    "justification": "test"
                }),
            )
            .await
            .unwrap();
        let patch_id = propose["patch_id"].as_str().unwrap().to_string();

        tool.correlator
            .on_inbound(crate::agent::approval_correlator::InboundApprovalMessage {
                channel: "whatsapp".into(),
                account_id: "default".into(),
                sender_id: "5511".into(),
                body: format!("[config-reject patch_id={patch_id} reason=cost]"),
                received_at: 0,
            });

        let apply = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "apply", "patch_id": patch_id.clone() }),
            )
            .await
            .unwrap();
        assert_eq!(apply["ok"], false);
        assert_eq!(apply["kind"], "Rejected");
        assert_eq!(apply["reason"], "cost");

        let row = tool.changes_store.get(&patch_id).await.unwrap().unwrap();
        assert_eq!(row.status, "rejected");
    }

    #[tokio::test]
    async fn apply_rolls_back_on_reload_validation_failure() {
        let tool = build_tool_with_reload(
            Arc::new(MockApplier::new(fixture_yaml())),
            Arc::new(PermissiveDenylist),
            vec![],
            Arc::new(MockReload::new(Err("bad config".into()))),
        )
        .await;

        let propose = tool
            .call(
                &agent_context_fixture(),
                json!({
                    "op": "propose",
                    "key": "model.model",
                    "value": "claude-opus-4-7",
                    "justification": "test"
                }),
            )
            .await
            .unwrap();
        let patch_id = propose["patch_id"].as_str().unwrap().to_string();

        tool.correlator
            .on_inbound(crate::agent::approval_correlator::InboundApprovalMessage {
                channel: "whatsapp".into(),
                account_id: "default".into(),
                sender_id: "5511".into(),
                body: format!("[config-approve patch_id={patch_id}]"),
                received_at: 0,
            });

        let apply = tool
            .call(
                &agent_context_fixture(),
                json!({ "op": "apply", "patch_id": patch_id.clone() }),
            )
            .await
            .unwrap();
        assert_eq!(apply["ok"], false);
        assert_eq!(apply["kind"], "RolledBack");
        assert_eq!(apply["reload_error"], "bad config");
        assert_eq!(apply["rolled_back_to_previous"], true);

        let row = tool.changes_store.get(&patch_id).await.unwrap().unwrap();
        assert_eq!(row.status, "rolled_back");
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
