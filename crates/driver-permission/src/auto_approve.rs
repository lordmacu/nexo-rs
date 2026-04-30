//! Phase 80.17 — curated auto-approve decision table.
//!
//! When `auto_approve == true` on a binding's effective policy, the
//! approval pipeline calls [`is_curated_auto_approve`] before
//! falling through to the interactive prompt. The fn returns
//! `true` for read-only / scoped-write / coordination tools and
//! `false` for everything else (including unknown tools — the
//! default arm is deny so newly-introduced tools never auto-allow
//! until they are explicitly listed).
//!
//! Composes with the binding policy `allowed_tools` filter (Phase
//! 16) and the existing approval pipeline: this fn never widens the
//! tool surface, only skips the prompt for tools that are already
//! on the binding's surface AND fall in the curated subset.
//!
//! Defense-in-depth:
//! - Bash: must pass [`bash_destructive::is_read_only`] AND
//!   [`bash_destructive::check_destructive_command`] returns `None`
//!   AND [`bash_destructive::check_sed_in_place`] returns `None`.
//!   Destructive commands always fall through to interactive
//!   approval.
//! - FileEdit / FileWrite: target path must canonicalize under the
//!   policy's `workspace_path`. Symlink-escape resistant. New files
//!   that don't exist yet canonicalize their parent then re-attach
//!   the filename.
//! - ConfigTool / REPL / remote_trigger / schedule_cron: always ask,
//!   even with the dial on.
//! - `mcp_*` / `ext_*` prefixed tools: default-ask (heterogeneous;
//!   per-server allowlists are a future operator-side knob).

use std::path::Path;

use serde_json::Value;

use crate::bash_destructive;

/// Public API: `true` iff this tool call should be auto-allowed
/// without operator approval. Never widens the binding's tool
/// surface — only skips the interactive prompt for already-allowed
/// tools that fall in the curated subset.
///
/// Provider-agnostic — the decision operates on the tool name +
/// JSON args + resolved policy snapshot, never on the LLM provider
/// driving the call.
pub fn is_curated_auto_approve(
    tool_name: &str,
    args: &Value,
    auto_approve_on: bool,
    workspace_path: Option<&Path>,
) -> bool {
    if !auto_approve_on {
        return false;
    }
    match tool_name {
        // ── Always read-only / info gathering ──
        "FileRead"
        | "Glob"
        | "Grep"
        | "LSP"
        | "list_agents"
        | "agent_status"
        | "agent_turns_tail"
        | "memory_history"
        | "dream_runs_tail"
        | "list_mcp_resources"
        | "read_mcp_resource"
        | "WebFetch"
        | "WebSearch"
        | "list_followups"
        | "list_peers"
        | "task_get" => true,

        // ── Bash — read-only AND not destructive AND not sed-in-place ──
        "Bash" => is_bash_safe_for_auto_allow(args),

        // ── File writes — only inside workspace ──
        "FileEdit" | "FileWrite" => is_path_inside_workspace(args, workspace_path),

        // ── Notifications + memory — outbound to operator-allowed channels ──
        "notify_origin"
        | "notify_channel"
        | "notify_push"
        | "forge_memory_checkpoint"
        | "dream_now"
        | "ask_user_question" => true,

        // ── Multi-agent coordination — internal team work ──
        "delegate"
        | "team_create"
        | "team_delete"
        | "send_to_peer"
        | "task_create"
        | "task_update"
        | "task_stop" => true,

        // ── ALWAYS interactive — never auto, even with the dial on ──
        "ConfigTool" | "config_self_edit" => false,
        "REPL" => false,
        "remote_trigger" => false,
        "schedule_cron" => false,

        // ── MCP / extension per-server tools — heterogeneous default ──
        name if name.starts_with("mcp_") || name.starts_with("ext_") => false,

        // ── Unknown / new tool — default-ask ──
        _ => false,
    }
}

fn is_bash_safe_for_auto_allow(args: &Value) -> bool {
    let cmd = match args.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return false,
    };
    bash_destructive::is_read_only(cmd)
        && bash_destructive::check_destructive_command(cmd).is_none()
        && bash_destructive::check_sed_in_place(cmd).is_none()
}

fn is_path_inside_workspace(args: &Value, workspace: Option<&Path>) -> bool {
    let ws = match workspace {
        Some(w) => w,
        None => return false,
    };
    let target = match args.get("file_path").and_then(|v| v.as_str()) {
        Some(p) => Path::new(p),
        None => return false,
    };
    canonical_starts_with(target, ws)
}

/// Defensive — canonicalize both paths and compare. Symlink-escape
/// resistant. For not-yet-existing files (FileWrite creating new),
/// canonicalize the parent and append the filename. Falls back to
/// `false` on any I/O error.
fn canonical_starts_with(target: &Path, ws: &Path) -> bool {
    let target_canon = match target.canonicalize() {
        Ok(p) => p,
        Err(_) => match target
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .zip(target.file_name())
        {
            Some((parent_canon, name)) => parent_canon.join(name),
            None => return false,
        },
    };
    let ws_canon = match ws.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    target_canon.starts_with(&ws_canon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn ws() -> Option<&'static Path> {
        None
    }

    #[test]
    fn disabled_returns_false_for_everything() {
        assert!(!is_curated_auto_approve("FileRead", &json!({}), false, None));
        assert!(!is_curated_auto_approve("Bash", &json!({"command": "ls"}), false, None));
        assert!(!is_curated_auto_approve("WebSearch", &json!({}), false, None));
    }

    #[test]
    fn file_read_always_auto_when_enabled() {
        assert!(is_curated_auto_approve("FileRead", &json!({"file_path": "/etc/hosts"}), true, None));
    }

    #[test]
    fn glob_grep_lsp_auto() {
        assert!(is_curated_auto_approve("Glob", &json!({}), true, None));
        assert!(is_curated_auto_approve("Grep", &json!({}), true, None));
        assert!(is_curated_auto_approve("LSP", &json!({}), true, None));
    }

    #[test]
    fn bash_ls_auto() {
        let ok = is_curated_auto_approve(
            "Bash",
            &json!({"command": "ls /tmp"}),
            true,
            ws(),
        );
        assert!(ok, "ls /tmp must auto-approve");
    }

    #[test]
    fn bash_rm_rf_never_auto() {
        let denied = is_curated_auto_approve(
            "Bash",
            &json!({"command": "rm -rf /tmp/foo"}),
            true,
            ws(),
        );
        assert!(!denied, "destructive commands must NOT auto-approve");
    }

    #[test]
    fn bash_sed_in_place_never_auto() {
        let denied = is_curated_auto_approve(
            "Bash",
            &json!({"command": "sed -i 's/x/y/' /etc/foo"}),
            true,
            ws(),
        );
        assert!(!denied, "sed -i must NOT auto-approve");
    }

    #[test]
    fn bash_missing_command_arg() {
        // Defensive: missing `command` field → false.
        assert!(!is_curated_auto_approve("Bash", &json!({}), true, ws()));
        assert!(!is_curated_auto_approve(
            "Bash",
            &json!({"command": null}),
            true,
            ws()
        ));
    }

    #[test]
    fn bash_pipe_with_destructive_in_chain() {
        // The destructive heuristic catches commands buried in pipes.
        let cmd = "ls | xargs rm -rf";
        let denied = is_curated_auto_approve(
            "Bash",
            &json!({"command": cmd}),
            true,
            ws(),
        );
        assert!(!denied, "destructive in pipe must veto");
    }

    #[test]
    fn file_edit_inside_workspace_auto() {
        let tmp = TempDir::new().unwrap();
        let ws_path = tmp.path().canonicalize().unwrap();
        let target = ws_path.join("MEMORY.md");
        std::fs::write(&target, b"hi").unwrap();
        let ok = is_curated_auto_approve(
            "FileEdit",
            &json!({"file_path": target.to_string_lossy()}),
            true,
            Some(&ws_path),
        );
        assert!(ok, "edit inside workspace must auto-approve");
    }

    #[test]
    fn file_edit_outside_workspace_never_auto() {
        let tmp = TempDir::new().unwrap();
        let ws_path = tmp.path().canonicalize().unwrap();
        // /etc/hosts is canonical-existing but NOT under the workspace.
        let denied = is_curated_auto_approve(
            "FileEdit",
            &json!({"file_path": "/etc/hosts"}),
            true,
            Some(&ws_path),
        );
        assert!(!denied, "edit outside workspace must NOT auto-approve");
    }

    #[test]
    fn file_edit_workspace_none_blocks() {
        // No workspace_path configured → always false.
        let denied = is_curated_auto_approve(
            "FileEdit",
            &json!({"file_path": "/tmp/x.md"}),
            true,
            None,
        );
        assert!(!denied, "no workspace = always ask");
    }

    #[test]
    fn file_edit_new_file_uses_parent_canonicalize() {
        let tmp = TempDir::new().unwrap();
        let ws_path = tmp.path().canonicalize().unwrap();
        // File doesn't exist yet — `canonicalize()` would fail; helper
        // falls back to `parent.canonicalize() + filename`.
        let target = ws_path.join("brand-new-file.md");
        let ok = is_curated_auto_approve(
            "FileWrite",
            &json!({"file_path": target.to_string_lossy()}),
            true,
            Some(&ws_path),
        );
        assert!(ok, "creating a new file inside workspace must auto-approve");
    }

    #[test]
    fn file_edit_missing_file_path_arg() {
        let tmp = TempDir::new().unwrap();
        let ws_path = tmp.path().canonicalize().unwrap();
        // Missing field → false.
        assert!(!is_curated_auto_approve(
            "FileEdit",
            &json!({}),
            true,
            Some(&ws_path)
        ));
    }

    #[test]
    fn notify_origin_auto() {
        assert!(is_curated_auto_approve("notify_origin", &json!({}), true, None));
    }

    #[test]
    fn notify_push_auto() {
        assert!(is_curated_auto_approve("notify_push", &json!({}), true, None));
    }

    #[test]
    fn dream_now_auto() {
        assert!(is_curated_auto_approve("dream_now", &json!({}), true, None));
    }

    #[test]
    fn delegate_auto() {
        assert!(is_curated_auto_approve("delegate", &json!({}), true, None));
    }

    #[test]
    fn team_create_auto() {
        assert!(is_curated_auto_approve("team_create", &json!({}), true, None));
    }

    #[test]
    fn task_create_auto() {
        assert!(is_curated_auto_approve("task_create", &json!({}), true, None));
    }

    #[test]
    fn task_get_auto() {
        // Read-only — always auto.
        assert!(is_curated_auto_approve("task_get", &json!({}), true, None));
    }

    #[test]
    fn config_tool_never_auto() {
        // Even with dial on, ConfigTool must ask.
        assert!(!is_curated_auto_approve("ConfigTool", &json!({}), true, None));
        assert!(!is_curated_auto_approve("config_self_edit", &json!({}), true, None));
    }

    #[test]
    fn repl_never_auto() {
        assert!(!is_curated_auto_approve("REPL", &json!({}), true, None));
    }

    #[test]
    fn remote_trigger_never_auto() {
        assert!(!is_curated_auto_approve("remote_trigger", &json!({}), true, None));
    }

    #[test]
    fn schedule_cron_never_auto() {
        assert!(!is_curated_auto_approve("schedule_cron", &json!({}), true, None));
    }

    #[test]
    fn mcp_prefix_default_ask() {
        assert!(!is_curated_auto_approve("mcp_github_create_issue", &json!({}), true, None));
        assert!(!is_curated_auto_approve("mcp_anything", &json!({}), true, None));
    }

    #[test]
    fn ext_prefix_default_ask() {
        assert!(!is_curated_auto_approve("ext_custom_tool", &json!({}), true, None));
    }

    #[test]
    fn unknown_tool_default_ask() {
        assert!(!is_curated_auto_approve("brand_new_future_tool", &json!({}), true, None));
        assert!(!is_curated_auto_approve("", &json!({}), true, None));
    }
}

// ─────────────────────────────────────────────────────────────────────
// Phase 80.17.b — `AutoApproveDecider` decorator wiring
//
// Wraps any `PermissionDecider`. Reads the resolved `auto_approve`
// flag + `workspace_path` from the request `metadata` map (populated
// caller-side from `EffectiveBindingPolicy`) and short-circuits to
// `AllowOnce` when `is_curated_auto_approve` says yes. Otherwise
// delegates to the inner decider with the original request.
//
// Caller-side metadata population is the deferred 80.17.b.c
// follow-up: when the wire that constructs `PermissionRequest`
// resolves the binding's policy, it must insert the two fields
// into `metadata` before invoking the decider. Until that ships,
// `auto_approve` reads `false` from missing metadata and the
// decorator becomes a transparent pass-through.
// ─────────────────────────────────────────────────────────────────────

use std::sync::Arc;

use async_trait::async_trait;

use crate::decider::PermissionDecider;
use crate::error::PermissionError;
use crate::types::{PermissionOutcome, PermissionRequest, PermissionResponse};

/// Metadata field name carrying the boolean dial.
pub const META_AUTO_APPROVE: &str = "auto_approve";
/// Metadata field name carrying the canonical workspace path used
/// for the FileEdit / FileWrite scope check.
pub const META_WORKSPACE_PATH: &str = "workspace_path";

/// Wrap any `PermissionDecider` so curated tool calls bypass the
/// inner decider when the request's `metadata.auto_approve` is true
/// AND `is_curated_auto_approve` agrees. Otherwise delegates.
///
/// # Wire
///
/// ```ignore
/// // Boot-time:
/// let inner: Arc<dyn PermissionDecider> = Arc::new(AllowAllDecider);
/// let decider = AutoApproveDecider::new(inner);
/// // Caller-side, before invoking decider:
/// let mut metadata = serde_json::Map::new();
/// metadata.insert("auto_approve".into(), policy.auto_approve.into());
/// if let Some(ws) = &policy.workspace_path {
///     metadata.insert(
///         "workspace_path".into(),
///         ws.to_string_lossy().to_string().into(),
///     );
/// }
/// let req = PermissionRequest { tool_name, input, metadata, .. };
/// decider.decide(req).await
/// ```
pub struct AutoApproveDecider<D: PermissionDecider + ?Sized> {
    inner: Arc<D>,
}

impl<D: PermissionDecider + ?Sized> AutoApproveDecider<D> {
    /// Build a decorator over an existing inner decider.
    pub fn new(inner: Arc<D>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl<D: PermissionDecider + ?Sized> PermissionDecider for AutoApproveDecider<D> {
    async fn decide(
        &self,
        request: PermissionRequest,
    ) -> Result<PermissionResponse, PermissionError> {
        let auto_on = request
            .metadata
            .get(META_AUTO_APPROVE)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let workspace = request
            .metadata
            .get(META_WORKSPACE_PATH)
            .and_then(|v| v.as_str())
            .map(Path::new);

        if is_curated_auto_approve(&request.tool_name, &request.input, auto_on, workspace) {
            return Ok(PermissionResponse {
                tool_use_id: request.tool_use_id.clone(),
                outcome: PermissionOutcome::AllowOnce {
                    updated_input: None,
                },
                rationale: format!(
                    "auto_approve: curated subset ({})",
                    request.tool_name
                ),
            });
        }
        self.inner.decide(request).await
    }
}

#[cfg(test)]
mod decorator_tests {
    use super::*;
    use crate::decider::{AllowAllDecider, DenyAllDecider};
    use nexo_driver_types::GoalId;
    use serde_json::json;

    fn req(tool_name: &str, input: serde_json::Value) -> PermissionRequest {
        PermissionRequest {
            goal_id: GoalId::new(),
            tool_use_id: "tu_1".into(),
            tool_name: tool_name.into(),
            input,
            metadata: serde_json::Map::new(),
        }
    }

    fn req_with_meta(
        tool_name: &str,
        input: serde_json::Value,
        meta: serde_json::Map<String, serde_json::Value>,
    ) -> PermissionRequest {
        PermissionRequest {
            goal_id: GoalId::new(),
            tool_use_id: "tu_1".into(),
            tool_name: tool_name.into(),
            input,
            metadata: meta,
        }
    }

    #[tokio::test]
    async fn delegates_when_metadata_missing() {
        // Inner is DenyAll so we can prove the decorator delegated.
        let inner: Arc<dyn PermissionDecider> =
            Arc::new(DenyAllDecider { reason: "delegated".into() });
        let dec = AutoApproveDecider::new(inner);
        let resp = dec.decide(req("FileRead", json!({}))).await.unwrap();
        assert!(matches!(resp.outcome, PermissionOutcome::Deny { .. }));
    }

    #[tokio::test]
    async fn delegates_when_flag_false() {
        let inner: Arc<dyn PermissionDecider> =
            Arc::new(DenyAllDecider { reason: "delegated".into() });
        let dec = AutoApproveDecider::new(inner);
        let mut meta = serde_json::Map::new();
        meta.insert(META_AUTO_APPROVE.into(), json!(false));
        let resp = dec
            .decide(req_with_meta("FileRead", json!({}), meta))
            .await
            .unwrap();
        assert!(matches!(resp.outcome, PermissionOutcome::Deny { .. }));
    }

    #[tokio::test]
    async fn short_circuits_for_curated_tool() {
        // Inner is DenyAll — if delegation happened we'd see Deny.
        let inner: Arc<dyn PermissionDecider> =
            Arc::new(DenyAllDecider { reason: "should_not_reach".into() });
        let dec = AutoApproveDecider::new(inner);
        let mut meta = serde_json::Map::new();
        meta.insert(META_AUTO_APPROVE.into(), json!(true));
        let resp = dec
            .decide(req_with_meta("FileRead", json!({}), meta))
            .await
            .unwrap();
        assert!(matches!(
            resp.outcome,
            PermissionOutcome::AllowOnce { updated_input: None }
        ));
        assert!(resp.rationale.contains("auto_approve"));
        assert!(resp.rationale.contains("FileRead"));
    }

    #[tokio::test]
    async fn delegates_for_destructive_bash() {
        // Bash rm -rf is curated-rejected, so decorator delegates.
        // Inner is AllowAll → would say AllowOnce; verify we got
        // there, not the decorator's short-circuit.
        let inner: Arc<dyn PermissionDecider> = Arc::new(AllowAllDecider);
        let dec = AutoApproveDecider::new(inner);
        let mut meta = serde_json::Map::new();
        meta.insert(META_AUTO_APPROVE.into(), json!(true));
        let resp = dec
            .decide(req_with_meta(
                "Bash",
                json!({"command": "rm -rf /tmp/foo"}),
                meta,
            ))
            .await
            .unwrap();
        assert!(matches!(
            resp.outcome,
            PermissionOutcome::AllowOnce { updated_input: None }
        ));
        // Rationale comes from AllowAllDecider, not the decorator.
        assert!(resp.rationale.contains("AllowAllDecider"));
    }

    #[tokio::test]
    async fn delegates_for_unknown_tool() {
        let inner: Arc<dyn PermissionDecider> =
            Arc::new(DenyAllDecider { reason: "unknown".into() });
        let dec = AutoApproveDecider::new(inner);
        let mut meta = serde_json::Map::new();
        meta.insert(META_AUTO_APPROVE.into(), json!(true));
        let resp = dec
            .decide(req_with_meta("brand_new_tool", json!({}), meta))
            .await
            .unwrap();
        assert!(matches!(resp.outcome, PermissionOutcome::Deny { .. }));
    }

    #[tokio::test]
    async fn handles_string_in_bool_field_defensively() {
        // metadata.auto_approve = "true" (string, not bool) → as_bool()
        // returns None → flag treated as false → delegate.
        let inner: Arc<dyn PermissionDecider> =
            Arc::new(DenyAllDecider { reason: "delegated".into() });
        let dec = AutoApproveDecider::new(inner);
        let mut meta = serde_json::Map::new();
        meta.insert(META_AUTO_APPROVE.into(), json!("true"));
        let resp = dec
            .decide(req_with_meta("FileRead", json!({}), meta))
            .await
            .unwrap();
        assert!(matches!(resp.outcome, PermissionOutcome::Deny { .. }));
    }

}

