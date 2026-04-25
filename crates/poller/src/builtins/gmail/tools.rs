//! Custom LLM tools shipped by the `gmail` poller built-in.
//!
//! Six tools beyond the generic `pollers_*`:
//!
//! - `gmail_count_unread {id}` — peek at how many messages currently
//!   match a job's query, without persisting state.
//! - `gmail_search {query, max_results?}` — ad-hoc Gmail search; uses
//!   the calling agent's Google credentials.
//! - `gmail_message_get {id}` — fetch one message's subject/from/snippet.
//! - `gmail_mark_read {id}` / `gmail_mark_unread {id}` — flip the
//!   UNREAD label on a single message.
//! - `gmail_labels_list` — enumerate the calling agent's labels.
//!
//! Every tool resolves the agent via `_agent_id` injected by
//! `nexo-poller-tools::CustomToolAdapter`. The `_` prefix prevents
//! a prompt-injection from spoofing the agent — the LLM cannot
//! override that key after the adapter sets it.

use std::sync::Arc;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use nexo_auth::handle::GOOGLE;
use nexo_llm::ToolDef;
use nexo_plugin_google::{GoogleAuthClient, GoogleAuthConfig, SecretSources};
use serde_json::{json, Value};

use super::GmailInner;
use crate::{CustomToolHandler, CustomToolSpec, PollerRunner};

pub fn build_tools(inner: Arc<GmailInner>) -> Vec<CustomToolSpec> {
    vec![
        spec("gmail_count_unread",
             "Run a gmail poll job's query once without persisting state. Returns how many messages match (`matching`) and how many would be dispatched (`would_dispatch`). Use as a sanity check before pause/resume or before editing the template.",
             json!({
                 "type": "object",
                 "properties": {
                     "id": { "type": "string", "description": "Gmail poll job id" }
                 },
                 "required": ["id"]
             }),
             CountUnread { inner: Arc::clone(&inner) }),
        spec("gmail_search",
             "Ad-hoc Gmail search using the calling agent's credentials. Returns up to `max_results` (default 10) messages with id, subject, from, snippet. Read-only — does not mark anything read.",
             json!({
                 "type": "object",
                 "properties": {
                     "query":        { "type": "string", "description": "Gmail search query (same syntax as the Gmail UI)." },
                     "max_results":  { "type": "integer", "description": "Cap on results (default 10, max 50)." }
                 },
                 "required": ["query"]
             }),
             Search { inner: Arc::clone(&inner) }),
        spec("gmail_message_get",
             "Fetch a single Gmail message by id. Returns subject, from, snippet, and a truncated body (first 4 KB).",
             json!({
                 "type": "object",
                 "properties": {
                     "id": { "type": "string", "description": "Gmail message id (from gmail_search)." }
                 },
                 "required": ["id"]
             }),
             MessageGet { inner: Arc::clone(&inner) }),
        spec("gmail_mark_read",
             "Mark a Gmail message as read (removes the UNREAD label).",
             json!({
                 "type": "object",
                 "properties": { "id": { "type": "string" } },
                 "required": ["id"]
             }),
             ModifyLabel { inner: Arc::clone(&inner), add: vec![], remove: vec!["UNREAD".into()], op: "mark_read" }),
        spec("gmail_mark_unread",
             "Restore the UNREAD label on a Gmail message.",
             json!({
                 "type": "object",
                 "properties": { "id": { "type": "string" } },
                 "required": ["id"]
             }),
             ModifyLabel { inner: Arc::clone(&inner), add: vec!["UNREAD".into()], remove: vec![], op: "mark_unread" }),
        spec("gmail_labels_list",
             "List every label in the calling agent's Gmail account (system + user-defined). Useful to find the right id for downstream filter / label automations.",
             json!({ "type": "object", "properties": {} }),
             LabelsList { inner }),
    ]
}

fn spec(
    name: &str,
    description: &str,
    parameters: Value,
    handler: impl CustomToolHandler,
) -> CustomToolSpec {
    CustomToolSpec {
        def: ToolDef {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        },
        handler: Arc::new(handler),
    }
}

// ── handlers ─────────────────────────────────────────────────────────

struct CountUnread {
    inner: Arc<GmailInner>,
}
#[async_trait]
impl CustomToolHandler for CountUnread {
    async fn call(&self, runner: Arc<PollerRunner>, args: Value) -> anyhow::Result<Value> {
        let _ = &self.inner; // silence unused-field warning when args ignore inner
        let id = args["id"]
            .as_str()
            .ok_or_else(|| anyhow!("gmail_count_unread requires `id`"))?;
        let outcome = runner.run_once(id).await?;
        Ok(json!({
            "ok": true,
            "matching": outcome.items_seen,
            "would_dispatch": outcome.items_dispatched,
        }))
    }
}

struct Search {
    inner: Arc<GmailInner>,
}
#[async_trait]
impl CustomToolHandler for Search {
    async fn call(&self, runner: Arc<PollerRunner>, args: Value) -> anyhow::Result<Value> {
        let agent = require_agent_id(&args)?;
        let query = args["query"]
            .as_str()
            .ok_or_else(|| anyhow!("gmail_search requires `query`"))?;
        let max = args["max_results"].as_u64().unwrap_or(10).min(50);

        let client = client_for_agent(&self.inner, &runner, agent).await?;
        let url = format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages?q={}&maxResults={}",
            urlencode(query),
            max
        );
        let listing: Value = client.authorized_call("GET", &url, None).await?;
        let ids: Vec<String> = listing
            .get("messages")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let mut results = Vec::with_capacity(ids.len());
        for id in &ids {
            let msg_url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=metadata&metadataHeaders=Subject&metadataHeaders=From"
            );
            if let Ok(msg) = client.authorized_call("GET", &msg_url, None).await {
                results.push(json!({
                    "id":      id,
                    "subject": header(&msg, "Subject").unwrap_or_default(),
                    "from":    header(&msg, "From").unwrap_or_default(),
                    "snippet": msg.get("snippet").and_then(Value::as_str).unwrap_or(""),
                }));
            }
        }
        Ok(json!({ "matches": results.len(), "messages": results }))
    }
}

struct MessageGet {
    inner: Arc<GmailInner>,
}
#[async_trait]
impl CustomToolHandler for MessageGet {
    async fn call(&self, runner: Arc<PollerRunner>, args: Value) -> anyhow::Result<Value> {
        let agent = require_agent_id(&args)?;
        let id = args["id"]
            .as_str()
            .ok_or_else(|| anyhow!("gmail_message_get requires `id`"))?;
        let client = client_for_agent(&self.inner, &runner, agent).await?;
        let url =
            format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full");
        let msg: Value = client.authorized_call("GET", &url, None).await?;
        let subject = header(&msg, "Subject").unwrap_or_default();
        let from = header(&msg, "From").unwrap_or_default();
        let snippet = msg
            .get("snippet")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut body = extract_body(&msg);
        const BODY_CAP: usize = 4096;
        if body.len() > BODY_CAP {
            body.truncate(BODY_CAP);
            body.push_str("\n…(truncated)");
        }
        Ok(json!({
            "id": id,
            "subject": subject,
            "from": from,
            "snippet": snippet,
            "body": body,
        }))
    }
}

struct ModifyLabel {
    inner: Arc<GmailInner>,
    add: Vec<String>,
    remove: Vec<String>,
    op: &'static str,
}
#[async_trait]
impl CustomToolHandler for ModifyLabel {
    async fn call(&self, runner: Arc<PollerRunner>, args: Value) -> anyhow::Result<Value> {
        let agent = require_agent_id(&args)?;
        let id = args["id"]
            .as_str()
            .ok_or_else(|| anyhow!("gmail_{} requires `id`", self.op))?;
        let client = client_for_agent(&self.inner, &runner, agent).await?;
        let url = format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}/modify");
        let body = json!({ "addLabelIds": self.add, "removeLabelIds": self.remove });
        client
            .authorized_call("POST", &url, Some(body))
            .await
            .with_context(|| format!("gmail_{}", self.op))?;
        Ok(json!({ "ok": true, "id": id, "op": self.op }))
    }
}

struct LabelsList {
    inner: Arc<GmailInner>,
}
#[async_trait]
impl CustomToolHandler for LabelsList {
    async fn call(&self, runner: Arc<PollerRunner>, args: Value) -> anyhow::Result<Value> {
        let agent = require_agent_id(&args)?;
        let client = client_for_agent(&self.inner, &runner, agent).await?;
        let url = "https://gmail.googleapis.com/gmail/v1/users/me/labels";
        let resp: Value = client.authorized_call("GET", url, None).await?;
        let labels = resp
            .get("labels")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let projection: Vec<Value> = labels
            .iter()
            .map(|l| {
                json!({
                    "id":   l.get("id").and_then(Value::as_str).unwrap_or(""),
                    "name": l.get("name").and_then(Value::as_str).unwrap_or(""),
                    "type": l.get("type").and_then(Value::as_str).unwrap_or(""),
                })
            })
            .collect();
        Ok(json!({ "labels": projection }))
    }
}

// ── shared helpers ───────────────────────────────────────────────────

fn require_agent_id(args: &Value) -> anyhow::Result<&str> {
    args["_agent_id"]
        .as_str()
        .ok_or_else(|| anyhow!("internal: _agent_id missing — adapter did not inject"))
}

async fn client_for_agent(
    inner: &Arc<GmailInner>,
    runner: &Arc<PollerRunner>,
    agent_id: &str,
) -> anyhow::Result<Arc<GoogleAuthClient>> {
    let bundle = runner.credentials();
    let handle = bundle
        .resolver
        .resolve(agent_id, GOOGLE)
        .map_err(|_| anyhow!("agent '{agent_id}' has no Google credential bound"))?;
    let account_id = handle.account_id_raw().to_string();
    if let Some(c) = inner.clients.get(&account_id) {
        return Ok(Arc::clone(c.value()));
    }
    let account = bundle
        .stores
        .google
        .account(&account_id)
        .ok_or_else(|| anyhow!("Google account '{account_id}' not in store"))?;
    let cid = std::fs::read_to_string(&account.client_id_path)?
        .trim()
        .to_string();
    let csec = std::fs::read_to_string(&account.client_secret_path)?
        .trim()
        .to_string();
    let cfg = GoogleAuthConfig {
        client_id: cid,
        client_secret: csec,
        scopes: account.scopes.clone(),
        token_file: account.token_path.to_string_lossy().into_owned(),
        redirect_port: 0,
    };
    let workspace = account
        .token_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let client = GoogleAuthClient::new_with_sources(
        cfg,
        &workspace,
        Some(SecretSources {
            client_id_path: account.client_id_path.clone(),
            client_secret_path: account.client_secret_path.clone(),
        }),
    );
    client.load_from_disk().await?;
    inner.clients.insert(account_id, client.clone());
    Ok(client)
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            out.push(ch);
        } else {
            for b in ch.to_string().as_bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

fn header(msg: &Value, name: &str) -> Option<String> {
    let headers = msg.get("payload")?.get("headers")?.as_array()?;
    for h in headers {
        if h.get("name").and_then(Value::as_str)? == name {
            return h.get("value").and_then(Value::as_str).map(str::to_string);
        }
    }
    None
}

fn extract_body(msg: &Value) -> String {
    let Some(payload) = msg.get("payload") else {
        return String::new();
    };
    if let Some(t) = find_body(payload, "text/plain") {
        return t;
    }
    if let Some(html) = find_body(payload, "text/html") {
        return strip_html(&html);
    }
    String::new()
}

fn find_body(part: &Value, want: &str) -> Option<String> {
    let mime = part.get("mimeType").and_then(Value::as_str).unwrap_or("");
    if mime == want {
        if let Some(data) = part
            .get("body")
            .and_then(|b| b.get("data"))
            .and_then(Value::as_str)
        {
            return decode_b64url(data);
        }
    }
    if let Some(parts) = part.get("parts").and_then(Value::as_array) {
        for p in parts {
            if let Some(t) = find_body(p, want) {
                return Some(t);
            }
        }
    }
    None
}

fn decode_b64url(s: &str) -> Option<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_agent_rejects_missing() {
        let args = json!({ "id": "x" });
        assert!(require_agent_id(&args).is_err());
    }

    #[test]
    fn require_agent_returns_string() {
        let args = json!({ "_agent_id": "ana", "id": "x" });
        assert_eq!(require_agent_id(&args).unwrap(), "ana");
    }

    #[test]
    fn build_tools_returns_six() {
        let inner = Arc::new(GmailInner {
            clients: dashmap::DashMap::new(),
        });
        let tools = build_tools(inner);
        let names: Vec<_> = tools.iter().map(|t| t.def.name.as_str()).collect();
        assert_eq!(names.len(), 6);
        assert!(names.contains(&"gmail_count_unread"));
        assert!(names.contains(&"gmail_search"));
        assert!(names.contains(&"gmail_message_get"));
        assert!(names.contains(&"gmail_mark_read"));
        assert!(names.contains(&"gmail_mark_unread"));
        assert!(names.contains(&"gmail_labels_list"));
    }
}
