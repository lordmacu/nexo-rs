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
/// Handlers read from `store`, publish to `router`, and stamp audit
/// rows with `agent_id` + `current_goal_id`.
///
/// C2 — `policy` is no longer captured at construction. Each handler
/// reads the per-call [`TeamPolicy`] from `ctx.effective_policy().team`
/// via [`TeamTools::policy_for`] so a hot-reload of `team.max_*` (or
/// per-binding override) is observed on the next intake event without
/// re-registration. Mirrors the pattern in
/// `claude-code-leak/src/services/mcp/useManageMCPConnections.ts:624`
/// (invalidate-and-refetch, no actor teardown).
pub struct TeamTools {
    pub store: Arc<dyn TeamStore>,
    pub router: Arc<TeamMessageRouter<AnyBroker>>,
    pub broker: AnyBroker,
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
        agent_id: impl Into<String>,
        current_goal_id: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            router,
            broker,
            agent_id: agent_id.into(),
            current_goal_id: current_goal_id.into(),
        })
    }

    /// Return the per-call team policy resolved from the effective
    /// binding policy. Cheap clone — `TeamPolicy` is six primitive
    /// fields. Single source of truth so handlers stay consistent
    /// across hot-reload.
    pub(super) fn policy_for(&self, ctx: &super::context::AgentContext) -> TeamPolicy {
        ctx.effective_policy().team.clone()
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

After creating the team, members can be added by the operator (via `nexo team add-member` CLI — Phase 79.6.b) or registered programmatically through the team store. The MVP exposes `TeamSendMessage` (DM + broadcast), `TeamList`, and `TeamStatus` so the lead can coordinate already-spawned members. Direct goal-spawn-as-teammate from inside a turn lands in Phase 79.6.b.

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
// Handlers
// ===============================================================
//
// Conventions:
//   * Capability gate (`policy.tool_enabled()`) is enforced at the
//     handler level too — main.rs only registers when enabled,
//     but a handler called without the gate (e.g. via tests with
//     a default policy) still refuses cleanly.
//   * Output shape mirrors Phase 79.5/79.10: `{ok: true, ...}`
//     or `{ok: false, kind: "...", error: "..."}`.
//   * Audit rows go through `store.record_event` best-effort —
//     a failed insert is logged via tracing but does not fail
//     the user-visible call.
//
// Reference (PRIMARY):
//   * `claude-code-leak/src/tools/TeamCreateTool/TeamCreateTool.ts:128-237`
//     — full `call()` body (single-team-per-leader guard at
//     `:133-140` we explicitly relax via `max_concurrent`).
//   * `claude-code-leak/src/tools/TeamDeleteTool/TeamDeleteTool.ts:71-135`
//     — call body, active-member guard.
//   * `claude-code-leak/src/tools/SendMessageTool/SendMessageTool.ts:1-58`
//     — discriminated union of structured messages.

use nexo_team_store::{
    sanitize_name, validate_member_name_for_lead, validate_team_name, TeamEventRow, TeamMemberRow,
    TeamRow, TeamStoreError, DM_BODY_MAX_BYTES, TEAM_LEAD_NAME,
};

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn new_event_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn err(kind: &str, msg: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "kind": kind,
        "error": msg.into(),
    })
}

/// Best-effort audit insert. Failures are logged but never fail
/// the user-visible call.
async fn record_event(
    inner: &TeamTools,
    team_id: &str,
    kind: &str,
    actor: Option<&str>,
    payload: Value,
) {
    let row = TeamEventRow {
        event_id: new_event_id(),
        team_id: team_id.to_string(),
        kind: kind.to_string(),
        actor_member_name: actor.map(str::to_string),
        payload_json: payload.to_string(),
        created_at: now_ts(),
    };
    if let Err(e) = inner.store.record_event(&row).await {
        tracing::warn!(
            target: "team::audit",
            team_id,
            kind,
            error = %e,
            "[team] audit record failed"
        );
    }
}

#[async_trait]
impl ToolHandler for TeamCreateTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        // Guard: only the main goal of this agent (not a teammate
        // running inside another team) can spawn a team. Mirror
        // of leak's `claude-code-leak/src/tools/AgentTool/prompt.ts:279-282`
        // ("teammates cannot spawn other teammates").
        if ctx.is_teammate() {
            return Ok(err(
                "TeammateCannotSpawnTeammate",
                "teammates cannot spawn other teams",
            ));
        }
        // C2 — pull policy from the per-call effective binding
        // policy. Cheap clone (six primitives). Hot-reload pickup
        // semantics: a snapshot swap that flips `team.enabled` /
        // `team.max_*` is observed on the next intake event.
        let team_policy = self.inner.policy_for(ctx);
        if !team_policy.tool_enabled() {
            return Ok(err(
                "TeamingDisabled",
                "team feature not enabled for this agent",
            ));
        }

        let team_name = match args.get("team_name").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return Ok(err("Wire", "TeamCreate requires `team_name` (string)")),
        };
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let agent_type = args
            .get("agent_type")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let worktree_default = team_policy.worktree_per_member;
        let worktree_per_member = args
            .get("worktree_per_member")
            .and_then(|v| v.as_bool())
            .unwrap_or(worktree_default);

        let team_id = match validate_team_name(&team_name) {
            Ok(s) => s,
            Err(_) => {
                return Ok(err(
                    "InvalidName",
                    format!("invalid team_name: `{team_name}`"),
                ))
            }
        };

        // Per-agent concurrent cap.
        let active_count = self
            .inner
            .store
            .count_active_for_agent(&self.inner.agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("count_active_for_agent: {e}"))?;
        let cap = team_policy.effective_max_concurrent() as usize;
        if active_count >= cap {
            tracing::warn!(
                target: "team::cap_exceeded",
                agent = %self.inner.agent_id,
                count = active_count,
                cap,
                "[team] ConcurrentCapExceeded"
            );
            return Ok(json!({
                "ok": false,
                "kind": "ConcurrentCapExceeded",
                "count": active_count,
                "cap": cap,
                "error": format!("agent already leads {active_count} active teams (cap {cap})"),
            }));
        }

        let now = now_ts();
        let team_row = TeamRow {
            team_id: team_id.clone(),
            display_name: team_name.clone(),
            description: description.clone(),
            lead_agent_id: self.inner.agent_id.clone(),
            lead_goal_id: self.inner.current_goal_id.clone(),
            // 79.6.b will wire the real Phase 14 FlowFlow id;
            // the team_id is a stable placeholder so downstream
            // queries (TeamStatus) have a non-empty value.
            flow_id: team_id.clone(),
            worktree_per_member,
            created_at: now,
            deleted_at: None,
            last_active_at: now,
        };

        if let Err(e) = self.inner.store.create_team(&team_row).await {
            return match e {
                TeamStoreError::TeamNameTaken(existing) => Ok(json!({
                    "ok": false,
                    "kind": "TeamNameTaken",
                    "existing_team_id": existing,
                    "error": format!("team `{team_id}` already exists"),
                })),
                other => Err(anyhow::anyhow!("create_team: {other}")),
            };
        }

        // Seed the lead row.
        let lead_name = TEAM_LEAD_NAME.to_string();
        let lead_member = TeamMemberRow {
            team_id: team_id.clone(),
            name: lead_name.clone(),
            agent_id: self.inner.agent_id.clone(),
            agent_type: agent_type.clone(),
            model: None,
            goal_id: self.inner.current_goal_id.clone(),
            worktree_path: None,
            joined_at: now,
            is_active: true,
            last_active_at: now,
        };
        if let Err(e) = self.inner.store.add_member(&lead_member).await {
            tracing::warn!(
                target: "team::create",
                team_id = %team_id,
                error = %e,
                "[team] lead row insert failed — soft-deleting team"
            );
            let _ = self.inner.store.soft_delete_team(&team_id, now).await;
            return Err(anyhow::anyhow!("add_member(lead): {e}"));
        }

        record_event(
            &self.inner,
            &team_id,
            "team_created",
            Some(&lead_name),
            json!({
                "display_name": team_name,
                "description": description,
                "lead_agent_id": self.inner.agent_id,
                "agent_type": agent_type,
                "worktree_per_member": worktree_per_member,
            }),
        )
        .await;

        tracing::info!(
            target: "team::create",
            team_id = %team_id,
            agent = %self.inner.agent_id,
            "[team] created"
        );

        Ok(json!({
            "ok": true,
            "team_id": team_id,
            "lead_agent_id": self.inner.agent_id,
            "lead_member_name": lead_name,
            "flow_id": team_row.flow_id,
            "created_at": now,
            "instructions": "Members are added by the operator (Phase 79.6.b CLI) or by the runtime when sub-goals spawn. From inside a turn, the model coordinates members via `TeamSendMessage` (DM or broadcast) + `TeamStatus`. Wind down via `TeamSendMessage { to: \"broadcast\", message: { type: \"shutdown_request\" } }` then `TeamDelete`."
        }))
    }
}

#[async_trait]
impl ToolHandler for TeamDeleteTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        if ctx.is_teammate() {
            return Ok(err(
                "TeammateCannotDeleteTeam",
                "only the team lead can delete the team (you are running as a teammate)",
            ));
        }
        if !self.inner.policy_for(ctx).tool_enabled() {
            return Ok(err("TeamingDisabled", "team feature not enabled"));
        }
        let team_id_raw = match args.get("team_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return Ok(err("Wire", "TeamDelete requires `team_id`")),
        };
        let team_id = sanitize_name(&team_id_raw);

        let team = match self.inner.store.get_team(&team_id).await {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(err("TeamNotFound", format!("team `{team_id}` not found"))),
            Err(e) => return Err(anyhow::anyhow!("get_team: {e}")),
        };
        if team.lead_agent_id != self.inner.agent_id {
            return Ok(err(
                "NotLeader",
                format!(
                    "agent `{}` is not the lead of team `{team_id}`",
                    self.inner.agent_id
                ),
            ));
        }
        if team.deleted_at.is_some() {
            return Ok(err(
                "AlreadyDeleted",
                format!("team `{team_id}` was already soft-deleted"),
            ));
        }

        // Active members guard. Members whose `is_active = true`
        // are still running their turn; refuse and prompt the
        // model to send a shutdown_request first.
        let members = self
            .inner
            .store
            .list_members(&team_id)
            .await
            .map_err(|e| anyhow::anyhow!("list_members: {e}"))?;
        let active_non_lead: Vec<&TeamMemberRow> = members
            .iter()
            .filter(|m| m.is_active && m.name != TEAM_LEAD_NAME)
            .collect();
        if !active_non_lead.is_empty() {
            let names: Vec<String> = active_non_lead.iter().map(|m| m.name.clone()).collect();
            return Ok(json!({
                "ok": false,
                "kind": "BlockedByActiveMembers",
                "names": names,
                "error": format!(
                    "team `{team_id}` has {} active member(s): {}. Send `TeamSendMessage {{ to: \"broadcast\", message: {{ type: \"shutdown_request\" }} }}` and wait for them to idle before deleting.",
                    active_non_lead.len(),
                    names.join(", ")
                ),
            }));
        }

        let now = now_ts();
        if let Err(e) = self.inner.store.soft_delete_team(&team_id, now).await {
            return Err(anyhow::anyhow!("soft_delete_team: {e}"));
        }
        self.inner.router.drop_team(&team_id);

        let members_cleaned = members.len();
        record_event(
            &self.inner,
            &team_id,
            "team_deleted",
            Some(TEAM_LEAD_NAME),
            json!({ "members_cleaned": members_cleaned }),
        )
        .await;

        tracing::info!(
            target: "team::delete",
            team_id = %team_id,
            members_cleaned,
            "[team] deleted"
        );

        Ok(json!({
            "ok": true,
            "team_id": team_id,
            "members_cleaned": members_cleaned,
            // Force-kill of in-flight goals during drain is wired
            // in step 11 (main.rs SIGTERM hook); the model-driven
            // delete path here only proceeds when no member is
            // active, so this stays 0 by construction.
            "force_killed": 0,
        }))
    }
}

#[async_trait]
impl ToolHandler for TeamSendMessageTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        if !self.inner.policy_for(ctx).tool_enabled() {
            return Ok(err("TeamingDisabled", "team feature not enabled"));
        }
        let team_id_raw = match args.get("team_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return Ok(err("Wire", "TeamSendMessage requires `team_id`")),
        };
        let team_id = sanitize_name(&team_id_raw);
        let to = match args.get("to").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                return Ok(err(
                    "Wire",
                    "TeamSendMessage requires `to` (member name or \"broadcast\")",
                ))
            }
        };
        let body = args.get("message").cloned().unwrap_or(Value::Null);
        if body.is_null() {
            return Ok(err(
                "Wire",
                "TeamSendMessage requires `message` (string or structured object)",
            ));
        }

        // Body cap.
        let body_len = serde_json::to_vec(&body)
            .map(|b| b.len())
            .unwrap_or(usize::MAX);
        if body_len > DM_BODY_MAX_BYTES {
            return Ok(json!({
                "ok": false,
                "kind": "BodyTooLarge",
                "actual": body_len,
                "max": DM_BODY_MAX_BYTES,
                "error": format!("message body is {body_len} bytes (max {DM_BODY_MAX_BYTES})"),
            }));
        }

        let team = match self.inner.store.get_team(&team_id).await {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(err("TeamNotFound", format!("team `{team_id}` not found"))),
            Err(e) => return Err(anyhow::anyhow!("get_team: {e}")),
        };
        if team.deleted_at.is_some() {
            return Ok(err(
                "TeamDeleted",
                format!("team `{team_id}` is soft-deleted"),
            ));
        }

        // Membership: caller must be lead OR a member of the team.
        let is_lead = team.lead_agent_id == self.inner.agent_id
            && (ctx.team_member_name.as_deref() == Some(TEAM_LEAD_NAME) || !ctx.is_teammate());
        let caller_member_name = if is_lead {
            TEAM_LEAD_NAME.to_string()
        } else {
            // Find the member entry by agent_id.
            let members = self
                .inner
                .store
                .list_members(&team_id)
                .await
                .map_err(|e| anyhow::anyhow!("list_members: {e}"))?;
            match members
                .iter()
                .find(|m| m.agent_id == self.inner.agent_id)
                .map(|m| m.name.clone())
            {
                Some(n) => n,
                None => {
                    return Ok(err(
                        "NotMember",
                        format!(
                            "agent `{}` is not a member of team `{team_id}`",
                            self.inner.agent_id
                        ),
                    ))
                }
            }
        };

        // Broadcast guard: only the lead can broadcast.
        if to == "broadcast" {
            if !is_lead {
                return Ok(err(
                    "OnlyLeadCanBroadcast",
                    "only the team lead can publish to `broadcast`",
                ));
            }
            self.inner
                .router
                .publish_broadcast(&team_id, &caller_member_name, body.clone())
                .await
                .map_err(|e| anyhow::anyhow!("publish_broadcast: {e}"))?;
            record_event(
                &self.inner,
                &team_id,
                "broadcast_sent",
                Some(&caller_member_name),
                json!({ "body_bytes": body_len }),
            )
            .await;
            // touch_team so the idle reaper sees activity.
            let _ = self.inner.store.touch_team(&team_id, now_ts()).await;
            tracing::info!(
                target: "team::broadcast_sent",
                team_id = %team_id,
                from = %caller_member_name,
                body_bytes = body_len,
                "[team] broadcast"
            );
            return Ok(json!({
                "ok": true,
                "team_id": team_id,
                "to": "broadcast",
            }));
        }

        // Point-to-point. Validate target name.
        let target_name = match validate_member_name_for_lead(&to) {
            Ok(n) => n,
            Err(_) => return Ok(err("InvalidMemberName", format!("invalid `to`: {to}"))),
        };
        let correlation_id = uuid::Uuid::new_v4().to_string();
        self.inner
            .router
            .publish_dm(
                &team_id,
                &caller_member_name,
                &target_name,
                body.clone(),
                Some(correlation_id.clone()),
            )
            .await
            .map_err(|e| anyhow::anyhow!("publish_dm: {e}"))?;
        record_event(
            &self.inner,
            &team_id,
            "dm_sent",
            Some(&caller_member_name),
            json!({
                "to": target_name,
                "body_bytes": body_len,
                "correlation_id": correlation_id,
            }),
        )
        .await;
        let _ = self.inner.store.touch_team(&team_id, now_ts()).await;
        tracing::info!(
            target: "team::dm_sent",
            team_id = %team_id,
            from = %caller_member_name,
            to = %target_name,
            body_bytes = body_len,
            "[team] DM"
        );
        Ok(json!({
            "ok": true,
            "team_id": team_id,
            "to": target_name,
            "correlation_id": correlation_id,
        }))
    }
}

#[async_trait]
impl ToolHandler for TeamListTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        if !self.inner.policy_for(ctx).tool_enabled() {
            return Ok(err("TeamingDisabled", "team feature not enabled"));
        }
        let active_only = args
            .get("active_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let teams = self
            .inner
            .store
            .list_teams(Some(&self.inner.agent_id), active_only)
            .await
            .map_err(|e| anyhow::anyhow!("list_teams: {e}"))?;
        let json_teams: Vec<Value> = teams
            .iter()
            .map(|t| {
                json!({
                    "team_id": t.team_id,
                    "display_name": t.display_name,
                    "description": t.description,
                    "lead_agent_id": t.lead_agent_id,
                    "created_at": t.created_at,
                    "deleted_at": t.deleted_at,
                    "last_active_at": t.last_active_at,
                })
            })
            .collect();
        Ok(json!({
            "ok": true,
            "n": teams.len(),
            "teams": json_teams,
        }))
    }
}

#[async_trait]
impl ToolHandler for TeamStatusTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        if !self.inner.policy_for(ctx).tool_enabled() {
            return Ok(err("TeamingDisabled", "team feature not enabled"));
        }
        let team_id_raw = match args.get("team_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return Ok(err("Wire", "TeamStatus requires `team_id`")),
        };
        let team_id = sanitize_name(&team_id_raw);
        let team = match self.inner.store.get_team(&team_id).await {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(err("TeamNotFound", format!("team `{team_id}` not found"))),
            Err(e) => return Err(anyhow::anyhow!("get_team: {e}")),
        };
        let members = self
            .inner
            .store
            .list_members(&team_id)
            .await
            .map_err(|e| anyhow::anyhow!("list_members: {e}"))?;
        // Membership gate: caller is either the lead OR a member.
        let is_lead = team.lead_agent_id == self.inner.agent_id;
        let is_member = members.iter().any(|m| m.agent_id == self.inner.agent_id);
        if !is_lead && !is_member {
            // Mirror the leak's "tool only available within the team"
            // semantics — a non-member shouldn't even confirm the
            // team's existence beyond the explicit refusal.
            let _ = ctx; // silence unused
            return Ok(err(
                "NotMember",
                "only the team lead or a current member may read the team status",
            ));
        }

        let n_running = members.iter().filter(|m| m.is_active).count();
        let n_idle = members.len().saturating_sub(n_running);

        let json_members: Vec<Value> = members
            .iter()
            .map(|m| {
                json!({
                    "name": m.name,
                    "agent_id": m.agent_id,
                    "agent_type": m.agent_type,
                    "model": m.model,
                    "joined_at": m.joined_at,
                    "is_active": m.is_active,
                    "last_active_at": m.last_active_at,
                })
            })
            .collect();

        Ok(json!({
            "ok": true,
            "team": {
                "team_id": team.team_id,
                "display_name": team.display_name,
                "description": team.description,
                "lead_agent_id": team.lead_agent_id,
                "created_at": team.created_at,
                "deleted_at": team.deleted_at,
                "last_active_at": team.last_active_at,
                "worktree_per_member": team.worktree_per_member,
            },
            "members": json_members,
            // Phase 14 FlowFlow integration is 79.6.b. Until
            // then, surface zero counts so the field is stable.
            "task_summary": {
                "pending": 0,
                "running": n_running,
                "done": n_idle,
            },
        }))
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
        for k in [
            "team_name",
            "description",
            "agent_type",
            "worktree_per_member",
        ] {
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

    // -----------------------------------------------------------
    // Handler tests (step 9). Use the real `SqliteTeamStore` in
    // memory + `LocalBroker` so every wire path exercises real
    // SQL + real broker subscribe/publish semantics.
    // -----------------------------------------------------------

    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use nexo_team_store::SqliteTeamStore;
    use std::sync::Arc;

    fn agent_cfg_with_team(team: TeamPolicy) -> AgentConfig {
        AgentConfig {
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
            team,
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
            empresa_id: None,
            extensions_config: std::collections::BTreeMap::new(),
        }
    }

    fn agent_cfg() -> AgentConfig {
        agent_cfg_with_team(TeamPolicy {
            enabled: true,
            ..TeamPolicy::default()
        })
    }

    /// C2 — build a ctx whose `effective_policy().team` returns the
    /// given policy. Tests that want to exercise handler-side policy
    /// gating use this helper; tests that just need a "team enabled"
    /// ctx use [`ctx_lead`].
    async fn ctx_lead_with_policy(policy: TeamPolicy) -> AgentContext {
        AgentContext::new(
            "cody",
            Arc::new(agent_cfg_with_team(policy)),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    async fn ctx_lead() -> AgentContext {
        AgentContext::new(
            "cody",
            Arc::new(agent_cfg()),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    async fn ctx_teammate() -> AgentContext {
        ctx_lead().await.with_team("feature-x", "researcher")
    }

    /// C2 — `policy` argument removed; handlers pull from
    /// `ctx.effective_policy().team`. Tests pass policy via ctx.
    async fn build_inner(agent_id: &str) -> Arc<TeamTools> {
        let broker = Arc::new(AnyBroker::local());
        let store: Arc<dyn TeamStore> = Arc::new(SqliteTeamStore::open_in_memory().await.unwrap());
        let router = TeamMessageRouter::new(broker.clone());
        let cancel = tokio_util::sync::CancellationToken::new();
        router.spawn(cancel);
        // Detach broker from Arc<AnyBroker> back to AnyBroker (it's
        // Clone) so TeamTools holds its own copy + the router holds
        // the Arc.
        TeamTools::new(store, router, (*broker).clone(), agent_id, "lead-goal-1")
    }

    #[tokio::test]
    async fn team_create_rejects_when_capability_disabled() {
        // C2 — `enabled` lives on the ctx's effective policy, not on
        // `TeamTools` anymore. Use `ctx_lead_with_policy` to get a
        // ctx that resolves `team.enabled = false`.
        let inner = build_inner("cody").await;
        let tool = TeamCreateTool::new(inner);
        let res = tool
            .call(
                &ctx_lead_with_policy(TeamPolicy::default()).await,
                json!({ "team_name": "feature-x" }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "TeamingDisabled");
    }

    #[tokio::test]
    async fn team_create_returns_team_id_and_lead_member() {
        let inner = build_inner("cody").await;
        let tool = TeamCreateTool::new(inner.clone());
        let res = tool
            .call(
                &ctx_lead().await,
                json!({
                    "team_name": "Feature-X",
                    "description": "Build the new auth flow",
                    "agent_type": "coordinator"
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["team_id"], "feature-x");
        assert_eq!(res["lead_member_name"], "team-lead");

        // Audit row recorded.
        let events = inner
            .store
            .tail_events(Some("feature-x"), 10)
            .await
            .unwrap();
        assert!(events.iter().any(|e| e.kind == "team_created"));
    }

    #[tokio::test]
    async fn team_create_rejects_collision() {
        let inner = build_inner("cody").await;
        let tool = TeamCreateTool::new(inner);
        let _ = tool
            .call(&ctx_lead().await, json!({ "team_name": "feature-x" }))
            .await
            .unwrap();
        let res = tool
            .call(&ctx_lead().await, json!({ "team_name": "feature-x" }))
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "TeamNameTaken");
        assert_eq!(res["existing_team_id"], "feature-x");
    }

    #[tokio::test]
    async fn team_create_rejects_when_at_max_concurrent() {
        // C2 — `max_concurrent` lives on ctx.effective_policy().team
        // now. Build a ctx with the cap = 1, drive both calls through
        // it.
        let inner = build_inner("cody").await;
        let ctx = ctx_lead_with_policy(TeamPolicy {
            enabled: true,
            max_concurrent: 1,
            ..TeamPolicy::default()
        })
        .await;
        let tool = TeamCreateTool::new(inner);
        let _ = tool
            .call(&ctx, json!({ "team_name": "first" }))
            .await
            .unwrap();
        let res = tool
            .call(&ctx, json!({ "team_name": "second" }))
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "ConcurrentCapExceeded");
        assert_eq!(res["cap"], 1);
    }

    #[tokio::test]
    async fn team_create_refuses_from_teammate_context() {
        let inner = build_inner("cody").await;
        let tool = TeamCreateTool::new(inner);
        let res = tool
            .call(&ctx_teammate().await, json!({ "team_name": "nested" }))
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "TeammateCannotSpawnTeammate");
    }

    #[tokio::test]
    async fn team_delete_only_lead_can_invoke() {
        // Create the team as `cody`, then try to delete from a
        // different-agent inner.
        let inner_lead = build_inner("cody").await;
        TeamCreateTool::new(inner_lead.clone())
            .call(&ctx_lead().await, json!({ "team_name": "feature-x" }))
            .await
            .unwrap();

        // Build a SECOND inner pointing at the same store with a
        // different agent_id. C2 — policy comes from ctx.
        let inner_other = TeamTools::new(
            Arc::clone(&inner_lead.store),
            Arc::clone(&inner_lead.router),
            inner_lead.broker.clone(),
            "other-agent",
            "other-goal",
        );
        let res = TeamDeleteTool::new(inner_other)
            .call(
                &AgentContext::new(
                    "other-agent",
                    Arc::new(agent_cfg()),
                    AnyBroker::local(),
                    Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
                ),
                json!({ "team_id": "feature-x" }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "NotLeader");
    }

    #[tokio::test]
    async fn team_delete_blocks_when_running_members() {
        let inner = build_inner("cody").await;
        TeamCreateTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "team_name": "feature-x" }))
            .await
            .unwrap();
        // Add an active non-lead member directly via the store.
        inner
            .store
            .add_member(&TeamMemberRow {
                team_id: "feature-x".into(),
                name: "researcher".into(),
                agent_id: "rsr".into(),
                agent_type: None,
                model: None,
                goal_id: "g".into(),
                worktree_path: None,
                joined_at: 0,
                is_active: true,
                last_active_at: 0,
            })
            .await
            .unwrap();

        let res = TeamDeleteTool::new(inner)
            .call(&ctx_lead().await, json!({ "team_id": "feature-x" }))
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "BlockedByActiveMembers");
        let names = res["names"].as_array().unwrap();
        assert!(names.iter().any(|v| v == "researcher"));
    }

    #[tokio::test]
    async fn team_delete_soft_deletes_and_records_event() {
        let inner = build_inner("cody").await;
        TeamCreateTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "team_name": "feature-x" }))
            .await
            .unwrap();
        let res = TeamDeleteTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "team_id": "feature-x" }))
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["team_id"], "feature-x");

        let team = inner.store.get_team("feature-x").await.unwrap().unwrap();
        assert!(team.deleted_at.is_some());

        let events = inner
            .store
            .tail_events(Some("feature-x"), 10)
            .await
            .unwrap();
        assert!(events.iter().any(|e| e.kind == "team_deleted"));
    }

    #[tokio::test]
    async fn team_send_message_rejects_oversized_body() {
        let inner = build_inner("cody").await;
        TeamCreateTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "team_name": "feature-x" }))
            .await
            .unwrap();
        let big = "x".repeat(DM_BODY_MAX_BYTES + 1);
        let res = TeamSendMessageTool::new(inner)
            .call(
                &ctx_lead().await,
                json!({
                    "team_id": "feature-x",
                    "to": "researcher",
                    "message": big
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "BodyTooLarge");
    }

    #[tokio::test]
    async fn team_send_message_broadcast_requires_lead() {
        let inner = build_inner("cody").await;
        TeamCreateTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "team_name": "feature-x" }))
            .await
            .unwrap();
        // Add the caller as a member.
        inner
            .store
            .add_member(&TeamMemberRow {
                team_id: "feature-x".into(),
                name: "researcher".into(),
                agent_id: "cody".into(),
                agent_type: None,
                model: None,
                goal_id: "g".into(),
                worktree_path: None,
                joined_at: 0,
                is_active: true,
                last_active_at: 0,
            })
            .await
            .unwrap();
        // Send as a teammate (not the lead).
        let res = TeamSendMessageTool::new(inner)
            .call(
                &ctx_teammate().await,
                json!({
                    "team_id": "feature-x",
                    "to": "broadcast",
                    "message": { "type": "shutdown_request" }
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "OnlyLeadCanBroadcast");
    }

    #[tokio::test]
    async fn team_send_message_dm_publishes_and_records() {
        let inner = build_inner("cody").await;
        TeamCreateTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "team_name": "feature-x" }))
            .await
            .unwrap();
        // Subscribe a "researcher" inbox.
        let _rx = inner.router.subscribe_member("feature-x", "researcher");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let res = TeamSendMessageTool::new(inner.clone())
            .call(
                &ctx_lead().await,
                json!({
                    "team_id": "feature-x",
                    "to": "researcher",
                    "message": { "ask": "ready?" }
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        assert!(res["correlation_id"].is_string());

        let events = inner
            .store
            .tail_events(Some("feature-x"), 20)
            .await
            .unwrap();
        assert!(events.iter().any(|e| e.kind == "dm_sent"));
    }

    #[tokio::test]
    async fn team_list_filters_active_only() {
        let inner = build_inner("cody").await;
        TeamCreateTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "team_name": "a" }))
            .await
            .unwrap();
        TeamCreateTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "team_name": "b" }))
            .await
            .unwrap();
        TeamDeleteTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "team_id": "a" }))
            .await
            .unwrap();
        let res = TeamListTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "active_only": true }))
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["n"], 1);
        let teams = res["teams"].as_array().unwrap();
        assert_eq!(teams[0]["team_id"], "b");

        let all = TeamListTool::new(inner)
            .call(&ctx_lead().await, json!({ "active_only": false }))
            .await
            .unwrap();
        assert_eq!(all["n"], 2);
    }

    #[tokio::test]
    async fn team_status_rejects_non_member() {
        let inner_lead = build_inner("cody").await;
        TeamCreateTool::new(inner_lead.clone())
            .call(&ctx_lead().await, json!({ "team_name": "feature-x" }))
            .await
            .unwrap();

        // Build a fresh inner pointing at the same store but a
        // different agent. C2 — policy comes from ctx.
        let inner_other = TeamTools::new(
            Arc::clone(&inner_lead.store),
            Arc::clone(&inner_lead.router),
            inner_lead.broker.clone(),
            "stranger",
            "g",
        );
        let res = TeamStatusTool::new(inner_other)
            .call(
                &AgentContext::new(
                    "stranger",
                    Arc::new(agent_cfg()),
                    AnyBroker::local(),
                    Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
                ),
                json!({ "team_id": "feature-x" }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["kind"], "NotMember");
    }

    #[tokio::test]
    async fn team_status_returns_members_and_summary_for_lead() {
        let inner = build_inner("cody").await;
        TeamCreateTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "team_name": "feature-x" }))
            .await
            .unwrap();
        let res = TeamStatusTool::new(inner.clone())
            .call(&ctx_lead().await, json!({ "team_id": "feature-x" }))
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["team"]["team_id"], "feature-x");
        let members = res["members"].as_array().unwrap();
        // Only the lead so far.
        assert_eq!(members.len(), 1);
        assert_eq!(members[0]["name"], "team-lead");
    }

    // ---- C2: per-call policy pull from EffectiveBindingPolicy ----

    /// Same shared `Arc<TeamTools>` driven through TWO ctxs with
    /// DIFFERENT `team.max_concurrent` values produces different
    /// outcomes — proves the handler reads policy fresh per call,
    /// not from a captured field. This is the reload-pickup proof
    /// at the unit level (integration test in `hot_reload_test.rs`
    /// drives the same path through `ConfigReloadCoordinator`).
    #[tokio::test]
    async fn team_create_policy_pulled_per_call_from_ctx() {
        let inner = build_inner("cody").await;
        let tool = TeamCreateTool::new(inner);

        // First ctx: cap=1. First call succeeds, second fails.
        let ctx_cap1 = ctx_lead_with_policy(TeamPolicy {
            enabled: true,
            max_concurrent: 1,
            ..TeamPolicy::default()
        })
        .await;
        let r1 = tool
            .call(&ctx_cap1, json!({ "team_name": "first" }))
            .await
            .unwrap();
        assert_eq!(r1["ok"], true);
        let r2 = tool
            .call(&ctx_cap1, json!({ "team_name": "second" }))
            .await
            .unwrap();
        assert_eq!(r2["ok"], false);
        assert_eq!(r2["kind"], "ConcurrentCapExceeded");

        // SAME tool instance — but a NEW ctx with cap=8 (mirrors a
        // hot-reload widening the cap). The next call now succeeds.
        let ctx_cap8 = ctx_lead_with_policy(TeamPolicy {
            enabled: true,
            max_concurrent: 8,
            ..TeamPolicy::default()
        })
        .await;
        let r3 = tool
            .call(&ctx_cap8, json!({ "team_name": "third" }))
            .await
            .unwrap();
        assert_eq!(r3["ok"], true, "after cap widened the call must succeed");
    }

    /// `team.enabled = false → true` on a hot-reload is observed by
    /// the same tool instance via the new ctx. This is the
    /// behaviour-equivalent of leak's
    /// `claude-code-leak/src/services/mcp/useManageMCPConnections.ts:624`
    /// (invalidate-and-refetch — no actor restart).
    #[tokio::test]
    async fn team_enabled_flip_picked_up_via_new_ctx() {
        let inner = build_inner("cody").await;
        let tool = TeamCreateTool::new(inner);

        // Pre-reload: disabled.
        let ctx_off = ctx_lead_with_policy(TeamPolicy::default()).await;
        let r1 = tool
            .call(&ctx_off, json!({ "team_name": "early" }))
            .await
            .unwrap();
        assert_eq!(r1["kind"], "TeamingDisabled");

        // Post-reload: enabled. Same tool, new ctx.
        let ctx_on = ctx_lead_with_policy(TeamPolicy {
            enabled: true,
            ..TeamPolicy::default()
        })
        .await;
        let r2 = tool
            .call(&ctx_on, json!({ "team_name": "post-reload" }))
            .await
            .unwrap();
        assert_eq!(r2["ok"], true);
    }
}
