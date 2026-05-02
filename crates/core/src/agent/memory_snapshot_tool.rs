//! `memory_snapshot` native tool.
//!
//! Lets the LLM trigger a write-only point-in-time snapshot of its
//! own memory. Restore is intentionally **not** exposed as a tool —
//! reverting an agent's state is operator-only (`nexo memory
//! restore`) so prompt injection cannot drive a destructive rollback.
//!
//! Gates riding on top of this tool:
//!
//! - Plan-mode: registered in `MUTATING_TOOLS` so it is denied while
//!   the agent is in plan mode.
//! - Per-binding allowlist: gated by `EffectiveBindingPolicy::allowed_tools`.
//! - Auto-approve: when `auto_approve` is off, each call goes through
//!   the standard human-confirm path.
//! - Per-binding rate limit: keyed `(tenant, "memory_snapshot")`.
//! - Deferred schema: `deferred() = true` keeps the verbose schema
//!   out of the default tool catalog (Phase 79.2 token budget).

use std::sync::Arc;

use async_trait::async_trait;
use nexo_llm::ToolDef;
use nexo_memory_snapshot::{
    MemorySnapshotter, SnapshotRequest,
};
use serde_json::{json, Value};

use super::context::AgentContext;
use super::tool_registry::ToolHandler;

const TOOL_NAME: &str = "memory_snapshot";
const DEFAULT_TENANT: &str = "default";

pub struct MemorySnapshotTool {
    snapshotter: Arc<dyn MemorySnapshotter>,
    /// Tenant string passed to every snapshot. Single-tenant
    /// deployments leave this `"default"`; SaaS deployments wire the
    /// per-binding tenant at boot.
    tenant: String,
    /// Whether `redact_secrets` defaults to `true` for tool-driven
    /// snapshots. Mirrors `memory.snapshot.redact_secrets_default`.
    redact_secrets_default: bool,
}

impl MemorySnapshotTool {
    pub fn new(snapshotter: Arc<dyn MemorySnapshotter>) -> Self {
        Self {
            snapshotter,
            tenant: DEFAULT_TENANT.to_string(),
            redact_secrets_default: true,
        }
    }

    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = tenant.into();
        self
    }

    pub fn with_redact_secrets_default(mut self, redact: bool) -> Self {
        self.redact_secrets_default = redact;
        self
    }

    /// Stable wire name + token-light description. Schema declares an
    /// optional `label` so the LLM can mark the bundle (e.g. "before
    /// prod rollout") and an optional `redact_secrets` override.
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: TOOL_NAME.into(),
            description: "Capture a point-in-time snapshot of this agent's memory \
                (git memdir + SQLite stores + state). Returns the new bundle's id, \
                size, and SHA-256. Operators run `nexo memory list` / `verify` / \
                `restore` against the result. Restore is operator-only and is NOT \
                callable from this tool. Use sparingly — each call writes a \
                full bundle to disk."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "label": {
                        "type": "string",
                        "description": "Free-form short label (e.g. 'pre-rollout'). Stored in the manifest.",
                        "maxLength": 200
                    },
                    "redact_secrets": {
                        "type": "boolean",
                        "description": "Override the runtime default for secret redaction over text artifacts."
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    /// Phase 79.2 — defer the schema from the default tool catalog so
    /// it only loads when the LLM explicitly asks for it via
    /// `tool_search`. This tool is high-value but rarely-used; the
    /// schema is verbose enough to be worth deferring.
    pub fn is_deferred() -> bool {
        true
    }
}

#[async_trait]
impl ToolHandler for MemorySnapshotTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let label = args
            .get("label")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let redact_secrets = args
            .get("redact_secrets")
            .and_then(|v| v.as_bool())
            .unwrap_or(self.redact_secrets_default);

        let req = SnapshotRequest {
            agent_id: ctx.agent_id.clone(),
            tenant: self.tenant.clone(),
            label,
            redact_secrets,
            encrypt: None,
            created_by: "tool".into(),
        };

        let meta = self
            .snapshotter
            .snapshot(req)
            .await
            .map_err(|e| anyhow::anyhow!("memory_snapshot failed: {e}"))?;

        Ok(json!({
            "snapshot_id": meta.id.to_string(),
            "bundle_path": meta.bundle_path.display().to_string(),
            "bundle_size_bytes": meta.bundle_size_bytes,
            "bundle_sha256": meta.bundle_sha256,
            "created_at_ms": meta.created_at_ms,
            "redactions_applied": meta.redactions_applied,
            "encrypted": meta.encrypted,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use async_trait::async_trait;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig,
    };
    use nexo_memory_snapshot::{
        AgentId, RestoreReport, RestoreRequest, SnapshotDiff, SnapshotError, SnapshotId,
        SnapshotMeta, VerifyReport,
    };
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    struct CapturingSnapshotter {
        last_label: Mutex<Option<String>>,
        last_redact: Mutex<Option<bool>>,
        last_created_by: Mutex<Option<String>>,
    }

    #[async_trait]
    impl MemorySnapshotter for CapturingSnapshotter {
        async fn snapshot(
            &self,
            req: SnapshotRequest,
        ) -> Result<SnapshotMeta, SnapshotError> {
            *self.last_label.lock().unwrap() = req.label.clone();
            *self.last_redact.lock().unwrap() = Some(req.redact_secrets);
            *self.last_created_by.lock().unwrap() = Some(req.created_by.clone());
            Ok(SnapshotMeta {
                id: SnapshotId::new(),
                agent_id: req.agent_id,
                tenant: req.tenant,
                label: req.label,
                created_at_ms: 1_700_000_000_000,
                bundle_path: PathBuf::from("/tmp/x.tar.zst"),
                bundle_size_bytes: 4096,
                bundle_sha256: "ab".repeat(32),
                git_oid: None,
                schema_versions: nexo_memory_snapshot::SchemaVersions::CURRENT,
                encrypted: false,
                redactions_applied: req.redact_secrets,
            })
        }
        async fn restore(
            &self,
            _req: RestoreRequest,
        ) -> Result<RestoreReport, SnapshotError> {
            unimplemented!()
        }
        async fn list(
            &self,
            _agent_id: &AgentId,
            _tenant: &str,
        ) -> Result<Vec<SnapshotMeta>, SnapshotError> {
            Ok(Vec::new())
        }
        async fn diff(
            &self,
            _agent_id: &AgentId,
            _tenant: &str,
            _a: SnapshotId,
            _b: SnapshotId,
        ) -> Result<SnapshotDiff, SnapshotError> {
            unimplemented!()
        }
        async fn verify(&self, _bundle: &Path) -> Result<VerifyReport, SnapshotError> {
            unimplemented!()
        }
        async fn delete(
            &self,
            _agent_id: &AgentId,
            _tenant: &str,
            _id: SnapshotId,
        ) -> Result<(), SnapshotError> {
            Ok(())
        }
        async fn export(
            &self,
            _agent_id: &AgentId,
            _tenant: &str,
            _id: SnapshotId,
            target: &Path,
        ) -> Result<PathBuf, SnapshotError> {
            Ok(target.to_path_buf())
        }
    }

    fn ctx() -> AgentContext {
        let cfg = Arc::new(AgentConfig {
            id: "ana".into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m".into(),
            },
            plugins: vec![],
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: vec![],
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: Default::default(),
            workspace_git: Default::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            outbound_allowlist: Default::default(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
            repl: Default::default(),
            auto_dream: None,
            assistant_mode: None,
            away_summary: None,
            brief: None,
            channels: None,
            auto_approve: false,
            extract_memories: None,
            event_subscribers: Vec::new(),
            tenant_id: None,
            extensions_config: std::collections::BTreeMap::new(),
        });
        let broker = AnyBroker::local();
        let sessions = Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 20));
        AgentContext::new("ana", cfg, broker, sessions)
    }

    #[tokio::test]
    async fn tool_def_is_stable() {
        let def = MemorySnapshotTool::tool_def();
        assert_eq!(def.name, "memory_snapshot");
        assert!(def.description.contains("snapshot"));
        // Schema rejects unknown fields.
        let schema = def.parameters.to_string();
        assert!(schema.contains("\"label\""));
        assert!(schema.contains("\"redact_secrets\""));
        assert!(schema.contains("\"additionalProperties\":false"));
    }

    #[tokio::test]
    async fn is_deferred_returns_true_for_token_budget() {
        assert!(MemorySnapshotTool::is_deferred());
    }

    #[tokio::test]
    async fn call_dispatches_with_label_and_redact_default() {
        let inner = Arc::new(CapturingSnapshotter {
            last_label: Mutex::new(None),
            last_redact: Mutex::new(None),
            last_created_by: Mutex::new(None),
        });
        let tool = MemorySnapshotTool::new(inner.clone() as Arc<dyn MemorySnapshotter>);
        let out = tool
            .call(&ctx(), json!({ "label": "manual" }))
            .await
            .unwrap();
        assert!(out.get("snapshot_id").is_some());
        assert_eq!(out.get("bundle_size_bytes").and_then(|v| v.as_u64()), Some(4096));
        assert_eq!(
            inner.last_label.lock().unwrap().as_deref(),
            Some("manual")
        );
        assert_eq!(*inner.last_redact.lock().unwrap(), Some(true));
        assert_eq!(
            inner.last_created_by.lock().unwrap().as_deref(),
            Some("tool")
        );
    }

    #[tokio::test]
    async fn call_honors_redact_override_from_args() {
        let inner = Arc::new(CapturingSnapshotter {
            last_label: Mutex::new(None),
            last_redact: Mutex::new(None),
            last_created_by: Mutex::new(None),
        });
        let tool = MemorySnapshotTool::new(inner.clone() as Arc<dyn MemorySnapshotter>);
        tool.call(&ctx(), json!({ "redact_secrets": false }))
            .await
            .unwrap();
        assert_eq!(*inner.last_redact.lock().unwrap(), Some(false));
    }

    #[tokio::test]
    async fn with_tenant_threads_through_to_request() {
        let inner = Arc::new(CapturingSnapshotter {
            last_label: Mutex::new(None),
            last_redact: Mutex::new(None),
            last_created_by: Mutex::new(None),
        });
        struct TenantCapture {
            last_tenant: Mutex<Option<String>>,
        }
        #[async_trait]
        impl MemorySnapshotter for TenantCapture {
            async fn snapshot(
                &self,
                req: SnapshotRequest,
            ) -> Result<SnapshotMeta, SnapshotError> {
                *self.last_tenant.lock().unwrap() = Some(req.tenant.clone());
                Ok(SnapshotMeta {
                    id: SnapshotId::new(),
                    agent_id: req.agent_id,
                    tenant: req.tenant,
                    label: None,
                    created_at_ms: 0,
                    bundle_path: PathBuf::from("/x"),
                    bundle_size_bytes: 0,
                    bundle_sha256: String::new(),
                    git_oid: None,
                    schema_versions: nexo_memory_snapshot::SchemaVersions::CURRENT,
                    encrypted: false,
                    redactions_applied: false,
                })
            }
            async fn restore(
                &self,
                _: RestoreRequest,
            ) -> Result<RestoreReport, SnapshotError> {
                unimplemented!()
            }
            async fn list(
                &self,
                _: &AgentId,
                _: &str,
            ) -> Result<Vec<SnapshotMeta>, SnapshotError> {
                Ok(Vec::new())
            }
            async fn diff(
                &self,
                _: &AgentId,
                _: &str,
                _: SnapshotId,
                _: SnapshotId,
            ) -> Result<SnapshotDiff, SnapshotError> {
                unimplemented!()
            }
            async fn verify(&self, _: &Path) -> Result<VerifyReport, SnapshotError> {
                unimplemented!()
            }
            async fn delete(
                &self,
                _: &AgentId,
                _: &str,
                _: SnapshotId,
            ) -> Result<(), SnapshotError> {
                Ok(())
            }
            async fn export(
                &self,
                _: &AgentId,
                _: &str,
                _: SnapshotId,
                t: &Path,
            ) -> Result<PathBuf, SnapshotError> {
                Ok(t.to_path_buf())
            }
        }
        let _ = inner; // silence: capturing struct above is a different impl
        let cap = Arc::new(TenantCapture {
            last_tenant: Mutex::new(None),
        });
        let tool = MemorySnapshotTool::new(cap.clone() as Arc<dyn MemorySnapshotter>)
            .with_tenant("acme");
        tool.call(&ctx(), json!({})).await.unwrap();
        assert_eq!(cap.last_tenant.lock().unwrap().as_deref(), Some("acme"));
    }
}
