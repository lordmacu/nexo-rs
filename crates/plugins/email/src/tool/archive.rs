//! `email_archive` — UID MOVE to the configured archive folder
//! (Phase 48.7). Falls back to COPY + STORE \Deleted + EXPUNGE on
//! servers without the MOVE extension (RFC 6851).

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
struct ArchiveArgs {
    instance: String,
    uids: Vec<u32>,
}

pub struct EmailArchiveTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailArchiveTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_archive".into(),
            description:
                "Move messages from INBOX to the configured archive folder. Uses IMAP UID MOVE \
                 (RFC 6851) when the server advertises MOVE; otherwise falls back to UID COPY + \
                 STORE \\Deleted + EXPUNGE. Returns `{ moved, fallback }`."
                    .into(),
            parameters: json!({
                "type": "object",
                "required": ["instance", "uids"],
                "properties": {
                    "instance": { "type": "string" },
                    "uids": { "type": "array", "items": { "type": "integer", "minimum": 1 } }
                },
                "additionalProperties": false
            }),
        }
    }

    pub async fn run(&self, args: Value) -> Value {
        let parsed: ArchiveArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => return json!({ "ok": false, "error": format!("invalid arguments: {e}") }),
        };
        match self.run_inner(parsed).await {
            Ok(v) => v,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }
    }

    async fn run_inner(&self, args: ArchiveArgs) -> Result<Value> {
        if args.uids.is_empty() {
            return Ok(json!({ "ok": true, "moved": 0, "fallback": false }));
        }
        let acct_cfg = self
            .ctx
            .account(&args.instance)
            .ok_or_else(|| anyhow!("unknown email instance: {}", args.instance))?
            .clone();
        let inbox = acct_cfg.folders.inbox.clone();
        let archive = acct_cfg.folders.archive.clone();
        let creds = self.ctx.creds.clone();
        let google = self.ctx.google.clone();
        let uid_set = format_uid_set(&args.uids);
        let n = args.uids.len();

        let result = run_imap_op(&acct_cfg, &creds, google, &inbox, move |mut conn, _mb| {
            let archive = archive.clone();
            let uid_set = uid_set.clone();
            async move {
                let supports_move = conn.capabilities.move_ext;
                if supports_move {
                    conn.uid_move(&uid_set, &archive).await?;
                    Ok((conn, json!({ "ok": true, "moved": n, "fallback": false })))
                } else {
                    conn.uid_copy(&uid_set, &archive).await?;
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
impl ToolHandler for EmailArchiveTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailArchiveTool::new(ctx);
    registry.register(EmailArchiveTool::tool_def(), tool);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::dispatcher_stub::stub_ctx;
    use serde_json::json;

    #[tokio::test]
    async fn empty_uids_returns_zero_moved() {
        let (ctx, _) = stub_ctx(vec!["ops".into()], false);
        let tool = EmailArchiveTool::new(ctx);
        let r = tool.run(json!({ "instance": "ops", "uids": [] })).await;
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["moved"], json!(0));
        assert_eq!(r["fallback"], json!(false));
    }

    #[tokio::test]
    async fn unknown_instance_errors() {
        let (ctx, _) = stub_ctx(vec!["ops".into()], false);
        let tool = EmailArchiveTool::new(ctx);
        let r = tool.run(json!({ "instance": "ghost", "uids": [1] })).await;
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"]
            .as_str()
            .unwrap()
            .contains("unknown email instance"));
    }

    #[tokio::test]
    async fn missing_uids_field_errors() {
        let (ctx, _) = stub_ctx(vec!["ops".into()], false);
        let tool = EmailArchiveTool::new(ctx);
        let r = tool.run(json!({ "instance": "ops" })).await;
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("invalid arguments"));
    }
}
