//! `email_move_to` — UID MOVE to an arbitrary folder (Phase 48.7).

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
use serde::Deserialize;
use serde_json::{json, Value};

use super::context::EmailToolContext;
use super::imap_op::run_imap_op;
use super::uid_set::format_uid_set;

#[derive(Debug, Deserialize)]
struct MoveArgs {
    instance: String,
    uids: Vec<u32>,
    folder: String,
}

pub struct EmailMoveToTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailMoveToTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_move_to".into(),
            description:
                "Move messages from INBOX to an arbitrary folder. UID MOVE primary, COPY + STORE \
                 \\Deleted + EXPUNGE fallback. Tool does not auto-create the folder; missing \
                 folder surfaces as an IMAP NO error in the result envelope."
                    .into(),
            parameters: json!({
                "type": "object",
                "required": ["instance", "uids", "folder"],
                "properties": {
                    "instance": { "type": "string" },
                    "uids":     { "type": "array", "items": { "type": "integer", "minimum": 1 } },
                    "folder":   { "type": "string", "minLength": 1 }
                },
                "additionalProperties": false
            }),
        }
    }

    pub async fn run(&self, args: Value) -> Value {
        let parsed: MoveArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => return json!({ "ok": false, "error": format!("invalid arguments: {e}") }),
        };
        match self.run_inner(parsed).await {
            Ok(v) => v,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }
    }

    async fn run_inner(&self, args: MoveArgs) -> Result<Value> {
        if args.uids.is_empty() {
            return Ok(json!({ "ok": true, "moved": 0, "fallback": false }));
        }
        let acct_cfg = self
            .ctx
            .account(&args.instance)
            .ok_or_else(|| anyhow!("unknown email instance: {}", args.instance))?
            .clone();
        let inbox = acct_cfg.folders.inbox.clone();
        let creds = self.ctx.creds.clone();
        let google = self.ctx.google.clone();
        let uid_set = format_uid_set(&args.uids);
        let folder = args.folder.clone();
        let n = args.uids.len();

        let result = run_imap_op(&acct_cfg, &creds, google, &inbox, move |mut conn, _mb| {
            let folder = folder.clone();
            let uid_set = uid_set.clone();
            async move {
                if conn.capabilities.move_ext {
                    conn.uid_move(&uid_set, &folder).await?;
                    Ok((conn, json!({ "ok": true, "moved": n, "fallback": false })))
                } else {
                    conn.uid_copy(&uid_set, &folder).await?;
                    conn.uid_store(&uid_set, "+FLAGS (\\Deleted)").await?;
                    conn.expunge().await?;
                    Ok((conn, json!({ "ok": true, "moved": n, "fallback": true })))
                }
            }
        })
        .await?;
        Ok(result)
    }
}

#[async_trait]
impl ToolHandler for EmailMoveToTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailMoveToTool::new(ctx);
    registry.register(EmailMoveToTool::tool_def(), tool);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::dispatcher_stub::stub_ctx;
    use serde_json::json;

    #[tokio::test]
    async fn empty_uids_returns_zero() {
        let (ctx, _) = stub_ctx(vec!["ops".into()], false);
        let tool = EmailMoveToTool::new(ctx);
        let r = tool
            .run(json!({ "instance": "ops", "uids": [], "folder": "X" }))
            .await;
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["moved"], json!(0));
    }

    #[tokio::test]
    async fn missing_folder_errors() {
        let (ctx, _) = stub_ctx(vec!["ops".into()], false);
        let tool = EmailMoveToTool::new(ctx);
        let r = tool
            .run(json!({ "instance": "ops", "uids": [1] }))
            .await;
        assert_eq!(r["ok"], json!(false));
    }
}
