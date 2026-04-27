//! `email_reply` — fetch parent, inherit threading, send (Phase 48.7).

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::events::{EmailMeta, OutboundAttachmentRef, OutboundCommand};
use crate::imap_conn::ImapConnection;
use crate::mime_parse::{parse_eml, ParseConfig};
use crate::threading::enrich_reply_threading;

use super::context::EmailToolContext;
use super::imap_op::run_imap_op;

#[derive(Debug, Deserialize)]
struct ReplyArgs {
    instance: String,
    parent_uid: u32,
    body: String,
    #[serde(default)]
    reply_all: bool,
    #[serde(default)]
    attachments: Vec<OutboundAttachmentRef>,
}

pub struct EmailReplyTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailReplyTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_reply".into(),
            description:
                "Reply to an email by IMAP UID. Fetches the parent, derives recipients (To = \
                 parent.From; reply_all=true also includes parent.To/Cc minus the account's own \
                 address), inherits the In-Reply-To / References chain (RFC 5322 §3.6.4 \
                 truncation to 10 ids), and enqueues via the outbound dispatcher. Returns the \
                 generated message_id along with the resolved To/Cc lists so the caller can \
                 audit who got the reply."
                    .into(),
            parameters: json!({
                "type": "object",
                "required": ["instance", "parent_uid", "body"],
                "properties": {
                    "instance":   { "type": "string" },
                    "parent_uid": { "type": "integer", "minimum": 1 },
                    "body":       { "type": "string" },
                    "reply_all":  { "type": "boolean" },
                    "attachments": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["data_path", "filename"],
                            "properties": {
                                "data_path":   { "type": "string" },
                                "filename":    { "type": "string" },
                                "mime_type":   { "type": "string" },
                                "content_id":  { "type": "string" },
                                "disposition": { "type": "string", "enum": ["inline", "attachment"] }
                            }
                        }
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    pub async fn run(&self, args: Value) -> Value {
        let parsed: ReplyArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => return json!({ "ok": false, "error": format!("invalid arguments: {e}") }),
        };
        match self.run_inner(parsed).await {
            Ok(v) => v,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }
    }

    async fn run_inner(&self, args: ReplyArgs) -> Result<Value> {
        let acct_cfg = self
            .ctx
            .account(&args.instance)
            .ok_or_else(|| anyhow!("unknown email instance: {}", args.instance))?
            .clone();

        // Fetch the parent's raw bytes via an ephemeral IMAP op.
        let inbox = acct_cfg.folders.inbox.clone();
        let creds = self.ctx.creds.clone();
        let google = self.ctx.google.clone();
        let parent_uid = args.parent_uid;
        let raw = run_imap_op(
            &acct_cfg,
            &creds,
            google,
            &inbox,
            move |mut conn: ImapConnection, _mb| async move {
                let msg = conn.fetch_uid(parent_uid).await?;
                Ok((conn, msg.raw_bytes))
            },
        )
        .await?;

        // Parse the parent into our `EmailMeta`. Attachment dir not
        // needed for reply context — point at a tempdir to keep the
        // helper happy; we discard the attachments side of the parse.
        let scratch = std::env::temp_dir().join("nexo-email-reply-scratch");
        let _ = tokio::fs::create_dir_all(&scratch).await;
        let parse_cfg = ParseConfig {
            max_body_bytes: self.ctx.config.max_body_bytes,
            max_attachment_bytes: 0,
            attachments_dir: scratch,
            fallback_internal_date: chrono::Utc::now().timestamp(),
        };
        let parsed = parse_eml(&raw, &parse_cfg).await?;
        let parent_meta = parsed.meta;

        let (to, cc) = derive_reply_recipients(&parent_meta, args.reply_all, &acct_cfg.address);
        // Audit follow-up — refuse to send a reply with an empty
        // `To:` list (RFC 5322 requires at least one recipient).
        // The most common cause is a parent message with no
        // resolvable `From:` (e.g. `Undisclosed Recipients:;` or
        // a malformed header that mail-parser couldn't decode).
        // Bubble a clean error rather than enqueue a malformed
        // outbound that the SMTP server will reject downstream.
        if to.is_empty() {
            anyhow::bail!(
                "cannot derive reply recipients: parent UID {} has no usable `From:` header (got '{}'). \
                 Inspect the parent via `email_search` and use `email_send` with explicit `to:` if you need to respond.",
                args.parent_uid,
                parent_meta.from.address
            );
        }
        let mut cmd = OutboundCommand {
            to: to.clone(),
            cc: cc.clone(),
            bcc: vec![],
            subject: prefix_re(&parent_meta.subject),
            body: args.body,
            in_reply_to: None,
            references: vec![],
            attachments: args.attachments,
        };
        enrich_reply_threading(&parent_meta, &mut cmd);

        let message_id = self
            .ctx
            .dispatcher
            .enqueue_for_instance(&args.instance, cmd)
            .await?;

        Ok(json!({
            "ok": true,
            "message_id": message_id,
            "to": to,
            "cc": cc,
        }))
    }
}

#[async_trait]
impl ToolHandler for EmailReplyTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailReplyTool::new(ctx);
    registry.register(EmailReplyTool::tool_def(), tool);
}

/// Pure helper exposed for unit testing. Computes To/Cc for the
/// reply: To is always parent.from. Cc is empty unless reply_all=true,
/// in which case parent.to + parent.cc minus the account's own
/// address (case-insensitive, deduped).
pub fn derive_reply_recipients(
    parent: &EmailMeta,
    reply_all: bool,
    own_address: &str,
) -> (Vec<String>, Vec<String>) {
    let to = if parent.from.address.is_empty() {
        vec![]
    } else {
        vec![parent.from.address.clone()]
    };
    let cc = if reply_all {
        let mut all: Vec<String> = parent
            .to
            .iter()
            .chain(parent.cc.iter())
            .map(|a| a.address.clone())
            .filter(|s| !s.is_empty())
            .filter(|s| !s.eq_ignore_ascii_case(own_address))
            .filter(|s| !s.eq_ignore_ascii_case(&parent.from.address))
            .collect();
        // Dedupe case-insensitively, preserving first-seen order.
        let mut seen: Vec<String> = Vec::with_capacity(all.len());
        all.retain(|s| {
            if seen.iter().any(|p| p.eq_ignore_ascii_case(s)) {
                false
            } else {
                seen.push(s.clone());
                true
            }
        });
        all
    } else {
        vec![]
    };
    (to, cc)
}

/// Add `Re: ` prefix when the parent subject doesn't already have
/// one (case-insensitive). Avoids stacking `Re: Re: Re:`.
fn prefix_re(subject: &str) -> String {
    let trimmed = subject.trim_start();
    if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("re:") {
        subject.to_string()
    } else {
        format!("Re: {subject}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AddressEntry;
    use std::collections::BTreeMap;

    fn meta(from: &str, to: &[&str], cc: &[&str]) -> EmailMeta {
        EmailMeta {
            message_id: Some("<m@x>".into()),
            in_reply_to: None,
            references: vec![],
            from: AddressEntry {
                address: from.into(),
                name: None,
            },
            to: to
                .iter()
                .map(|s| AddressEntry {
                    address: (*s).into(),
                    name: None,
                })
                .collect(),
            cc: cc
                .iter()
                .map(|s| AddressEntry {
                    address: (*s).into(),
                    name: None,
                })
                .collect(),
            subject: "Hi".into(),
            body_text: String::new(),
            body_html: None,
            date: 0,
            headers_extra: BTreeMap::new(),
            body_truncated: false,
        }
    }

    #[test]
    fn single_recipient_when_not_reply_all() {
        let m = meta("alice@x", &["bob@x", "ops@x"], &["chris@x"]);
        let (to, cc) = derive_reply_recipients(&m, false, "ops@x");
        assert_eq!(to, vec!["alice@x".to_string()]);
        assert!(cc.is_empty());
    }

    #[test]
    fn reply_all_includes_to_cc_minus_own() {
        let m = meta("alice@x", &["bob@x", "OPS@X"], &["chris@x"]);
        let (to, cc) = derive_reply_recipients(&m, true, "ops@x");
        assert_eq!(to, vec!["alice@x".to_string()]);
        assert_eq!(cc, vec!["bob@x".to_string(), "chris@x".to_string()]);
    }

    #[test]
    fn reply_all_dedupes_case_insensitively() {
        let m = meta("alice@x", &["bob@x", "BOB@X"], &["chris@x", "Chris@X"]);
        let (_to, cc) = derive_reply_recipients(&m, true, "ops@x");
        assert_eq!(cc, vec!["bob@x".to_string(), "chris@x".to_string()]);
    }

    #[test]
    fn reply_all_excludes_parent_from() {
        let m = meta("alice@x", &["alice@x", "bob@x"], &[]);
        let (_, cc) = derive_reply_recipients(&m, true, "ops@x");
        assert_eq!(cc, vec!["bob@x".to_string()]);
    }

    #[test]
    fn empty_from_yields_empty_to() {
        let m = meta("", &["bob@x"], &[]);
        let (to, _) = derive_reply_recipients(&m, false, "ops@x");
        assert!(to.is_empty());
    }

    #[test]
    fn re_prefix_idempotent() {
        assert_eq!(prefix_re("hi"), "Re: hi");
        assert_eq!(prefix_re("Re: hi"), "Re: hi");
        assert_eq!(prefix_re("re: hi"), "re: hi");
        assert_eq!(prefix_re("RE: hi"), "RE: hi");
    }
}
