//! `email_health` — admin tool over the per-instance HealthMap.
//!
//! Snapshots `AccountHealth` for every account (or one by `instance`)
//! so admin agents can drive dashboards / alerts without scraping
//! Prometheus. Read-only — never mutates worker state.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::health::WorkerState;

use super::context::EmailToolContext;

#[derive(Debug, Default, Deserialize)]
struct HealthArgs {
    /// Optional instance filter. Omitted = every instance.
    #[serde(default)]
    instance: Option<String>,
}

pub struct EmailHealthTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailHealthTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_health".into(),
            description:
                "Snapshot per-account inbound/outbound health: worker state (Connecting / Idle / \
                 Polling / Down), last_connect_ok_ts, last_idle_alive_ts, last_poll_ts, \
                 consecutive_failures, messages_seen_total, outbound_queue_depth, \
                 outbound_dlq_depth, sent / failed totals, and the last error. Read-only."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "instance": { "type": "string", "description": "Filter to one instance. Omit for all." }
                },
                "additionalProperties": false
            }),
        }
    }

    pub async fn run(&self, args: Value) -> Value {
        let parsed: HealthArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => return json!({ "ok": false, "error": format!("invalid arguments: {e}") }),
        };
        match self.run_inner(parsed).await {
            Ok(v) => v,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }
    }

    async fn run_inner(&self, args: HealthArgs) -> Result<Value> {
        // Walk the DashMap snapshotting each entry. We sort the
        // payload by instance id so output is deterministic for
        // dashboards / diffs.
        let mut entries: Vec<(
            String,
            Arc<tokio::sync::RwLock<crate::health::AccountHealth>>,
        )> = self
            .ctx
            .health
            .iter()
            .map(|kv| (kv.key().clone(), kv.value().clone()))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out: Vec<Value> = Vec::with_capacity(entries.len());
        for (instance, lock) in entries {
            if let Some(filter) = &args.instance {
                if filter != &instance {
                    continue;
                }
            }
            let h = lock.read().await;
            out.push(json!({
                "instance": instance,
                "state": worker_state_label(h.state),
                "state_value": h.state.as_metric_value(),
                "last_connect_ok_ts": h.last_connect_ok_ts,
                "last_idle_alive_ts": h.last_idle_alive_ts,
                "last_poll_ts": h.last_poll_ts,
                "consecutive_failures": h.consecutive_failures,
                "messages_seen_total": h.messages_seen_total,
                "outbound_queue_depth": h.outbound_queue_depth,
                "outbound_dlq_depth": h.outbound_dlq_depth,
                "outbound_sent_total": h.outbound_sent_total,
                "outbound_failed_total": h.outbound_failed_total,
                "last_error": h.last_error,
            }));
        }

        Ok(json!({
            "ok": true,
            "instances": out,
            "instance_count": out.len(),
        }))
    }
}

#[async_trait]
impl ToolHandler for EmailHealthTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailHealthTool::new(ctx);
    registry.register(EmailHealthTool::tool_def(), tool);
}

fn worker_state_label(s: WorkerState) -> &'static str {
    match s {
        WorkerState::Connecting => "connecting",
        WorkerState::Idle => "idle",
        WorkerState::Polling => "polling",
        WorkerState::Down => "down",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::AccountHealth;
    use crate::tool::dispatcher_stub::stub_ctx;
    use dashmap::DashMap;
    use tokio::sync::RwLock;

    fn ctx_with_health_entries(entries: Vec<(&str, AccountHealth)>) -> Arc<EmailToolContext> {
        let (ctx, _) = stub_ctx(entries.iter().map(|(i, _)| (*i).into()).collect(), false);
        let map = DashMap::new();
        for (i, h) in entries {
            map.insert(i.into(), Arc::new(RwLock::new(h)));
        }
        Arc::new(EmailToolContext {
            creds: ctx.creds.clone(),
            google: ctx.google.clone(),
            config: ctx.config.clone(),
            dispatcher: ctx.dispatcher.clone(),
            health: Arc::new(map),
            bounce_store: ctx.bounce_store.clone(),
            attachment_store: ctx.attachment_store.clone(),
            attachments_dir: ctx.attachments_dir.clone(),
        })
    }

    #[tokio::test]
    async fn returns_one_entry_per_instance() {
        let ctx = ctx_with_health_entries(vec![
            (
                "a",
                AccountHealth {
                    state: WorkerState::Idle,
                    last_connect_ok_ts: 100,
                    ..Default::default()
                },
            ),
            (
                "b",
                AccountHealth {
                    state: WorkerState::Down,
                    last_error: Some("nope".into()),
                    ..Default::default()
                },
            ),
        ]);
        let tool = EmailHealthTool::new(ctx);
        let r = tool.run(json!({})).await;
        assert_eq!(r["ok"], true);
        let arr = r["instances"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        // Sorted by instance.
        assert_eq!(arr[0]["instance"], "a");
        assert_eq!(arr[0]["state"], "idle");
        assert_eq!(arr[1]["instance"], "b");
        assert_eq!(arr[1]["state"], "down");
        assert_eq!(arr[1]["last_error"], "nope");
    }

    #[tokio::test]
    async fn instance_filter_isolates_one() {
        let ctx = ctx_with_health_entries(vec![
            ("a", AccountHealth::default()),
            ("b", AccountHealth::default()),
        ]);
        let tool = EmailHealthTool::new(ctx);
        let r = tool.run(json!({ "instance": "b" })).await;
        let arr = r["instances"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["instance"], "b");
    }

    #[tokio::test]
    async fn empty_health_map_yields_empty_list() {
        let ctx = ctx_with_health_entries(vec![]);
        let tool = EmailHealthTool::new(ctx);
        let r = tool.run(json!({})).await;
        assert_eq!(r["ok"], true);
        assert!(r["instances"].as_array().unwrap().is_empty());
    }
}
