//! Phase 79.4 — `TodoWrite` intra-turn scratch list tool.
//!
//! Lift from `claude-code-leak/src/tools/TodoWriteTool/TodoWriteTool.ts:31-115`
//! (full-replace semantics, wipe-on-all-completed, return both old
//! and new lists for the diff).
//!
//! This tool does NOT persist across goal lifetime — by design:
//!
//! * Long Phase 67 driver-loop turns coordinate sub-steps via
//!   TodoWrite without paying the cost of spawning sub-goals.
//! * Operators who want persistent multi-day work programs use
//!   Phase 14 TaskFlow — distinct shape, distinct lifecycle.
//!
//! Permission model: zero checks, classified `ReadOnly` by
//! `nexo_core::plan_mode::READ_ONLY_TOOLS` so the model can update
//! its own scratch list even while plan mode is on.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use crate::todo::{all_completed, validate_todos, TodoItem, TodoList, TodoStatus};
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};

/// Description shown to the model in the tool catalogue. Lift the
/// canonical wording from
/// `claude-code-leak/src/tools/TodoWriteTool/prompt.ts:184`.
pub const TODO_WRITE_DESCRIPTION: &str = "Update the todo list for the current session. To be used proactively and often to track progress and pending tasks. Make sure that at least one task is in_progress at all times. Always provide both content (imperative) and activeForm (present continuous) for each task. Full-replace semantics: every call replaces the entire list.";

pub struct TodoWriteTool;

impl TodoWriteTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "TodoWrite".to_string(),
            description: TODO_WRITE_DESCRIPTION.to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The full updated todo list. Replaces the previous list entirely. When every item has status=completed, the runtime wipes the list to [] so the next planning cycle starts fresh.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "Imperative form (e.g. \"Run tests\")."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                },
                                "activeForm": {
                                    "type": "string",
                                    "description": "Present continuous form (e.g. \"Running tests\"). Rendered while the item is in_progress."
                                }
                            },
                            "required": ["content", "status", "activeForm"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }
}

/// Parse one item out of the JSON Value, accepting both the leak's
/// `activeForm` (camelCase) and our snake_case `active_form` so the
/// tool is friendly regardless of which dialect the model emits.
fn parse_item(v: &Value, idx: usize) -> anyhow::Result<TodoItem> {
    let content = v
        .get("content")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("todo[{idx}]: missing string `content`"))?
        .to_string();
    let status_str = v
        .get("status")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("todo[{idx}]: missing string `status`"))?;
    let status = match status_str {
        "pending" => TodoStatus::Pending,
        "in_progress" => TodoStatus::InProgress,
        "completed" => TodoStatus::Completed,
        other => {
            return Err(anyhow::anyhow!(
                "todo[{idx}]: unknown status `{other}` (expected pending|in_progress|completed)"
            ));
        }
    };
    // Accept either spelling — the leak uses camelCase, the rest of
    // nexo-rs leans snake_case. Either survives the round-trip.
    let active_form = v
        .get("activeForm")
        .or_else(|| v.get("active_form"))
        .and_then(|x| x.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!("todo[{idx}]: missing string `activeForm` (or `active_form`)")
        })?
        .to_string();
    Ok(TodoItem {
        content,
        status,
        active_form,
    })
}

#[async_trait]
impl ToolHandler for TodoWriteTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let raw = args
            .get("todos")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("TodoWrite requires `todos` (array)"))?;

        let mut new_items: TodoList = Vec::with_capacity(raw.len());
        for (idx, v) in raw.iter().enumerate() {
            new_items.push(parse_item(v, idx)?);
        }

        // Validation BEFORE swapping state — a bad write must not
        // clobber the existing list.
        validate_todos(&new_items)?;

        // Lift from `TodoWriteTool.ts:69-70`: when every item is
        // completed, persist `[]` so the next planning cycle starts
        // fresh. The `oldTodos` echo still carries the just-completed
        // list so the model can write a summary.
        let wipe = all_completed(&new_items);
        let stored: TodoList = if wipe {
            TodoList::new()
        } else {
            new_items.clone()
        };

        let mut guard = ctx.todos.write().await;
        let old = std::mem::take(&mut *guard);
        *guard = stored;
        drop(guard);

        let in_progress_count = new_items
            .iter()
            .filter(|t| t.status == TodoStatus::InProgress)
            .count();

        Ok(json!({
            "old_todos": old,
            "new_todos": new_items,
            "wiped_on_all_completed": wipe,
            "in_progress_count": in_progress_count,
            "instructions": "Todos updated. Keep exactly one item `in_progress` at a time. Mark completed IMMEDIATELY after finishing each task; do not batch completions."
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use std::sync::Arc;

    fn ctx() -> AgentContext {
        let cfg = AgentConfig {
            id: "a".into(),
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
        };
        AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    fn item(content: &str, status: &str, active: &str) -> Value {
        json!({"content": content, "status": status, "activeForm": active})
    }

    #[tokio::test]
    async fn first_call_seeds_the_list() {
        let c = ctx();
        let res = TodoWriteTool
            .call(
                &c,
                json!({"todos": [item("Run tests", "pending", "Running tests")]}),
            )
            .await
            .unwrap();
        assert_eq!(res["old_todos"], json!([]));
        assert_eq!(res["new_todos"][0]["content"], "Run tests");
        assert_eq!(c.todos.read().await.len(), 1);
    }

    #[tokio::test]
    async fn second_call_replaces_full_list() {
        let c = ctx();
        TodoWriteTool
            .call(
                &c,
                json!({"todos": [
                    item("A", "pending", "Doing A"),
                    item("B", "pending", "Doing B")
                ]}),
            )
            .await
            .unwrap();
        let res = TodoWriteTool
            .call(&c, json!({"todos": [item("C", "in_progress", "Doing C")]}))
            .await
            .unwrap();
        assert_eq!(res["old_todos"].as_array().unwrap().len(), 2);
        assert_eq!(res["new_todos"].as_array().unwrap().len(), 1);
        assert_eq!(c.todos.read().await.len(), 1);
        assert_eq!(c.todos.read().await[0].content, "C");
    }

    #[tokio::test]
    async fn all_completed_wipes_stored_list_but_echoes_old() {
        let c = ctx();
        TodoWriteTool
            .call(&c, json!({"todos": [item("A", "in_progress", "Doing A")]}))
            .await
            .unwrap();
        let res = TodoWriteTool
            .call(&c, json!({"todos": [item("A", "completed", "Doing A")]}))
            .await
            .unwrap();
        assert_eq!(res["wiped_on_all_completed"], true);
        // Stored list is empty.
        assert!(c.todos.read().await.is_empty());
        // But the response still carries new_todos = the just-finished list
        // so the model can summarise.
        assert_eq!(res["new_todos"].as_array().unwrap().len(), 1);
        assert_eq!(res["new_todos"][0]["status"], "completed");
    }

    #[tokio::test]
    async fn accepts_snake_case_active_form_too() {
        let c = ctx();
        let res = TodoWriteTool
            .call(
                &c,
                json!({"todos": [{
                    "content": "X",
                    "status": "pending",
                    "active_form": "Doing X"
                }]}),
            )
            .await;
        assert!(res.is_ok(), "snake_case `active_form` should be accepted");
    }

    #[tokio::test]
    async fn rejects_too_many_items_without_clobbering_state() {
        let c = ctx();
        TodoWriteTool
            .call(&c, json!({"todos": [item("seed", "pending", "Seeding")]}))
            .await
            .unwrap();
        let too_many: Vec<Value> = (0..51)
            .map(|i| item(&format!("c{i}"), "pending", &format!("a{i}")))
            .collect();
        let err = TodoWriteTool
            .call(&c, json!({"todos": too_many}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("too many"), "got: {err}");
        // State preserved on rejection.
        assert_eq!(c.todos.read().await.len(), 1);
        assert_eq!(c.todos.read().await[0].content, "seed");
    }

    #[tokio::test]
    async fn rejects_unknown_status() {
        let c = ctx();
        let err = TodoWriteTool
            .call(
                &c,
                json!({"todos": [{
                    "content": "X",
                    "status": "wat",
                    "activeForm": "Doing"
                }]}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown status"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_missing_active_form() {
        let c = ctx();
        let err = TodoWriteTool
            .call(
                &c,
                json!({"todos": [{
                    "content": "X",
                    "status": "pending"
                }]}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("activeForm"), "got: {err}");
    }

    #[tokio::test]
    async fn empty_todos_argument_is_valid_clears_list() {
        let c = ctx();
        TodoWriteTool
            .call(&c, json!({"todos": [item("seed", "pending", "Seeding")]}))
            .await
            .unwrap();
        let res = TodoWriteTool.call(&c, json!({"todos": []})).await.unwrap();
        assert_eq!(res["old_todos"].as_array().unwrap().len(), 1);
        assert_eq!(res["new_todos"].as_array().unwrap().len(), 0);
        assert!(c.todos.read().await.is_empty());
    }

    #[tokio::test]
    async fn in_progress_count_is_reported() {
        let c = ctx();
        let res = TodoWriteTool
            .call(
                &c,
                json!({"todos": [
                    item("A", "completed", "Doing A"),
                    item("B", "in_progress", "Doing B"),
                    item("C", "pending", "Doing C"),
                ]}),
            )
            .await
            .unwrap();
        assert_eq!(res["in_progress_count"], 1);
    }
}
