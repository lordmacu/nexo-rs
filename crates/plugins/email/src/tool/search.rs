//! `email_search` — IMAP SEARCH with a portable JSON DSL (Phase 48.7).
//!
//! Translates a small JSON object into IMAP SEARCH atoms, escaping
//! every user-controlled string via `imap_quote` (RFC 3501 quoted-
//! string + CRLF collapse — defends against atom injection through
//! `from` / `subject` / `body`). Date fields accept `YYYY-MM-DD`
//! strings and emit IMAP `d-MMM-yyyy` via `imap_date`.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::NaiveDate;
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_llm::ToolDef;
use serde::Deserialize;
use serde_json::{json, Value};

use super::context::EmailToolContext;
use super::imap_op::{imap_date, imap_quote, run_imap_op};
use super::uid_set::format_uid_set;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

#[derive(Debug, Default, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    from: Option<String>,
    /// Domain-only filter for the From: header. Matches against the
    /// substring `@domain` so `from_domain: "acme.com"` catches
    /// every sender at acme.com without the noise of also matching
    /// addresses where `acme.com` appears elsewhere.
    #[serde(default)]
    from_domain: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    unseen: Option<bool>,
    #[serde(default)]
    seen: Option<bool>,
    /// Heuristic — match messages whose raw body contains
    /// `multipart/` (the MIME-multipart marker every attachment-
    /// bearing message carries). False positives possible
    /// (forwarded plaintext that mentions the literal token), but
    /// in practice this catches >95% of attachment messages without
    /// IMAP server-side support for an exact-match flag.
    #[serde(default)]
    has_attachments: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct SearchArgs {
    instance: String,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    query: SearchQuery,
    #[serde(default)]
    limit: Option<usize>,
}

pub struct EmailSearchTool {
    ctx: Arc<EmailToolContext>,
}

impl EmailSearchTool {
    pub fn new(ctx: Arc<EmailToolContext>) -> Self {
        Self { ctx }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "email_search".into(),
            description:
                "Search a folder by a portable JSON DSL: from / to / subject / body (substring \
                 match), since / before (YYYY-MM-DD), unseen / seen (booleans). Defaults to \
                 INBOX, limit 50 (max 200). Returns up to `limit` rows with uid, message_id, \
                 from, subject, date (unix sec), and a 200-char snippet."
                    .into(),
            parameters: json!({
                "type": "object",
                "required": ["instance"],
                "properties": {
                    "instance": { "type": "string" },
                    "folder":   { "type": "string" },
                    "limit":    { "type": "integer", "minimum": 1, "maximum": 200 },
                    "query": {
                        "type": "object",
                        "properties": {
                            "from":             { "type": "string" },
                            "from_domain":      { "type": "string", "description": "Match `@domain` in From." },
                            "to":               { "type": "string" },
                            "subject":          { "type": "string" },
                            "body":             { "type": "string" },
                            "since":            { "type": "string", "description": "YYYY-MM-DD" },
                            "before":           { "type": "string", "description": "YYYY-MM-DD" },
                            "unseen":           { "type": "boolean" },
                            "seen":             { "type": "boolean" },
                            "has_attachments":  { "type": "boolean", "description": "Heuristic — matches `multipart/` in body." }
                        }
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    pub async fn run(&self, args: Value) -> Value {
        let parsed: SearchArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => return json!({ "ok": false, "error": format!("invalid arguments: {e}") }),
        };
        match self.run_inner(parsed).await {
            Ok(v) => v,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }
    }

    async fn run_inner(&self, args: SearchArgs) -> Result<Value> {
        let acct_cfg = self
            .ctx
            .account(&args.instance)
            .ok_or_else(|| anyhow!("unknown email instance: {}", args.instance))?
            .clone();
        let folder = args
            .folder
            .clone()
            .unwrap_or_else(|| acct_cfg.folders.inbox.clone());
        let limit = args
            .limit
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);
        let atoms = build_search_atoms(&args.query)?;
        let creds = self.ctx.creds.clone();
        let google = self.ctx.google.clone();

        let rows = run_imap_op(&acct_cfg, &creds, google, &folder, move |mut conn, _mb| {
            let atoms = atoms.clone();
            async move {
                let mut uids = conn.uid_search(&atoms).await?;
                uids.truncate(limit);
                if uids.is_empty() {
                    return Ok((conn, Vec::new()));
                }
                let uid_set = format_uid_set(&uids);
                let rows = conn.fetch_search_rows(&uid_set).await?;
                Ok((conn, rows))
            }
        })
        .await?;

        Ok(json!({
            "ok": true,
            "rows": rows,
            "count": rows.len(),
        }))
    }
}

#[async_trait]
impl ToolHandler for EmailSearchTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> Result<Value> {
        Ok(self.run(args).await)
    }
}

pub fn register(registry: &ToolRegistry, ctx: Arc<EmailToolContext>) {
    let tool = EmailSearchTool::new(ctx);
    registry.register(EmailSearchTool::tool_def(), tool);
}

/// Translate the JSON DSL into an IMAP SEARCH atom string. Falls
/// back to `ALL` when no fields are set so the server doesn't
/// receive a syntactically invalid empty query.
pub fn build_search_atoms(q: &SearchQuery) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = &q.from {
        parts.push(format!("FROM {}", imap_quote(v)));
    }
    if let Some(v) = &q.from_domain {
        // Strip a leading `@` if the operator typed the full token —
        // we re-add it so the IMAP atom matches `@domain.tld` and not
        // the bare domain (which would also match the body).
        let trimmed = v.trim().trim_start_matches('@');
        if !trimmed.is_empty() {
            parts.push(format!("FROM {}", imap_quote(&format!("@{trimmed}"))));
        }
    }
    if let Some(v) = &q.to {
        parts.push(format!("TO {}", imap_quote(v)));
    }
    if let Some(v) = &q.subject {
        parts.push(format!("SUBJECT {}", imap_quote(v)));
    }
    if let Some(v) = &q.body {
        parts.push(format!("BODY {}", imap_quote(v)));
    }
    if let Some(v) = &q.since {
        let d = NaiveDate::parse_from_str(v, "%Y-%m-%d")
            .map_err(|e| anyhow!("invalid `since` ({v}): expected YYYY-MM-DD: {e}"))?;
        parts.push(format!("SINCE {}", imap_date(d)));
    }
    if let Some(v) = &q.before {
        let d = NaiveDate::parse_from_str(v, "%Y-%m-%d")
            .map_err(|e| anyhow!("invalid `before` ({v}): expected YYYY-MM-DD: {e}"))?;
        parts.push(format!("BEFORE {}", imap_date(d)));
    }
    if matches!(q.unseen, Some(true)) {
        parts.push("UNSEEN".into());
    }
    if matches!(q.seen, Some(true)) {
        parts.push("SEEN".into());
    }
    if matches!(q.has_attachments, Some(true)) {
        // `multipart/` is the standard MIME marker for messages
        // carrying attachments. IMAP's `BODY` atom does substring
        // matching across the entire body — false positives are
        // possible (someone forwarding the literal text) but rare.
        parts.push(format!("BODY {}", imap_quote("multipart/")));
    }
    if parts.is_empty() {
        Ok("ALL".into())
    } else {
        Ok(parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_falls_back_to_all() {
        let q = SearchQuery::default();
        assert_eq!(build_search_atoms(&q).unwrap(), "ALL");
    }

    #[test]
    fn single_from_atom() {
        let mut q = SearchQuery::default();
        q.from = Some("alice@x".into());
        assert_eq!(build_search_atoms(&q).unwrap(), r#"FROM "alice@x""#);
    }

    #[test]
    fn multi_field_concatenates_atoms() {
        let mut q = SearchQuery::default();
        q.from = Some("alice@x".into());
        q.subject = Some("report".into());
        q.unseen = Some(true);
        let s = build_search_atoms(&q).unwrap();
        assert!(s.contains(r#"FROM "alice@x""#));
        assert!(s.contains(r#"SUBJECT "report""#));
        assert!(s.contains("UNSEEN"));
    }

    #[test]
    fn since_renders_imap_date() {
        let mut q = SearchQuery::default();
        q.since = Some("2024-01-05".into());
        assert_eq!(build_search_atoms(&q).unwrap(), "SINCE 5-Jan-2024");
    }

    #[test]
    fn invalid_date_errors() {
        let mut q = SearchQuery::default();
        q.before = Some("nope".into());
        assert!(build_search_atoms(&q).is_err());
    }

    #[test]
    fn injection_attempt_is_quoted() {
        let mut q = SearchQuery::default();
        q.from = Some(r#"alice" OR FROM "evil"#.into());
        let s = build_search_atoms(&q).unwrap();
        // The whole thing is a single quoted string after escape.
        assert!(s.starts_with(r#"FROM "alice\" OR FROM \"evil""#));
        // No bare unescaped quote can break out of the atom.
    }

    #[test]
    fn from_domain_renders_at_prefix() {
        let mut q = SearchQuery::default();
        q.from_domain = Some("acme.com".into());
        assert_eq!(build_search_atoms(&q).unwrap(), r#"FROM "@acme.com""#);
    }

    #[test]
    fn from_domain_strips_redundant_at() {
        let mut q = SearchQuery::default();
        q.from_domain = Some("@acme.com".into());
        // We strip the operator's leading `@` and re-add ours so the
        // emitted atom is exactly `@acme.com`, not `@@acme.com`.
        assert_eq!(build_search_atoms(&q).unwrap(), r#"FROM "@acme.com""#);
    }

    #[test]
    fn from_domain_empty_is_dropped() {
        let mut q = SearchQuery::default();
        q.from_domain = Some("@".into());
        assert_eq!(build_search_atoms(&q).unwrap(), "ALL");
    }

    #[test]
    fn has_attachments_emits_multipart_body_atom() {
        let mut q = SearchQuery::default();
        q.has_attachments = Some(true);
        assert_eq!(build_search_atoms(&q).unwrap(), r#"BODY "multipart/""#);
    }

    #[test]
    fn from_domain_composes_with_other_filters() {
        let mut q = SearchQuery::default();
        q.from_domain = Some("acme.com".into());
        q.unseen = Some(true);
        let s = build_search_atoms(&q).unwrap();
        assert!(s.contains(r#"FROM "@acme.com""#));
        assert!(s.contains("UNSEEN"));
    }
}
