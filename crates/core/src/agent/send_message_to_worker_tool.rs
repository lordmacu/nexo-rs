//! Phase 84.3 — `SendMessageToWorker` continuation tool.
//!
//! The coordinator (Phase 84.1) needs to re-engage a finished
//! worker with its loaded context. Today the choice was binary:
//! - `TeamCreate` → spawn a fresh worker (loses research context)
//! - `SendToPeer` → at-rest peer agent (different semantics)
//!
//! `SendMessageToWorker` fills the gap. The tool looks up
//! `worker_id` in a [`WorkerRegistry`] (Phase 84.3 worker registry
//! module), validates the four error scenarios from the spec, and
//! returns a structured response payload that the coordinator's
//! next turn can read.
//!
//! ## Error scenarios (spec)
//!
//! | Scenario | Returned `kind` |
//! |---|---|
//! | Worker exists, finished, this binding spawned it | `Continued` (success) |
//! | `worker_id` matches no registry entry | `UnknownWorker` |
//! | Worker exists but is `Running` | `WorkerStillRunning` |
//! | Worker registered under a different coordinator binding | `UnknownWorker` (defense-in-depth — no cross-binding existence oracle) |
//!
//! ## Producer side (deferred)
//!
//! The actual transcript-resume execution (taking the worker's
//! prior `messages`, appending `message` as a new user turn, and
//! running another fork-loop turn that emits a fresh
//! `<task-notification>`) lands when the fork-as-tool spawn
//! pipeline arrives. Until then, the success path returns a
//! placeholder `Continued` payload + an explicit
//! `pipeline_pending: true` flag so the coordinator can see that
//! the work was registered but not yet executed. This matches the
//! 84.2 deferral pattern (build the type + producer, defer the
//! end-to-end wire-up).

use std::sync::Arc;

use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use super::worker_registry::{WorkerLookup, WorkerRegistry};

/// The LLM-facing tool name. Stable string — referenced by the
/// coordinator persona prompt (Phase 84.1) and the binding
/// allowlist.
pub const SEND_MESSAGE_TO_WORKER_TOOL_NAME: &str = "SendMessageToWorker";

const MAX_MESSAGE_BYTES: usize = 32 * 1024;

pub struct SendMessageToWorkerTool {
    pub registry: Arc<dyn WorkerRegistry>,
}

impl SendMessageToWorkerTool {
    pub fn new(registry: Arc<dyn WorkerRegistry>) -> Self {
        Self { registry }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: SEND_MESSAGE_TO_WORKER_TOOL_NAME.to_string(),
            description: r#"Continue a previously-finished worker by appending a new user turn to its loaded session context. Distinct from `SendToPeer` (peer-to-peer messaging to a live agent) and `TeamCreate` (spawn fresh worker with empty context).

Use when the new ask shares >50% of the prior worker's read files / search terms — continuation preserves the cache and avoids re-reading. Use `TeamCreate` when the new work is in a different subsystem.

Errors: `UnknownWorker` (no such worker_id in this binding's registry), `WorkerStillRunning` (use SendToPeer instead), `MessageTooLarge` (>32 KiB).
Coordinator-only — `role: coordinator` bindings only."#
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "worker_id": {
                        "type": "string",
                        "description": "Stable worker id taken from the prior `<task-notification>`'s `<task-id>` element (Phase 84.2)."
                    },
                    "message": {
                        "type": "string",
                        "description": "Synthesized continuation spec. The worker's session sees this as one new user turn appended to its loaded context. Must be a complete, actionable request — workers see notifications, not chitchat."
                    }
                },
                "required": ["worker_id", "message"],
                "additionalProperties": false
            }),
        }
    }
}

fn err(kind: &str, message: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "kind": kind,
        "error": message.into(),
    })
}

/// Pure-args / pure-registry continuation handler. Extracted from
/// the [`ToolHandler`] impl so tests can exercise every error
/// branch without spinning up an `AgentContext` (broker + session
/// manager + agent config). The handler proper composes this with
/// the per-call binding key + role read from the ctx.
pub async fn handle_send_message_to_worker(
    registry: &dyn WorkerRegistry,
    role: nexo_config::types::plan_mode::BindingRole,
    binding_key: Option<&str>,
    args: &Value,
) -> Value {
    use nexo_config::types::plan_mode::BindingRole;

    if !matches!(role, BindingRole::Coordinator) {
        return err(
            "RoleRefused",
            "SendMessageToWorker requires `role: coordinator`",
        );
    }

    let worker_id = match args.get("worker_id").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            return err(
                "Wire",
                "SendMessageToWorker requires `worker_id` (non-empty string)",
            )
        }
    };
    let message = match args.get("message").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => {
            return err(
                "Wire",
                "SendMessageToWorker requires `message` (non-empty string)",
            )
        }
    };
    if message.len() > MAX_MESSAGE_BYTES {
        return err(
            "MessageTooLarge",
            format!(
                "message is {} bytes; cap is {} bytes",
                message.len(),
                MAX_MESSAGE_BYTES
            ),
        );
    }

    let binding_key = match binding_key {
        Some(b) if !b.is_empty() => b,
        _ => {
            return err(
                "BindingUnresolved",
                "SendMessageToWorker requires a resolved binding context",
            )
        }
    };

    match registry.lookup(binding_key, &worker_id).await {
        WorkerLookup::Unknown => err(
            "UnknownWorker",
            format!("no worker `{worker_id}` registered for this binding"),
        ),
        WorkerLookup::Running(_) => err(
            "WorkerStillRunning",
            "worker is mid-loop; use SendToPeer for live peer messaging",
        ),
        WorkerLookup::Continuable(snapshot) => json!({
            "ok": true,
            "kind": "Continued",
            "worker_id": snapshot.worker_id,
            "prior_status": match snapshot.status {
                super::worker_registry::WorkerStatus::Completed => "completed",
                super::worker_registry::WorkerStatus::Terminated => "terminated",
                super::worker_registry::WorkerStatus::Running => "running",
            },
            "messages_count": snapshot.messages_count,
            "message_bytes": message.len(),
            "pipeline_pending": true,
        }),
    }
}

#[async_trait]
impl ToolHandler for SendMessageToWorkerTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let policy = ctx.effective_policy();
        let role = nexo_config::types::plan_mode::BindingRole::from_role_str(
            policy.role.as_deref(),
        );
        let binding_key = policy.binding_id();
        Ok(
            handle_send_message_to_worker(
                self.registry.as_ref(),
                role,
                binding_key.as_deref(),
                &args,
            )
            .await,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::worker_registry::{
        InMemoryWorkerRegistry, WorkerSnapshot, WorkerStatus,
    };
    use nexo_config::types::plan_mode::BindingRole;

    fn snap(
        binding: &str,
        worker: &str,
        status: WorkerStatus,
        messages_count: usize,
    ) -> WorkerSnapshot {
        WorkerSnapshot {
            worker_id: worker.to_string(),
            status,
            coordinator_binding_key: binding.to_string(),
            messages_count,
        }
    }

    fn args(worker_id: &str, message: &str) -> Value {
        json!({ "worker_id": worker_id, "message": message })
    }

    #[test]
    fn tool_def_metadata() {
        let def = SendMessageToWorkerTool::tool_def();
        assert_eq!(def.name, SEND_MESSAGE_TO_WORKER_TOOL_NAME);
        assert!(def.description.contains("coordinator"));
        let params_str = def.parameters.to_string();
        assert!(params_str.contains("worker_id"));
        assert!(params_str.contains("message"));
    }

    #[tokio::test]
    async fn success_continuation_returns_continued_payload() {
        // Spec scenario 1: worker exists, finished, this binding
        // spawned it → success.
        let r = InMemoryWorkerRegistry::new();
        r.upsert(snap("ana:default", "w-1", WorkerStatus::Completed, 12));
        let out = handle_send_message_to_worker(
            &r,
            BindingRole::Coordinator,
            Some("ana:default"),
            &args("w-1", "Continue: investigate the auth bug"),
        )
        .await;
        assert_eq!(out["ok"], true);
        assert_eq!(out["kind"], "Continued");
        assert_eq!(out["worker_id"], "w-1");
        assert_eq!(out["prior_status"], "completed");
        assert_eq!(out["messages_count"], 12);
        assert_eq!(out["pipeline_pending"], true);
    }

    #[tokio::test]
    async fn unknown_worker_id_returns_404_style_error() {
        // Spec scenario 2: no such worker_id in this binding's
        // registry.
        let r = InMemoryWorkerRegistry::new();
        let out = handle_send_message_to_worker(
            &r,
            BindingRole::Coordinator,
            Some("ana:default"),
            &args("hallucinated", "do the thing"),
        )
        .await;
        assert_eq!(out["ok"], false);
        assert_eq!(out["kind"], "UnknownWorker");
    }

    #[tokio::test]
    async fn worker_still_running_returns_refused() {
        // Spec scenario 3: worker exists but is mid-loop. Refuse —
        // distinct from SendToPeer semantics.
        let r = InMemoryWorkerRegistry::new();
        r.upsert(snap("ana:default", "w-2", WorkerStatus::Running, 4));
        let out = handle_send_message_to_worker(
            &r,
            BindingRole::Coordinator,
            Some("ana:default"),
            &args("w-2", "go go go"),
        )
        .await;
        assert_eq!(out["ok"], false);
        assert_eq!(out["kind"], "WorkerStillRunning");
        // Hint to use SendToPeer is in the error body.
        let msg = out["error"].as_str().unwrap_or_default();
        assert!(msg.contains("SendToPeer"));
    }

    #[tokio::test]
    async fn cross_binding_worker_id_returns_unknown_not_existence_oracle() {
        // Spec scenario 4: worker registered under binding A is
        // invisible to binding B. The error returned MUST be
        // `UnknownWorker` (not a "wrong binding" hint), so
        // binding B can't enumerate which worker_ids belong to
        // binding A.
        let r = InMemoryWorkerRegistry::new();
        r.upsert(snap("ana:default", "secret-w", WorkerStatus::Completed, 9));
        let out = handle_send_message_to_worker(
            &r,
            BindingRole::Coordinator,
            Some("ana:other"),
            &args("secret-w", "snoop"),
        )
        .await;
        assert_eq!(out["ok"], false);
        assert_eq!(out["kind"], "UnknownWorker");
        // Defense in depth: the error returned for a cross-
        // binding probe must be byte-identical to the error
        // returned for a worker that exists nowhere. Compare to
        // the no-such-worker case directly.
        let r_empty = InMemoryWorkerRegistry::new();
        let baseline = handle_send_message_to_worker(
            &r_empty,
            BindingRole::Coordinator,
            Some("ana:other"),
            &args("secret-w", "snoop"),
        )
        .await;
        assert_eq!(out, baseline);
    }

    #[tokio::test]
    async fn worker_role_is_refused() {
        // Defense in depth: even when the binding's allowed_tools
        // is wide-open (`["*"]`), the handler refuses non-
        // coordinator roles.
        let r = InMemoryWorkerRegistry::new();
        r.upsert(snap("ana:default", "w-1", WorkerStatus::Completed, 12));
        let out = handle_send_message_to_worker(
            &r,
            BindingRole::Worker,
            Some("ana:default"),
            &args("w-1", "hi"),
        )
        .await;
        assert_eq!(out["ok"], false);
        assert_eq!(out["kind"], "RoleRefused");
    }

    #[tokio::test]
    async fn unset_role_is_refused() {
        let r = InMemoryWorkerRegistry::new();
        let out = handle_send_message_to_worker(
            &r,
            BindingRole::Unset,
            Some("ana:default"),
            &args("w-1", "hi"),
        )
        .await;
        assert_eq!(out["kind"], "RoleRefused");
    }

    #[tokio::test]
    async fn missing_binding_key_returns_binding_unresolved() {
        // Synthesised policies (delegation intake, heartbeat,
        // tests) have no binding_id() — the tool refuses cleanly
        // rather than fall through to a default.
        let r = InMemoryWorkerRegistry::new();
        let out = handle_send_message_to_worker(
            &r,
            BindingRole::Coordinator,
            None,
            &args("w-1", "hi"),
        )
        .await;
        assert_eq!(out["kind"], "BindingUnresolved");
    }

    #[tokio::test]
    async fn empty_worker_id_is_wire_error() {
        let r = InMemoryWorkerRegistry::new();
        let out = handle_send_message_to_worker(
            &r,
            BindingRole::Coordinator,
            Some("ana:default"),
            &args("", "hi"),
        )
        .await;
        assert_eq!(out["kind"], "Wire");
    }

    #[tokio::test]
    async fn empty_message_is_wire_error() {
        let r = InMemoryWorkerRegistry::new();
        let out = handle_send_message_to_worker(
            &r,
            BindingRole::Coordinator,
            Some("ana:default"),
            &args("w-1", "   "),
        )
        .await;
        assert_eq!(out["kind"], "Wire");
    }

    #[tokio::test]
    async fn oversize_message_returns_message_too_large() {
        let r = InMemoryWorkerRegistry::new();
        r.upsert(snap("ana:default", "w-1", WorkerStatus::Completed, 1));
        let huge = "x".repeat(MAX_MESSAGE_BYTES + 1);
        let out = handle_send_message_to_worker(
            &r,
            BindingRole::Coordinator,
            Some("ana:default"),
            &args("w-1", &huge),
        )
        .await;
        assert_eq!(out["kind"], "MessageTooLarge");
    }

    #[tokio::test]
    async fn terminated_worker_is_continuable() {
        // A worker that exited via failure / timeout / kill is
        // still continuable — the coordinator may want to ask
        // "what happened?" or attempt a recovery turn.
        let r = InMemoryWorkerRegistry::new();
        r.upsert(snap("ana:default", "w-bad", WorkerStatus::Terminated, 3));
        let out = handle_send_message_to_worker(
            &r,
            BindingRole::Coordinator,
            Some("ana:default"),
            &args("w-bad", "What happened?"),
        )
        .await;
        assert_eq!(out["ok"], true);
        assert_eq!(out["prior_status"], "terminated");
    }
}
