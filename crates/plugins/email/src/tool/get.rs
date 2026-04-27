//! `email_get` — full-body fetch by IMAP UID.
//!
//! `email_search` returns a 200-char snippet per row; tools that
//! actually need to read the message (triage, classify, summarise,
//! follow-up bots) need the full body. This handler fetches the
//! parent's raw bytes via an ephemeral IMAP op, parses with
//! `parse_eml` against the plugin's configured `max_body_bytes`,
//! and returns the meta + attachment metadata. Attachment bytes
//! are NOT inlined — only sha256 + local_path so the agent can
//! follow up with whatever it needs (a future `email_attachment_get`
//! is tracked separately).

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::imap_conn::ImapConnection;
use crate::mime_parse::{parse_eml, ParseConfig};
use crate::threading::{resolve_thread_root, session_id_for_thread};

use super::context::EmailToolContext;
use super::imap_op::run_imap_op;

#[derive(Debug, Deserialize)]
struct GetArgs {
    instance: String,
    uid: u32,
    /// Optional folder. Defaults to the account's configured INBOX.
    #[serde(default)]
    folder: Option<String>,
    /// Whether to include the (potentially large) `body_html` field.
    /// Default true.
    #[serde(default = "default_true")]
    include_html: bool,
}

fn default_true() -> bool {
    true
}

pub struct EmailGetTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailGetTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_get".into(),
            description:
                "Fetch a single message by IMAP UID and return its parsed meta + full body. \
                 The text body honours the plugin's `max_body_bytes` (truncation flag echoed). \
                 Attachments are returned as metadata only (sha256, local_path, mime_type, \
                 size, filename) — bytes are not inlined. Pair with `email_search` (which \
                 only returns 200-char snippets) for the standard 'find then read' flow."
                    .into(),
            parameters: json!({
                "type": "object",
                "required": ["instance", "uid"],
                "properties": {
                    "instance":     { "type": "string" },
                    "uid":          { "type": "integer", "minimum": 1 },
                    "folder":       { "type": "string", "description": "Default = account.folders.inbox" },
                    "include_html": { "type": "boolean", "description": "Default true" }
                },
                "additionalProperties": false
            }),
        }
    }

    pub async fn run(&self, args: Value) -> Value {
        let parsed: GetArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => return json!({ "ok": false, "error": format!("invalid arguments: {e}") }),
        };
        match self.run_inner(parsed).await {
            Ok(v) => v,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }
    }

    async fn run_inner(&self, args: GetArgs) -> Result<Value> {
        let acct_cfg = self
            .ctx
            .account(&args.instance)
            .ok_or_else(|| anyhow!("unknown email instance: {}", args.instance))?
            .clone();
        let folder = args
            .folder
            .clone()
            .unwrap_or_else(|| acct_cfg.folders.inbox.clone());
        let creds = self.ctx.creds.clone();
        let google = self.ctx.google.clone();
        let uid = args.uid;

        let raw = run_imap_op(
            &acct_cfg,
            &creds,
            google,
            &folder,
            move |mut conn: ImapConnection, _mb| async move {
                let msg = conn.fetch_uid(uid).await?;
                Ok((conn, msg.raw_bytes))
            },
        )
        .await?;

        // Attachments side: persist into the configured attachments
        // dir so the parser dedupes against AttachmentStore (when
        // wired). The agent gets the metadata + on-disk path.
        let scratch = std::env::temp_dir().join("nexo-email-get-scratch");
        let _ = tokio::fs::create_dir_all(&scratch).await;
        let parse_cfg = ParseConfig {
            max_body_bytes: self.ctx.config.max_body_bytes,
            max_attachment_bytes: self.ctx.config.max_attachment_bytes,
            attachments_dir: scratch,
            fallback_internal_date: chrono::Utc::now().timestamp(),
        };
        let parsed = parse_eml(&raw, &parse_cfg).await?;

        let thread_root = resolve_thread_root(&parsed.meta, args.uid, &acct_cfg.address);
        let session_id = session_id_for_thread(&thread_root).to_string();

        // Build the meta payload — selectively drop body_html so
        // tokens aren't burned on the HTML twin when the caller
        // doesn't want it.
        let mut meta_value = serde_json::to_value(&parsed.meta)?;
        if !args.include_html {
            if let Some(map) = meta_value.as_object_mut() {
                map.remove("body_html");
            }
        }

        let attachments: Vec<Value> = parsed
            .attachments
            .iter()
            .map(|a| serde_json::to_value(a).unwrap_or(json!({})))
            .collect();

        Ok(json!({
            "ok": true,
            "uid": args.uid,
            "folder": folder,
            "meta": meta_value,
            "attachments": attachments,
            "thread_root": thread_root,
            "session_id": session_id,
        }))
    }
}

#[async_trait]
impl ToolHandler for EmailGetTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailGetTool::new(ctx);
    registry.register(EmailGetTool::tool_def(), tool);
}
