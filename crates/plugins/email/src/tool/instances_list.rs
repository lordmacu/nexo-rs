//! `email_instances_list` — list configured email accounts.
//!
//! Meta-discovery for agents that need to enumerate the email
//! surface they have access to (multi-account workflows, dashboards,
//! "send via the right account" routers). Returns one row per
//! `accounts:` entry from `email.yaml`. Live state is reflected via
//! `is_live` (in dispatcher's instance set) so an account that
//! crashed during spawn is visible separately from one that hasn't
//! been declared.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
use serde_json::{json, Value};

use super::context::EmailToolContext;

pub struct EmailInstancesListTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailInstancesListTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_instances_list".into(),
            description:
                "List every email instance declared in `email.yaml` (instance, address, \
                 provider, IMAP host, SMTP host) plus a per-row `is_live` flag indicating \
                 whether the dispatcher currently owns it. Use this to enumerate accounts \
                 before calling instance-scoped tools."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }

    pub async fn run(&self, _args: Value) -> Value {
        match self.run_inner().await {
            Ok(v) => v,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }
    }

    async fn run_inner(&self) -> Result<Value> {
        let live: std::collections::HashSet<String> =
            self.ctx.dispatcher.instance_ids().into_iter().collect();
        let rows: Vec<Value> = self
            .ctx
            .config
            .accounts
            .iter()
            .map(|a| {
                json!({
                    "instance": a.instance,
                    "address":  a.address,
                    "provider": format!("{:?}", a.provider).to_ascii_lowercase(),
                    "imap_host": a.imap.host,
                    "imap_port": a.imap.port,
                    "smtp_host": a.smtp.host,
                    "smtp_port": a.smtp.port,
                    "is_live":  live.contains(&a.instance),
                })
            })
            .collect();
        Ok(json!({
            "ok": true,
            "instances": rows,
            "count": rows.len(),
        }))
    }
}

#[async_trait]
impl ToolHandler for EmailInstancesListTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailInstancesListTool::new(ctx);
    registry.register(EmailInstancesListTool::tool_def(), tool);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::dispatcher_stub::stub_ctx;

    #[tokio::test]
    async fn lists_every_declared_account() {
        let (ctx, _) = stub_ctx(vec!["ops".into(), "support".into()], false);
        let tool = EmailInstancesListTool::new(ctx);
        let r = tool.run(json!({})).await;
        assert_eq!(r["ok"], true);
        let rows = r["instances"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let names: Vec<&str> = rows.iter().map(|r| r["instance"].as_str().unwrap()).collect();
        assert!(names.contains(&"ops"));
        assert!(names.contains(&"support"));
        // Both should be marked live since the stub dispatcher
        // returns every declared id.
        for row in rows {
            assert_eq!(row["is_live"], true);
        }
    }

    #[tokio::test]
    async fn empty_when_no_accounts_declared() {
        let (ctx, _) = stub_ctx(vec![], false);
        let tool = EmailInstancesListTool::new(ctx);
        let r = tool.run(json!({})).await;
        assert_eq!(r["ok"], true);
        assert!(r["instances"].as_array().unwrap().is_empty());
    }
}
