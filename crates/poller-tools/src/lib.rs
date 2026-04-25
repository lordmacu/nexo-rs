//! LLM-callable tools for the Phase 19 poller subsystem.
//!
//! Lives outside `agent-core` so the dependency graph stays acyclic
//! (core → poller → plugin-google → core would loop). `main.rs`
//! pulls this crate alongside `agent-core` and registers the tools
//! per agent.
//!
//! Six tools, all read + control on already-declared jobs:
//!  - `pollers_list`    list every job + state
//!  - `pollers_show`    detail for one job
//!  - `pollers_run`     manual tick (bypasses schedule + lease)
//!  - `pollers_pause`   set paused = 1
//!  - `pollers_resume`  set paused = 0
//!  - `pollers_reset`   wipe cursor / consecutive_errors
//!
//! Create / delete are intentionally not exposed: a prompt-injection
//! could plant a `webhook_poll` job aimed at internal infra. Operators
//! still own `pollers.yaml` + `agent pollers reload`.

use std::sync::Arc;

use agent_core::agent::context::AgentContext;
use agent_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use agent_llm::ToolDef;
use agent_poller::PollerRunner;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PollersListTool {
    runner: Arc<PollerRunner>,
}
impl PollersListTool {
    pub fn new(runner: Arc<PollerRunner>) -> Self { Self { runner } }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "pollers_list".to_string(),
            description:
                "List every configured poll job (gmail, rss, calendar, …) with its kind, agent owner, paused flag, last status and counters."
                    .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }
}
#[async_trait]
impl ToolHandler for PollersListTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        let jobs = self.runner.list_jobs().await?;
        Ok(serde_json::to_value(&jobs)?)
    }
}

pub struct PollersShowTool {
    runner: Arc<PollerRunner>,
}
impl PollersShowTool {
    pub fn new(runner: Arc<PollerRunner>) -> Self { Self { runner } }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "pollers_show".to_string(),
            description: "Inspect a single poll job by id.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Job id (matches pollers.yaml)" }
                },
                "required": ["id"]
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for PollersShowTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let id = args["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("pollers_show requires `id`"))?;
        let jobs = self.runner.list_jobs().await?;
        let job = jobs
            .into_iter()
            .find(|j| j.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown poll job '{id}'"))?;
        Ok(serde_json::to_value(&job)?)
    }
}

pub struct PollersRunTool {
    runner: Arc<PollerRunner>,
}
impl PollersRunTool {
    pub fn new(runner: Arc<PollerRunner>) -> Self { Self { runner } }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "pollers_run".to_string(),
            description:
                "Trigger one tick of a poll job out-of-band (bypasses schedule + lease). Returns items_seen / items_dispatched / deliveries."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Job id" }
                },
                "required": ["id"]
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for PollersRunTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let id = args["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("pollers_run requires `id`"))?;
        let outcome = self.runner.run_once(id).await?;
        Ok(json!({
            "ok": true,
            "items_seen": outcome.items_seen,
            "items_dispatched": outcome.items_dispatched,
            "deliveries": outcome.deliver.len(),
        }))
    }
}

pub struct PollersPauseTool {
    runner: Arc<PollerRunner>,
}
impl PollersPauseTool {
    pub fn new(runner: Arc<PollerRunner>) -> Self { Self { runner } }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "pollers_pause".to_string(),
            description:
                "Pause a poll job. The schedule stops firing until pollers_resume is called."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Job id" }
                },
                "required": ["id"]
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for PollersPauseTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let id = args["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("pollers_pause requires `id`"))?;
        self.runner.set_paused(id, true).await?;
        Ok(json!({"ok": true, "paused": true}))
    }
}

pub struct PollersResumeTool {
    runner: Arc<PollerRunner>,
}
impl PollersResumeTool {
    pub fn new(runner: Arc<PollerRunner>) -> Self { Self { runner } }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "pollers_resume".to_string(),
            description: "Resume a paused poll job.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Job id" }
                },
                "required": ["id"]
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for PollersResumeTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let id = args["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("pollers_resume requires `id`"))?;
        self.runner.set_paused(id, false).await?;
        Ok(json!({"ok": true, "paused": false}))
    }
}

pub struct PollersResetTool {
    runner: Arc<PollerRunner>,
}
impl PollersResetTool {
    pub fn new(runner: Arc<PollerRunner>) -> Self { Self { runner } }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "pollers_reset".to_string(),
            description:
                "Reset the cursor and error state of a poll job. Destructive: the next tick re-baselines (gmail will scan from `newer_than`, calendar will fetch a fresh syncToken). Confirm intent before calling."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Job id" }
                },
                "required": ["id"]
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for PollersResetTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let id = args["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("pollers_reset requires `id`"))?;
        self.runner.reset_cursor(id).await?;
        Ok(json!({"ok": true, "reset": true}))
    }
}

/// Adapter wrapping a `agent_poller::CustomToolHandler` into the
/// `agent_core::ToolHandler` shape. Captures the runner so each
/// LLM call gets a fresh handle.
struct CustomToolAdapter {
    runner: Arc<PollerRunner>,
    inner: Arc<dyn agent_poller::CustomToolHandler>,
}
#[async_trait]
impl ToolHandler for CustomToolAdapter {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        self.inner.call(Arc::clone(&self.runner), args).await
    }
}

/// Wire the six generic `pollers_*` tools plus every per-kind custom
/// tool exposed by the registered `Poller` impls. Called from `main.rs`
/// per agent.
pub fn register_all(registry: &ToolRegistry, runner: Arc<PollerRunner>) {
    registry.register(PollersListTool::tool_def(), PollersListTool::new(runner.clone()));
    registry.register(PollersShowTool::tool_def(), PollersShowTool::new(runner.clone()));
    registry.register(PollersRunTool::tool_def(), PollersRunTool::new(runner.clone()));
    registry.register(PollersPauseTool::tool_def(), PollersPauseTool::new(runner.clone()));
    registry.register(
        PollersResumeTool::tool_def(),
        PollersResumeTool::new(runner.clone()),
    );
    registry.register(PollersResetTool::tool_def(), PollersResetTool::new(runner.clone()));

    // Per-kind custom tools — each registered Poller impl can return
    // a Vec<CustomToolSpec>. Empty by default.
    for spec in runner.collect_custom_tools() {
        registry.register(
            spec.def,
            CustomToolAdapter {
                runner: Arc::clone(&runner),
                inner: spec.handler,
            },
        );
    }
}
