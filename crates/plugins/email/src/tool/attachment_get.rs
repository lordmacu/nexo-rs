//! `email_attachment_get` — fetch attachment bytes by sha256.
//!
//! Defends against arbitrary path read by gating on `AttachmentStore`
//! membership: the sha256 must already be recorded by the inbound
//! parser before bytes are returned. The on-disk path is always
//! `attachments_dir.join(sha256)` — no user-supplied path
//! component, no `../` traversal possible.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
use serde::Deserialize;
use serde_json::{json, Value};

use super::context::EmailToolContext;

/// Hard ceiling on bytes returned in one call. Lower than the
/// plugin-level `max_attachment_bytes` because the agent loop
/// has its own token budget — handing it a 25 MB blob doesn't
/// help a triage flow. Operators with larger needs should
/// stream out-of-band.
const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct GetArgs {
    sha256: String,
    /// `base64` (default) returns the bytes inline; `text` decodes
    /// as UTF-8 lossy — useful for plain-text logs / diffs that
    /// landed as attachments.
    #[serde(default)]
    encoding: Option<String>,
}

pub struct EmailAttachmentGetTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailAttachmentGetTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_attachment_get".into(),
            description:
                "Read attachment bytes by sha256. The hash must already exist in the dedup store \
                 (the parser records every attachment it persists) — arbitrary disk reads are \
                 refused. Default encoding is base64; pass `encoding: \"text\"` to get UTF-8 \
                 lossy. Hard 5 MiB cap on returned bytes — larger files must be streamed \
                 out-of-band."
                    .into(),
            parameters: json!({
                "type": "object",
                "required": ["sha256"],
                "properties": {
                    "sha256":   { "type": "string", "minLength": 32, "maxLength": 128 },
                    "encoding": { "type": "string", "enum": ["base64", "text"] }
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
        let store = self.ctx.attachment_store.as_ref().ok_or_else(|| {
            anyhow!("attachment store unavailable — set email.attachments_db in config")
        })?;
        // Defence-in-depth: reject anything that doesn't match the
        // hex shape of a sha256 hash even before we hit the store.
        if !is_hex_sha256(&args.sha256) {
            anyhow::bail!("sha256 must be 32–128 hex chars; got '{}'", args.sha256);
        }
        let last_seen = store
            .last_seen(&args.sha256)
            .await?
            .ok_or_else(|| anyhow!("sha256 '{}' not present in attachment store", args.sha256))?;

        let path = self.ctx.attachments_dir.join(&args.sha256);
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| anyhow!("read {}: {}", path.display(), e))?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            anyhow::bail!(
                "attachment is {} bytes (max {} returned in one call)",
                bytes.len(),
                MAX_RESPONSE_BYTES
            );
        }

        let encoding = args.encoding.as_deref().unwrap_or("base64");
        let body = match encoding {
            "base64" => json!({ "encoding": "base64", "bytes": B64.encode(&bytes) }),
            "text" => json!({
                "encoding": "text",
                "text": String::from_utf8_lossy(&bytes).into_owned(),
            }),
            other => anyhow::bail!("unsupported encoding '{other}' (use 'base64' or 'text')"),
        };

        Ok(json!({
            "ok": true,
            "sha256": args.sha256,
            "size_bytes": bytes.len(),
            "last_seen": last_seen,
            "body": body,
        }))
    }
}

#[async_trait]
impl ToolHandler for EmailAttachmentGetTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailAttachmentGetTool::new(ctx);
    registry.register(EmailAttachmentGetTool::tool_def(), tool);
}

fn is_hex_sha256(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachment_store::AttachmentStore;
    use crate::tool::dispatcher_stub::stub_ctx;
    use tempfile::tempdir;

    fn ctx_with_attachments(
        store: Arc<AttachmentStore>,
        dir: std::path::PathBuf,
    ) -> Arc<EmailToolContext> {
        let (ctx, _) = stub_ctx(vec!["ops".into()], false);
        let owned = (*ctx).clone_for_test_replace_attachments(Some(store), dir);
        Arc::new(owned)
    }

    impl EmailToolContext {
        fn clone_for_test_replace_attachments(
            &self,
            store: Option<Arc<AttachmentStore>>,
            dir: std::path::PathBuf,
        ) -> Self {
            EmailToolContext {
                creds: self.creds.clone(),
                google: self.google.clone(),
                config: self.config.clone(),
                dispatcher: self.dispatcher.clone(),
                health: self.health.clone(),
                bounce_store: self.bounce_store.clone(),
                attachment_store: store,
                attachments_dir: dir,
            }
        }
    }

    #[tokio::test]
    async fn refuses_when_sha256_absent() {
        let dir = tempdir().unwrap();
        let store = Arc::new(AttachmentStore::open("sqlite::memory:").await.unwrap());
        store.migrate().await.unwrap();
        let ctx = ctx_with_attachments(store, dir.path().to_path_buf());
        let tool = EmailAttachmentGetTool::new(ctx);
        let r = tool.run(json!({ "sha256": "deadbeef".repeat(8) })).await;
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap().contains("not present"));
    }

    #[tokio::test]
    async fn returns_bytes_when_sha256_present() {
        let dir = tempdir().unwrap();
        let store = Arc::new(AttachmentStore::open("sqlite::memory:").await.unwrap());
        store.migrate().await.unwrap();
        let sha = "a".repeat(64);
        store.record(&sha).await.unwrap();
        std::fs::write(dir.path().join(&sha), b"hello world").unwrap();
        let ctx = ctx_with_attachments(store, dir.path().to_path_buf());
        let tool = EmailAttachmentGetTool::new(ctx);
        let r = tool.run(json!({ "sha256": sha, "encoding": "text" })).await;
        assert_eq!(r["ok"], true);
        assert_eq!(r["body"]["text"], "hello world");
        assert_eq!(r["size_bytes"], 11);
    }

    #[tokio::test]
    async fn rejects_non_hex_sha() {
        let dir = tempdir().unwrap();
        let store = Arc::new(AttachmentStore::open("sqlite::memory:").await.unwrap());
        store.migrate().await.unwrap();
        let ctx = ctx_with_attachments(store, dir.path().to_path_buf());
        let tool = EmailAttachmentGetTool::new(ctx);
        let r = tool.run(json!({ "sha256": "../etc/passwd" })).await;
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap().contains("hex"));
    }

    #[tokio::test]
    async fn missing_attachment_store_yields_clean_error() {
        let (ctx, _) = stub_ctx(vec!["ops".into()], false);
        let tool = EmailAttachmentGetTool::new(ctx);
        let r = tool.run(json!({ "sha256": "a".repeat(64) })).await;
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap().contains("attachment store"));
    }
}
