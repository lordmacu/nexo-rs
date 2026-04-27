#![allow(clippy::all)] // Phase 48 scaffolding — re-enable when email use-cases settle

//! `email_send` — outbound email via the dispatcher (Phase 48.7).
//!
//! Schema is path-by-reference for attachments (matches 48.5
//! `OutboundAttachmentRef` / 48.4 dispatcher). The `from` address is
//! pinned to `EmailAccountConfig.address` server-side; agents cannot
//! spoof it.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::events::{OutboundAttachmentRef, OutboundCommand};

use super::context::EmailToolContext;

#[derive(Debug, Deserialize)]
struct SendArgs {
    instance: String,
    to: Vec<String>,
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    bcc: Vec<String>,
    subject: String,
    body: String,
    #[serde(default)]
    attachments: Vec<OutboundAttachmentRef>,
    /// When `true`, refuse to enqueue if any recipient already has a
    /// permanent bounce on record. Default `false` keeps the legacy
    /// advisory-warning behaviour (recipient_warnings on the
    /// success envelope). Use this from automation that should
    /// halt rather than spend deliverability budget retrying a
    /// known-dead address.
    #[serde(default)]
    strict_bounce_check: bool,
}

pub struct EmailSendTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailSendTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_send".into(),
            description:
                "Send an email through the configured outbound dispatcher. The `from` address is \
                 fixed to the account's configured address (anti-spoof). Returns the \
                 generated `message_id` so the caller can correlate the eventual ack on \
                 `plugin.outbound.email.<instance>.ack`. Attachments are referenced by \
                 file path; the dispatcher reads them at enqueue time and fails fast on \
                 missing files."
                    .into(),
            parameters: json!({
                "type": "object",
                "required": ["instance", "to", "subject", "body"],
                "properties": {
                    "instance": { "type": "string", "description": "Email account instance id." },
                    "to":  { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                    "cc":  { "type": "array", "items": { "type": "string" } },
                    "bcc": { "type": "array", "items": { "type": "string" } },
                    "subject": { "type": "string" },
                    "body":    { "type": "string" },
                    "attachments": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["data_path", "filename"],
                            "properties": {
                                "data_path":  { "type": "string" },
                                "filename":   { "type": "string" },
                                "mime_type":  { "type": "string" },
                                "content_id": { "type": "string" },
                                "disposition": {
                                    "type": "string",
                                    "enum": ["inline", "attachment"]
                                }
                            }
                        }
                    },
                    "strict_bounce_check": {
                        "type": "boolean",
                        "description": "Refuse to enqueue when any recipient has a permanent bounce on record (default false)."
                    }
                },
                "additionalProperties": false
            }),
        }
    }
}

impl EmailSendTool {
    /// Pure-logic entry point. The trait `call` wraps it so unit tests
    /// can exercise schema + dispatcher routing without standing up
    /// an `AgentContext`.
    pub async fn run(&self, args: Value) -> Value {
        let parsed: SendArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => {
                return json!({
                    "ok": false,
                    "error": format!("invalid arguments: {e}"),
                });
            }
        };
        if self.ctx.account(&parsed.instance).is_none() {
            return json!({
                "ok": false,
                "error": format!("unknown email instance: {}", parsed.instance),
            });
        }
        // Phase 48 follow-up #4 — surface any prior bounce against
        // the recipients we're about to mail. Advisory only; the
        // operator may have fixed the destination since the
        // bounce. The agent decides what to do with the warning.
        let recipient_warnings = collect_recipient_warnings(
            &self.ctx,
            &parsed.instance,
            &parsed.to,
            &parsed.cc,
            &parsed.bcc,
        )
        .await;

        // strict_bounce_check — refuse to enqueue when any recipient
        // already has a permanent bounce on record. Distinct from
        // the advisory path above: that one annotates a successful
        // enqueue; this one short-circuits before we burn an
        // SMTP attempt.
        if parsed.strict_bounce_check {
            let permanent: Vec<&Value> = recipient_warnings
                .iter()
                .filter(|w| w.get("classification").and_then(|v| v.as_str()) == Some("permanent"))
                .collect();
            if !permanent.is_empty() {
                return json!({
                    "ok": false,
                    "error": "strict_bounce_check: at least one recipient has a permanent bounce on record",
                    "recipient_warnings": permanent,
                });
            }
        }

        let cmd = OutboundCommand {
            to: parsed.to,
            cc: parsed.cc,
            bcc: parsed.bcc,
            subject: parsed.subject,
            body: parsed.body,
            in_reply_to: None,
            references: vec![],
            attachments: parsed.attachments,
        };
        match self
            .ctx
            .dispatcher
            .enqueue_for_instance(&parsed.instance, cmd)
            .await
        {
            Ok(message_id) => {
                let mut out = json!({ "ok": true, "message_id": message_id });
                if !recipient_warnings.is_empty() {
                    out["recipient_warnings"] =
                        serde_json::to_value(&recipient_warnings).unwrap_or_default();
                }
                out
            }
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }
    }
}

/// Look up every recipient (to + cc + bcc) against the bounce
/// store and emit a one-line summary per match. Empty when the
/// store is unavailable or no recipient has bounced before.
async fn collect_recipient_warnings(
    ctx: &EmailToolContext,
    instance: &str,
    to: &[String],
    cc: &[String],
    bcc: &[String],
) -> Vec<Value> {
    let Some(store) = ctx.bounce_store.as_ref() else {
        return vec![];
    };
    let mut out = Vec::new();
    for r in to.iter().chain(cc.iter()).chain(bcc.iter()) {
        match store.get(instance, r).await {
            Ok(Some(status)) => {
                let label = match status.classification {
                    crate::dsn::BounceClassification::Permanent => "permanent",
                    crate::dsn::BounceClassification::Transient => "transient",
                    crate::dsn::BounceClassification::Unknown => "unknown",
                };
                out.push(json!({
                    "recipient": r,
                    "classification": label,
                    "status_code": status.status_code,
                    "last_seen": status.last_seen,
                    "count": status.count,
                }));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!(
                    target: "plugin.email",
                    recipient = %r,
                    error = %e,
                    "bounce_store lookup failed"
                );
            }
        }
    }
    out
}

#[async_trait]
impl ToolHandler for EmailSendTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailSendTool::new(ctx);
    registry.register(EmailSendTool::tool_def(), tool);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::OutboundCommand;
    use crate::inbound::HealthMap;
    use anyhow::Result;
    use async_trait::async_trait;
    use dashmap::DashMap;
    use nexo_auth::email::EmailCredentialStore;
    use nexo_auth::google::GoogleCredentialStore;
    use nexo_config::types::plugins::EmailPluginConfigFile;
    use std::sync::Mutex;

    /// Test stub of `DispatcherHandle` that records every call and
    /// returns a deterministic message_id.
    struct StubDispatcher {
        instance_ids_list: Vec<String>,
        captured: Mutex<Vec<(String, OutboundCommand)>>,
        next_id: Mutex<u32>,
        force_err: bool,
    }

    #[async_trait]
    impl crate::tool::DispatcherHandle for StubDispatcher {
        async fn enqueue_for_instance(
            &self,
            instance: &str,
            cmd: OutboundCommand,
        ) -> Result<String> {
            if self.force_err {
                anyhow::bail!("dispatcher rejected for test");
            }
            let mut n = self.next_id.lock().unwrap();
            *n += 1;
            let id = format!("<stub-{n}@test>");
            self.captured.lock().unwrap().push((instance.into(), cmd));
            Ok(id)
        }
        fn instance_ids(&self) -> Vec<String> {
            self.instance_ids_list.clone()
        }
    }

    fn build_ctx(
        declared: Vec<String>,
        force_err: bool,
    ) -> (Arc<EmailToolContext>, Arc<StubDispatcher>) {
        let yaml = format!(
            "email:\n  accounts:\n{}\n",
            declared
                .iter()
                .map(|i| format!(
                    "    - instance: {i}\n      address: {i}@example.com\n      imap: {{ host: imap.x, port: 993 }}\n      smtp: {{ host: smtp.x, port: 587 }}\n"
                ))
                .collect::<String>()
        );
        let f: EmailPluginConfigFile = serde_yaml::from_str(&yaml).unwrap();
        let dispatcher = Arc::new(StubDispatcher {
            instance_ids_list: declared.clone(),
            captured: Mutex::new(vec![]),
            next_id: Mutex::new(0),
            force_err,
        });
        let ctx = Arc::new(EmailToolContext {
            creds: Arc::new(EmailCredentialStore::empty()),
            google: Arc::new(GoogleCredentialStore::empty()),
            config: Arc::new(f.email),
            dispatcher: dispatcher.clone(),
            health: HealthMap::new(DashMap::new().into()),
            bounce_store: None,
            attachment_store: None,
            attachments_dir: std::path::PathBuf::new(),
        });
        (ctx, dispatcher)
    }

    #[tokio::test]
    async fn happy_send_returns_message_id() {
        let (ctx, disp) = build_ctx(vec!["ops".into()], false);
        let tool = EmailSendTool::new(ctx);
        let r = tool
            .run(json!({
                "instance": "ops",
                "to": ["alice@x"],
                "subject": "Hi",
                "body": "hello"
            }))
            .await;
        assert_eq!(r["ok"], json!(true));
        assert!(r["message_id"].as_str().unwrap().contains("stub-1"));
        let captured = disp.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, "ops");
        assert_eq!(captured[0].1.subject, "Hi");
    }

    #[tokio::test]
    async fn unknown_instance_errors() {
        let (ctx, _) = build_ctx(vec!["ops".into()], false);
        let tool = EmailSendTool::new(ctx);
        let r = tool
            .run(json!({
                "instance": "ghost",
                "to": ["alice@x"],
                "subject": "Hi",
                "body": "hello"
            }))
            .await;
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"]
            .as_str()
            .unwrap()
            .contains("unknown email instance"));
    }

    #[tokio::test]
    async fn missing_required_field_returns_error_envelope() {
        let (ctx, _) = build_ctx(vec!["ops".into()], false);
        let tool = EmailSendTool::new(ctx);
        let r = tool.run(json!({ "instance": "ops", "to": ["a@x"] })).await;
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("invalid arguments"));
    }

    #[tokio::test]
    async fn dispatcher_error_surfaces_in_envelope() {
        let (ctx, _) = build_ctx(vec!["ops".into()], true);
        let tool = EmailSendTool::new(ctx);
        let r = tool
            .run(json!({
                "instance": "ops",
                "to": ["a@x"],
                "subject": "Hi",
                "body": "ok"
            }))
            .await;
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("rejected"));
    }

    fn build_ctx_with_bounce(
        declared: Vec<String>,
        bounce_store: Arc<crate::bounce_store::BounceStore>,
    ) -> Arc<EmailToolContext> {
        let yaml = format!(
            "email:\n  accounts:\n{}\n",
            declared
                .iter()
                .map(|i| format!(
                    "    - instance: {i}\n      address: {i}@example.com\n      imap: {{ host: imap.x, port: 993 }}\n      smtp: {{ host: smtp.x, port: 587 }}\n"
                ))
                .collect::<String>()
        );
        let f: EmailPluginConfigFile = serde_yaml::from_str(&yaml).unwrap();
        let dispatcher = Arc::new(StubDispatcher {
            instance_ids_list: declared.clone(),
            captured: Mutex::new(vec![]),
            next_id: Mutex::new(0),
            force_err: false,
        });
        Arc::new(EmailToolContext {
            creds: Arc::new(EmailCredentialStore::empty()),
            google: Arc::new(GoogleCredentialStore::empty()),
            config: Arc::new(f.email),
            dispatcher,
            health: HealthMap::new(DashMap::new().into()),
            bounce_store: Some(bounce_store),
            attachment_store: None,
            attachments_dir: std::path::PathBuf::new(),
        })
    }

    #[tokio::test]
    async fn strict_bounce_check_refuses_when_recipient_is_permanently_bounced() {
        use crate::bounce_store::BounceStore;
        use crate::dsn::{BounceClassification, BounceEvent};
        let store = BounceStore::open("sqlite::memory:").await.unwrap();
        store
            .record(&BounceEvent {
                account_id: "ops@example.com".into(),
                instance: "ops".into(),
                original_message_id: Some("<o@x>".into()),
                recipient: Some("dead@x".into()),
                status_code: Some("5.1.1".into()),
                action: Some("failed".into()),
                reason: None,
                classification: BounceClassification::Permanent,
            })
            .await
            .unwrap();
        let ctx = build_ctx_with_bounce(vec!["ops".into()], Arc::new(store));
        let tool = EmailSendTool::new(ctx);
        let r = tool
            .run(json!({
                "instance": "ops",
                "to": ["dead@x"],
                "subject": "Hi",
                "body": "ok",
                "strict_bounce_check": true
            }))
            .await;
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap().contains("strict_bounce_check"));
        let warns = r["recipient_warnings"].as_array().unwrap();
        assert_eq!(warns.len(), 1);
        assert_eq!(warns[0]["recipient"], "dead@x");
        assert_eq!(warns[0]["classification"], "permanent");
    }

    #[tokio::test]
    async fn strict_bounce_check_allows_when_recipient_is_only_transient() {
        use crate::bounce_store::BounceStore;
        use crate::dsn::{BounceClassification, BounceEvent};
        let store = BounceStore::open("sqlite::memory:").await.unwrap();
        store
            .record(&BounceEvent {
                account_id: "ops@example.com".into(),
                instance: "ops".into(),
                original_message_id: Some("<o@x>".into()),
                recipient: Some("flaky@x".into()),
                status_code: Some("4.7.1".into()),
                action: Some("delayed".into()),
                reason: None,
                classification: BounceClassification::Transient,
            })
            .await
            .unwrap();
        let ctx = build_ctx_with_bounce(vec!["ops".into()], Arc::new(store));
        let tool = EmailSendTool::new(ctx);
        let r = tool
            .run(json!({
                "instance": "ops",
                "to": ["flaky@x"],
                "subject": "Hi",
                "body": "ok",
                "strict_bounce_check": true
            }))
            .await;
        // Transient bounces don't block — the agent gets the
        // message_id + an advisory warning.
        assert_eq!(r["ok"], true);
        assert!(r["recipient_warnings"].as_array().is_some());
    }

    #[tokio::test]
    async fn default_send_does_not_strict_check() {
        use crate::bounce_store::BounceStore;
        use crate::dsn::{BounceClassification, BounceEvent};
        let store = BounceStore::open("sqlite::memory:").await.unwrap();
        store
            .record(&BounceEvent {
                account_id: "ops@example.com".into(),
                instance: "ops".into(),
                original_message_id: Some("<o@x>".into()),
                recipient: Some("dead@x".into()),
                status_code: Some("5.1.1".into()),
                action: Some("failed".into()),
                reason: None,
                classification: BounceClassification::Permanent,
            })
            .await
            .unwrap();
        let ctx = build_ctx_with_bounce(vec!["ops".into()], Arc::new(store));
        let tool = EmailSendTool::new(ctx);
        let r = tool
            .run(json!({
                "instance": "ops",
                "to": ["dead@x"],
                "subject": "Hi",
                "body": "ok"
            }))
            .await;
        // Default behaviour: still enqueues, advisory warning surfaced.
        assert_eq!(r["ok"], true);
        assert!(r["recipient_warnings"].as_array().is_some());
    }
}
