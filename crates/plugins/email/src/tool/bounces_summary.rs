//! `email_bounces_summary` — aggregate bounce report (Phase 48 use cases #A).
//!
//! Reads `BounceStore` and returns per-instance counts (permanent /
//! transient / unknown) plus the top-N chattiest recipients ordered
//! by repeat-bounce count desc. Used by admin agents to spot
//! permanent failures, dead lists, and recipients worth purging.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::dsn::BounceClassification;

use super::context::EmailToolContext;

const DEFAULT_TOP_N: usize = 10;
const MAX_TOP_N: usize = 200;

#[derive(Debug, Default, Deserialize)]
struct SummaryArgs {
    /// Optional instance filter. Omitted = walk every instance.
    #[serde(default)]
    instance: Option<String>,
    /// Per-instance top-N recipients to include. Default 10, max 200,
    /// `0` returns the aggregate without per-row detail.
    #[serde(default)]
    top_n: Option<usize>,
}

pub struct EmailBouncesSummaryTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailBouncesSummaryTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_bounces_summary".into(),
            description:
                "Aggregate bounce report. Returns one entry per instance with permanent / \
                 transient / unknown counts and the `top_n` chattiest recipients ordered by \
                 repeat-bounce count desc. Use to spot dead lists or recipients worth purging."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "instance": { "type": "string", "description": "Filter to one instance. Omit for all." },
                    "top_n":    { "type": "integer", "minimum": 0, "maximum": 200, "description": "Per-instance top recipients. Default 10." }
                },
                "additionalProperties": false
            }),
        }
    }

    pub async fn run(&self, args: Value) -> Value {
        let parsed: SummaryArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => return json!({ "ok": false, "error": format!("invalid arguments: {e}") }),
        };
        match self.run_inner(parsed).await {
            Ok(v) => v,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }
    }

    async fn run_inner(&self, args: SummaryArgs) -> Result<Value> {
        // Bounce-less plugins (couldn't open `bounces.db` at boot)
        // should respond cleanly rather than 500. Admin agents
        // can branch on `ok=false, reason=…` to fall back.
        let store = match self.ctx.bounce_store.as_ref() {
            Some(s) => s,
            None => {
                return Ok(json!({
                    "ok": false,
                    "error": "bounce store unavailable — set email.bounces_db in config",
                }));
            }
        };
        let top_n = args
            .top_n
            .unwrap_or(DEFAULT_TOP_N)
            .min(MAX_TOP_N);
        let summaries = store
            .summary(args.instance.as_deref(), top_n)
            .await?;

        let payload: Vec<Value> = summaries
            .iter()
            .map(|s| {
                json!({
                    "instance":   s.instance,
                    "total_rows": s.total_rows,
                    "permanent":  s.permanent,
                    "transient":  s.transient,
                    "unknown":    s.unknown,
                    "top_recipients": s.top_recipients.iter().map(|r| json!({
                        "recipient":      r.recipient,
                        "classification": classification_label(r.classification),
                        "status_code":    r.status_code,
                        "action":         r.action,
                        "last_seen":      r.last_seen,
                        "count":          r.count,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();

        Ok(json!({
            "ok": true,
            "instances": payload,
            "instance_count": summaries.len(),
        }))
    }
}

#[async_trait]
impl ToolHandler for EmailBouncesSummaryTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailBouncesSummaryTool::new(ctx);
    registry.register(EmailBouncesSummaryTool::tool_def(), tool);
}

fn classification_label(c: BounceClassification) -> &'static str {
    match c {
        BounceClassification::Permanent => "permanent",
        BounceClassification::Transient => "transient",
        BounceClassification::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bounce_store::BounceStore;
    use crate::dsn::{BounceClassification, BounceEvent};
    use crate::tool::dispatcher_stub::stub_ctx_with_bounce;

    fn ev(instance: &str, recipient: &str, c: BounceClassification) -> BounceEvent {
        BounceEvent {
            account_id: format!("{instance}@example.com"),
            instance: instance.into(),
            original_message_id: Some("<o@x>".into()),
            recipient: Some(recipient.into()),
            status_code: Some("5.1.1".into()),
            action: Some("failed".into()),
            reason: Some("user unknown".into()),
            classification: c,
        }
    }

    async fn ctx_with_bounces() -> Arc<EmailToolContext> {
        let store = BounceStore::open("sqlite::memory:").await.unwrap();
        store
            .record(&ev("ops", "alice@x", BounceClassification::Permanent))
            .await
            .unwrap();
        store
            .record(&ev("ops", "bob@x", BounceClassification::Transient))
            .await
            .unwrap();
        store
            .record(&ev("ops", "alice@x", BounceClassification::Permanent))
            .await
            .unwrap();
        let (ctx, _) = stub_ctx_with_bounce(
            vec!["ops".into()],
            false,
            Some(Arc::new(store)),
        );
        ctx
    }

    #[tokio::test]
    async fn returns_aggregate_with_top_recipients() {
        let ctx = ctx_with_bounces().await;
        let tool = EmailBouncesSummaryTool::new(ctx);
        let out = tool.run(json!({})).await;
        assert_eq!(out["ok"], true);
        let instances = out["instances"].as_array().unwrap();
        assert_eq!(instances.len(), 1);
        let ops = &instances[0];
        assert_eq!(ops["instance"], "ops");
        assert_eq!(ops["permanent"], 1);
        assert_eq!(ops["transient"], 1);
        let top = ops["top_recipients"].as_array().unwrap();
        // alice@x bounced twice → first by count desc.
        assert_eq!(top[0]["recipient"], "alice@x");
        assert_eq!(top[0]["count"], 2);
        assert_eq!(top[1]["recipient"], "bob@x");
    }

    #[tokio::test]
    async fn instance_filter_isolates_one_account() {
        let store = BounceStore::open("sqlite::memory:").await.unwrap();
        store
            .record(&ev("a", "x@y", BounceClassification::Permanent))
            .await
            .unwrap();
        store
            .record(&ev("b", "x@y", BounceClassification::Permanent))
            .await
            .unwrap();
        let (ctx, _) = stub_ctx_with_bounce(
            vec!["a".into(), "b".into()],
            false,
            Some(Arc::new(store)),
        );
        let tool = EmailBouncesSummaryTool::new(ctx);
        let out = tool.run(json!({ "instance": "a" })).await;
        let instances = out["instances"].as_array().unwrap();
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0]["instance"], "a");
    }

    #[tokio::test]
    async fn top_n_zero_skips_recipients() {
        let ctx = ctx_with_bounces().await;
        let tool = EmailBouncesSummaryTool::new(ctx);
        let out = tool.run(json!({ "top_n": 0 })).await;
        let top = out["instances"][0]["top_recipients"].as_array().unwrap();
        assert!(top.is_empty());
    }

    #[tokio::test]
    async fn missing_bounce_store_yields_clean_error() {
        let (ctx, _) = stub_ctx_with_bounce(vec!["ops".into()], false, None);
        let tool = EmailBouncesSummaryTool::new(ctx);
        let out = tool.run(json!({})).await;
        assert_eq!(out["ok"], false);
        assert!(out["error"].as_str().unwrap().contains("bounce store"));
    }

    #[tokio::test]
    async fn invalid_args_surface_error() {
        let ctx = ctx_with_bounces().await;
        let tool = EmailBouncesSummaryTool::new(ctx);
        // top_n must be an integer if present.
        let out = tool.run(json!({ "top_n": "not-a-number" })).await;
        assert_eq!(out["ok"], false);
    }
}
