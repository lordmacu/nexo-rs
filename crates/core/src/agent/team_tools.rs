//! Phase 79.6 — five `Team*` tools sharing one `Arc<TeamTools>`
//! inner.
//!
//! Step 8 ships:
//!   * `TeamTools` shared inner (bag of `Arc`s the handlers
//!     consume).
//!   * Five `pub struct *Tool { inner: Arc<TeamTools> }`
//!     wrappers, each with its `tool_def() -> ToolDef`.
//!   * Placeholder `ToolHandler::call` impls returning
//!     `{"ok": false, "kind": "NotImplemented"}` so the tool
//!     surface is stable while step 9 wires the real handlers.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/TeamCreateTool/TeamCreateTool.ts:37-49`
//!     — `inputSchema { team_name, description?, agent_type? }`.
//!     We add `worktree_per_member?`.
//!   * `claude-code-leak/src/tools/TeamDeleteTool/TeamDeleteTool.ts:21-22`
//!     — empty `strictObject`. We widen to `team_id` because we
//!     allow multiple teams per leader (relaxed from the leak's
//!     single-team guard).
//!   * `claude-code-leak/src/tools/SendMessageTool/SendMessageTool.ts:46-58`
//!     — discriminated union of structured messages.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use crate::team_message_router::TeamMessageRouter;
use async_trait::async_trait;
use nexo_broker::AnyBroker;
use nexo_config::types::team::TeamPolicy;
use nexo_llm::ToolDef;
use nexo_team_store::TeamStore;
use serde_json::{json, Value};
use std::sync::Arc;

/// Shared inner the five `Team*Tool` structs hold via `Arc`.
/// Handlers in step 9 will read from `store`, publish to
/// `router`, enforce caps via `policy`, and stamp audit rows
/// with `agent_id` + `current_goal_id`.
pub struct TeamTools {
    pub store: Arc<dyn TeamStore>,
    pub router: Arc<TeamMessageRouter<AnyBroker>>,
    pub broker: AnyBroker,
    pub policy: TeamPolicy,
    pub agent_id: String,
    /// The lead's own goal_id at construction. When this agent
    /// runs *as a teammate* (delegated by another team's lead),
    /// `team_member_name` on `AgentContext` is `Some(...)` and
    /// the lead-only operations (`TeamCreate`, `TeamDelete`,
    /// `TeamSendMessage { to: "broadcast" }`) refuse. The
    /// in-team-as-non-lead guard runs at call time on the
    /// passed `&AgentContext`.
    pub current_goal_id: String,
}

impl TeamTools {
    pub fn new(
        store: Arc<dyn TeamStore>,
        router: Arc<TeamMessageRouter<AnyBroker>>,
        broker: AnyBroker,
        policy: TeamPolicy,
        agent_id: impl Into<String>,
        current_goal_id: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            router,
            broker,
            policy,
            agent_id: agent_id.into(),
            current_goal_id: current_goal_id.into(),
        })
    }
}

// ===============================================================
// Five wrappers. Each holds `Arc<TeamTools>` so they share one
// inner; each has its own `tool_def()` returning the right
// `ToolDef` for the LLM tool catalogue.
// ===============================================================

pub struct TeamCreateTool {
    pub inner: Arc<TeamTools>,
}

impl TeamCreateTool {
    pub fn new(inner: Arc<TeamTools>) -> Self {
        Self { inner }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "TeamCreate".to_string(),
            description: r#"Create a new named team for coordinated parallel work. Returns a `team_id` you reference in subsequent calls.

After creating the team, spawn members by calling `delegate_to` with extra args `team_id` and `team_member_name`. Members operate in parallel, share a Phase 14 TaskFlow task list, and DM each other via `TeamSendMessage`. Idle members go to sleep between turns and wake when they receive a DM.

Caps: 8 members per team (incl. lead), 4 concurrent teams per agent, 24 h idle timeout."#
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "team_name": {
                        "type": "string",
                        "description": "Human-readable name. Sanitized to [a-z0-9-]+ for the team_id (unique). Max 64 chars."
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional one-line summary of what the team is doing."
                    },
                    "agent_type": {
                        "type": "string",
                        "description": "Role label for the lead (e.g. \"coordinator\", \"researcher\"). Free-form; surfaces in TeamStatus."
                    },
                    "worktree_per_member": {
                        "type": "boolean",
                        "description": "When true, each member gets its own git worktree under <workspace>/.team/<team_id>/<member_name>/. Default false (members share the workspace)."
                    }
                },
                "required": ["team_name"]
            }),
        }
    }
}

pub struct TeamDeleteTool {
    pub inner: Arc<TeamTools>,
}

impl TeamDeleteTool {
    pub fn new(inner: Arc<TeamTools>) -> Self {
        Self { inner }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "TeamDelete".to_string(),
            description: r#"Disband a team and clean up its members + DM topics. Refuses with `BlockedByActiveMembers` when any member's goal is still `Running`; send `[shutdown_request]` via `TeamSendMessage { to: "broadcast", message: { type: "shutdown_request" } }` first and wait for members to idle.

Idle members are gracefully cancelled. After the cleanup, the team_id is reusable; tasks + audit rows persist in the store under the deleted team."#
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "team_id": {
                        "type": "string",
                        "description": "Sanitized id returned by TeamCreate. Must be a team this agent leads."
                    }
                },
                "required": ["team_id"]
            }),
        }
    }
}

pub struct TeamSendMessageTool {
    pub inner: Arc<TeamTools>,
}

impl TeamSendMessageTool {
    pub fn new(inner: Arc<TeamTools>) -> Self {
        Self { inner }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "TeamSendMessage".to_string(),
            description: r#"Send a message to a teammate (point-to-point) or to all members (broadcast). Wakes idle teammates. Body capped at 64 KiB serialised JSON.

`to`: member name (e.g. "researcher") for DM, or the literal string "broadcast" (lead only).
`message`: free-form text OR a structured message:
- `{ "type": "shutdown_request", "reason"?: "..." }` — ask members to wind down.
- `{ "type": "shutdown_response", "request_id": "...", "approved": true|false, "reason"?: "..." }`.
- `{ "type": "task_assigned", "task_id": "..." }`.
- `{ "type": "done", "result"?: ... }`.

Only the lead can broadcast. Any team member can DM another."#
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "team_id": {
                        "type": "string",
                        "description": "Sanitized id returned by TeamCreate."
                    },
                    "to": {
                        "type": "string",
                        "description": "Member name (point-to-point) or \"broadcast\" (lead only)."
                    },
                    "message": {
                        "description": "String or structured object {type, ...}. Capped at 64 KiB."
                    }
                },
                "required": ["team_id", "to", "message"]
            }),
        }
    }
}

pub struct TeamListTool {
    pub inner: Arc<TeamTools>,
}

impl TeamListTool {
    pub fn new(inner: Arc<TeamTools>) -> Self {
        Self { inner }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "TeamList".to_string(),
            description: "List teams owned by this agent (read-only). Pass `active_only: true` to exclude soft-deleted teams.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "active_only": {
                        "type": "boolean",
                        "description": "Default true — only return teams with `deleted_at IS NULL`."
                    }
                }
            }),
        }
    }
}

pub struct TeamStatusTool {
    pub inner: Arc<TeamTools>,
}

impl TeamStatusTool {
    pub fn new(inner: Arc<TeamTools>) -> Self {
        Self { inner }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "TeamStatus".to_string(),
            description: "Detailed status of one team: members + their last_active_at + agent_type + idle/running. Read-only. Caller must be the lead OR a current member (NotMember otherwise).".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "team_id": {
                        "type": "string",
                        "description": "Sanitized id returned by TeamCreate."
                    }
                },
                "required": ["team_id"]
            }),
        }
    }
}

// ===============================================================
// Step 8 placeholder handlers — return NotImplemented so the
// tool surface is reachable while step 9 fills the body.
// ===============================================================

fn not_implemented(tool: &'static str) -> Value {
    json!({
        "ok": false,
        "error": format!("Phase 79.6 step 8: `{tool}` handler ships in step 9"),
        "kind": "NotImplemented",
    })
}

#[async_trait]
impl ToolHandler for TeamCreateTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        Ok(not_implemented("TeamCreate"))
    }
}

#[async_trait]
impl ToolHandler for TeamDeleteTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        Ok(not_implemented("TeamDelete"))
    }
}

#[async_trait]
impl ToolHandler for TeamSendMessageTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        Ok(not_implemented("TeamSendMessage"))
    }
}

#[async_trait]
impl ToolHandler for TeamListTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        Ok(not_implemented("TeamList"))
    }
}

#[async_trait]
impl ToolHandler for TeamStatusTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        Ok(not_implemented("TeamStatus"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_create_def_advertises_required_team_name() {
        let def = TeamCreateTool::tool_def();
        assert_eq!(def.name, "TeamCreate");
        let required = def.parameters["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "team_name"));
        // Optional fields surfaced in properties.
        let props = def.parameters["properties"].as_object().unwrap();
        for k in ["team_name", "description", "agent_type", "worktree_per_member"] {
            assert!(props.contains_key(k), "missing property `{k}`");
        }
    }

    #[test]
    fn team_delete_def_requires_team_id() {
        let def = TeamDeleteTool::tool_def();
        assert_eq!(def.name, "TeamDelete");
        let required = def.parameters["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "team_id");
    }

    #[test]
    fn team_send_message_def_requires_team_id_to_message() {
        let def = TeamSendMessageTool::tool_def();
        assert_eq!(def.name, "TeamSendMessage");
        let required = def.parameters["required"].as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"team_id"));
        assert!(names.contains(&"to"));
        assert!(names.contains(&"message"));
    }

    #[test]
    fn team_list_def_optional_active_only() {
        let def = TeamListTool::tool_def();
        assert_eq!(def.name, "TeamList");
        // No `required` field at all — every property is optional.
        assert!(def.parameters.get("required").is_none());
        let props = def.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("active_only"));
    }

    #[test]
    fn team_status_def_requires_team_id() {
        let def = TeamStatusTool::tool_def();
        assert_eq!(def.name, "TeamStatus");
        let required = def.parameters["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "team_id");
    }
}
