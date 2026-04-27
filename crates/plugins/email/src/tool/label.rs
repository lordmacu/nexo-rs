//! `email_label` — Gmail-only `X-GM-LABELS` add/remove (Phase 48.7).
//! `STORE +X-GM-LABELS (...)` / `STORE -X-GM-LABELS (...)` is a Gmail
//! IMAP extension; non-Gmail accounts get a clean error rather than a
//! confusing IMAP NO downstream.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use nexo_config::types::plugins::EmailProvider;
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
use serde::Deserialize;
use serde_json::{json, Value};

use super::context::EmailToolContext;
use super::imap_op::run_imap_op;
use super::uid_set::format_uid_set;

#[derive(Debug, Deserialize)]
struct LabelArgs {
    instance: String,
    uids: Vec<u32>,
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

pub struct EmailLabelTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailLabelTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_label".into(),
            description:
                "Add and/or remove Gmail labels (X-GM-LABELS extension) on a UID set. Returns \
                 `{ updated }`. Errors with `requires gmail provider` for non-Gmail accounts — \
                 IMAP servers without X-GM-LABELS would return NO and confuse the agent."
                    .into(),
            parameters: json!({
                "type": "object",
                "required": ["instance", "uids"],
                "properties": {
                    "instance": { "type": "string" },
                    "uids":     { "type": "array", "items": { "type": "integer", "minimum": 1 } },
                    "add":      { "type": "array", "items": { "type": "string" } },
                    "remove":   { "type": "array", "items": { "type": "string" } }
                },
                "additionalProperties": false
            }),
        }
    }

    pub async fn run(&self, args: Value) -> Value {
        let parsed: LabelArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => return json!({ "ok": false, "error": format!("invalid arguments: {e}") }),
        };
        match self.run_inner(parsed).await {
            Ok(v) => v,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }
    }

    async fn run_inner(&self, args: LabelArgs) -> Result<Value> {
        let acct_cfg = self
            .ctx
            .account(&args.instance)
            .ok_or_else(|| anyhow!("unknown email instance: {}", args.instance))?
            .clone();
        if !matches!(acct_cfg.provider, EmailProvider::Gmail) {
            anyhow::bail!(
                "label tool requires gmail provider; got '{:?}'",
                acct_cfg.provider
            );
        }
        if args.uids.is_empty() || (args.add.is_empty() && args.remove.is_empty()) {
            return Ok(json!({ "ok": true, "updated": 0 }));
        }
        let inbox = acct_cfg.folders.inbox.clone();
        let creds = self.ctx.creds.clone();
        let google = self.ctx.google.clone();
        let uid_set = format_uid_set(&args.uids);
        let n = args.uids.len();
        let add_list = format_label_list(&args.add);
        let remove_list = format_label_list(&args.remove);

        let result = run_imap_op(&acct_cfg, &creds, google, &inbox, move |mut conn, _mb| {
            let uid_set = uid_set.clone();
            let add_list = add_list.clone();
            let remove_list = remove_list.clone();
            async move {
                if !add_list.is_empty() {
                    conn.uid_store(&uid_set, &format!("+X-GM-LABELS {add_list}"))
                        .await?;
                }
                if !remove_list.is_empty() {
                    conn.uid_store(&uid_set, &format!("-X-GM-LABELS {remove_list}"))
                        .await?;
                }
                Ok((conn, json!({ "ok": true, "updated": n })))
            }
        })
        .await?;
        Ok(result)
    }
}

#[async_trait]
impl ToolHandler for EmailLabelTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailLabelTool::new(ctx);
    registry.register(EmailLabelTool::tool_def(), tool);
}

/// Format `["A", "Bee"]` → `(A "Bee")`. Quotes only when the label
/// contains characters IMAP atoms forbid (whitespace, parens, etc.);
/// keeps simple labels unquoted for readable wire traffic.
pub fn format_label_list(labels: &[String]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = labels
        .iter()
        .map(|l| {
            let needs_quote = l.is_empty()
                || l.chars().any(|c| {
                    c.is_whitespace() || c == '(' || c == ')' || c == '"' || c == '\\'
                });
            if needs_quote {
                let escaped = l.replace('\\', "\\\\").replace('"', "\\\"");
                format!("\"{escaped}\"")
            } else {
                l.clone()
            }
        })
        .collect();
    format!("({})", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::dispatcher_stub::stub_ctx;
    use serde_json::json;

    #[tokio::test]
    async fn non_gmail_provider_rejected() {
        // stub_ctx defaults provider to `Custom` (no provider set).
        let (ctx, _) = stub_ctx(vec!["ops".into()], false);
        let tool = EmailLabelTool::new(ctx);
        let r = tool
            .run(json!({ "instance": "ops", "uids": [1], "add": ["Important"] }))
            .await;
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("gmail provider"));
    }

    #[tokio::test]
    async fn empty_add_remove_returns_zero() {
        // Even on Gmail, no labels to add/remove is a no-op.
        let (ctx, _) = stub_ctx(vec!["gmail-ops".into()], false);
        // Force provider to Gmail by editing the cloned config.
        let mut cfg = (*ctx.config).clone();
        cfg.accounts[0].provider = EmailProvider::Gmail;
        let new_ctx = Arc::new(EmailToolContext {
            creds: ctx.creds.clone(),
            google: ctx.google.clone(),
            config: Arc::new(cfg),
            dispatcher: ctx.dispatcher.clone(),
            health: ctx.health.clone(),
        });
        let tool = EmailLabelTool::new(new_ctx);
        let r = tool
            .run(json!({ "instance": "gmail-ops", "uids": [], "add": [] }))
            .await;
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["updated"], json!(0));
    }

    #[test]
    fn format_label_list_simple() {
        assert_eq!(
            format_label_list(&["Important".into()]),
            "(Important)"
        );
    }

    #[test]
    fn format_label_list_quotes_when_whitespace() {
        assert_eq!(
            format_label_list(&["My Label".into()]),
            r#"("My Label")"#
        );
    }

    #[test]
    fn format_label_list_escapes_quote_and_backslash() {
        assert_eq!(
            format_label_list(&[r#"a"b\c"#.into()]),
            r#"("a\"b\\c")"#
        );
    }
}
