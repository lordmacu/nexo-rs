//! `email_thread` — return every message in the same thread as a
//! seed UID.
//!
//! Resolves the seed's thread root via `resolve_thread_root` (RFC
//! 5322 §3.6.4 priority) then runs `UID SEARCH OR HEADER MESSAGE-ID
//! "<root>" HEADER REFERENCES "<root>"` to gather every message
//! that points to the root either as itself or via the
//! `References:` chain. Rows come back sorted by `internal_date`
//! asc so follow-up automation reads the conversation in temporal
//! order.

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
use super::imap_op::{imap_quote, run_imap_op};
use super::uid_set::format_uid_set;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

#[derive(Debug, Deserialize)]
struct ThreadArgs {
    instance: String,
    seed_uid: u32,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

pub struct EmailThreadTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailThreadTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_thread".into(),
            description:
                "Return every message in the same RFC 5322 thread as `seed_uid`. Resolves the \
                 seed's root via Message-ID / In-Reply-To / References, then searches the \
                 folder for messages whose Message-ID equals the root OR whose References \
                 chain includes it. Rows come back sorted oldest-first so a follow-up agent \
                 reads the conversation in order. Same row shape as `email_search` plus the \
                 resolved `thread_root` and `session_id`."
                    .into(),
            parameters: json!({
                "type": "object",
                "required": ["instance", "seed_uid"],
                "properties": {
                    "instance":  { "type": "string" },
                    "seed_uid":  { "type": "integer", "minimum": 1 },
                    "folder":    { "type": "string", "description": "Default = account.folders.inbox" },
                    "limit":     { "type": "integer", "minimum": 1, "maximum": 200 }
                },
                "additionalProperties": false
            }),
        }
    }

    pub async fn run(&self, args: Value) -> Value {
        let parsed: ThreadArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => return json!({ "ok": false, "error": format!("invalid arguments: {e}") }),
        };
        match self.run_inner(parsed).await {
            Ok(v) => v,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }
    }

    async fn run_inner(&self, args: ThreadArgs) -> Result<Value> {
        let acct_cfg = self
            .ctx
            .account(&args.instance)
            .ok_or_else(|| anyhow!("unknown email instance: {}", args.instance))?
            .clone();
        let folder = args
            .folder
            .clone()
            .unwrap_or_else(|| acct_cfg.folders.inbox.clone());
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let creds = self.ctx.creds.clone();
        let google = self.ctx.google.clone();
        let seed_uid = args.seed_uid;

        // First pass — fetch the seed's raw bytes so we can parse
        // Message-ID / In-Reply-To / References and derive the
        // thread root. We do this in the same IMAP session as the
        // search to avoid two TLS handshakes.
        let acct_address = acct_cfg.address.clone();
        let max_body_bytes = self.ctx.config.max_body_bytes;
        let max_attachment_bytes = self.ctx.config.max_attachment_bytes;

        let (thread_root, rows) = run_imap_op(
            &acct_cfg,
            &creds,
            google,
            &folder,
            move |mut conn: ImapConnection, _mb| {
                let acct_address = acct_address.clone();
                async move {
                    let seed = conn.fetch_uid(seed_uid).await?;
                    let scratch = std::env::temp_dir().join("nexo-email-thread-scratch");
                    let _ = tokio::fs::create_dir_all(&scratch).await;
                    let parse_cfg = ParseConfig {
                        max_body_bytes,
                        max_attachment_bytes,
                        attachments_dir: scratch,
                        fallback_internal_date: chrono::Utc::now().timestamp(),
                    };
                    let parsed = parse_eml(&seed.raw_bytes, &parse_cfg).await?;
                    let thread_root = resolve_thread_root(&parsed.meta, seed_uid, &acct_address);

                    // The thread_root we got back is wrapped in `<...>`;
                    // strip them for the IMAP HEADER atom (the IMAP
                    // server matches on the field value, not the
                    // angle-bracketed form).
                    let inner = thread_root
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_string();
                    let q = imap_quote(&inner);
                    let atoms = format!("OR HEADER MESSAGE-ID {q} HEADER REFERENCES {q}");
                    let mut uids = conn.uid_search(&atoms).await?;
                    uids.sort_unstable();
                    uids.truncate(limit);
                    let rows = if uids.is_empty() {
                        Vec::new()
                    } else {
                        let uid_set = format_uid_set(&uids);
                        conn.fetch_search_rows(&uid_set).await?
                    };
                    Ok((conn, (thread_root, rows)))
                }
            },
        )
        .await?;

        let mut sorted = rows;
        sorted.sort_by_key(|r| r.date);

        let session_id = session_id_for_thread(&thread_root).to_string();

        Ok(json!({
            "ok": true,
            "thread_root": thread_root,
            "session_id": session_id,
            "folder": folder,
            "rows": sorted,
            "count": sorted.len(),
        }))
    }
}

#[async_trait]
impl ToolHandler for EmailThreadTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailThreadTool::new(ctx);
    registry.register(EmailThreadTool::tool_def(), tool);
}
