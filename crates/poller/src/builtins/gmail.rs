//! `kind: gmail` — Gmail v1 search → regex extract → outbound dispatch.
//!
//! One `PollerJob` per query: a single agent (Ana) can run many Gmail
//! polls (`ana_leads`, `ana_invoices`, `ana_monitor`) sharing the same
//! Google credentials but with independent cursors, schedules, and
//! delivery targets.
//!
//! Cursor: not used today — Gmail's `is:unread` + post-dispatch
//! `mark_read` is the dedup. The cursor slot stays reserved so a
//! future migration to `historyId` is non-breaking.

use std::collections::HashMap;
use std::sync::Arc;

use agent_auth::handle::GOOGLE;
use agent_plugin_google::{GoogleAuthClient, GoogleAuthConfig, SecretSources};
use anyhow::Context;
use async_trait::async_trait;
use base64::Engine;
use dashmap::DashMap;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::PollerError;
use crate::poller::{OutboundDelivery, PollContext, Poller, TickOutcome};

/// Per-job YAML shape under `config:`.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct GmailJobConfig {
    /// Gmail search query (`is:unread`, `subject:lead`, …).
    pub query: String,
    /// `newer_than:` suffix appended to the query (`1d`, `2h`).
    /// Avoids back-filling years of historical mail on first deploy.
    #[serde(default)]
    pub newer_than: Option<String>,
    /// Hard cap on dispatches per tick.
    #[serde(default = "default_max_per_tick")]
    pub max_per_tick: usize,
    /// Throttle (ms) between dispatches inside the same tick.
    #[serde(default = "default_dispatch_delay")]
    pub dispatch_delay_ms: u64,
    /// `From:` substring filter — empty = accept any sender.
    #[serde(default)]
    pub sender_allowlist: Vec<String>,
    /// Named regexes against the body. Each capture group becomes a
    /// `{field}` placeholder in the template.
    #[serde(default)]
    pub extract: HashMap<String, String>,
    /// Skip dispatch when any of these extracted fields ended up empty.
    #[serde(default)]
    pub require_fields: Vec<String>,
    /// Mustache-light template with `{field}` substitutions. `{subject}`,
    /// `{from}`, and `{snippet}` are always available.
    pub message_template: String,
    /// Mark each dispatched message as read in Gmail. Default true.
    #[serde(default = "default_mark_read")]
    pub mark_read_on_dispatch: bool,
    /// Where to send the rendered message. The runner uses Phase 17 to
    /// look up the agent's binding for `deliver.channel`.
    pub deliver: DeliverCfg,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DeliverCfg {
    /// `whatsapp` | `telegram` | `google`.
    pub channel: String,
    /// JID / chat_id / phone — passed through to the channel plugin.
    pub to: String,
}

fn default_max_per_tick() -> usize { 20 }
fn default_dispatch_delay() -> u64 { 1000 }
fn default_mark_read() -> bool { true }

pub struct GmailPoller {
    /// Cached `GoogleAuthClient`s keyed by Google account id. Multiple
    /// gmail jobs for the same agent share the same cached client so
    /// token refreshes happen once.
    clients: DashMap<String, Arc<GoogleAuthClient>>,
}

impl GmailPoller {
    pub fn new() -> Self {
        Self {
            clients: DashMap::new(),
        }
    }
}

impl Default for GmailPoller {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl Poller for GmailPoller {
    fn kind(&self) -> &'static str { "gmail" }

    fn description(&self) -> &'static str {
        "Polls Gmail with a search query, extracts fields via regex, dispatches to a channel."
    }

    fn validate(&self, config: &Value) -> Result<(), PollerError> {
        let _: GmailJobConfig =
            serde_json::from_value(config.clone()).map_err(|e| PollerError::Config {
                job: "<gmail>".into(),
                reason: e.to_string(),
            })?;
        Ok(())
    }

    fn custom_tools(&self) -> Vec<crate::CustomToolSpec> {
        use agent_llm::ToolDef;
        use serde_json::json;
        struct CountUnread;
        #[async_trait::async_trait]
        impl crate::CustomToolHandler for CountUnread {
            async fn call(
                &self,
                runner: std::sync::Arc<crate::PollerRunner>,
                args: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                let id = args["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("gmail_count_unread requires `id`"))?;
                // Trigger one tick out-of-band; report items_seen as a
                // proxy for "currently unread matching the query".
                let outcome = runner.run_once(id).await?;
                Ok(json!({
                    "ok": true,
                    "matching": outcome.items_seen,
                    "would_dispatch": outcome.items_dispatched,
                }))
            }
        }
        vec![crate::CustomToolSpec {
            def: ToolDef {
                name: "gmail_count_unread".into(),
                description:
                    "Run the gmail job's query once without persisting state — returns how many messages currently match. Useful as a sanity check before changing the template or pause/resume."
                        .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Gmail poll job id" }
                    },
                    "required": ["id"]
                }),
            },
            handler: std::sync::Arc::new(CountUnread),
        }]
    }

    async fn tick(&self, ctx: &PollContext) -> Result<TickOutcome, PollerError> {
        let cfg: GmailJobConfig = serde_json::from_value(ctx.config.clone()).map_err(|e| {
            PollerError::Config {
                job: ctx.job_id.clone(),
                reason: e.to_string(),
            }
        })?;

        let google = ctx
            .credentials
            .resolve(&ctx.agent_id, GOOGLE)
            .map_err(|_| PollerError::CredentialsMissing {
                agent: ctx.agent_id.clone(),
                channel: GOOGLE,
            })?;

        let client = self.client_for(ctx, &google).await?;

        // Compose the effective query.
        let mut q = cfg.query.clone();
        if let Some(bound) = cfg.newer_than.as_deref() {
            if !bound.trim().is_empty() {
                q.push_str(&format!(" newer_than:{bound}"));
            }
        }
        let list_url = format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages?q={}&maxResults={}",
            urlencode(&q),
            cfg.max_per_tick
        );
        let list: Value = client
            .authorized_call("GET", &list_url, None)
            .await
            .map_err(|e| classify_google_err(e, "list messages"))?;

        let messages = match list.get("messages").and_then(Value::as_array) {
            Some(v) => v,
            None => return Ok(TickOutcome::default()),
        };

        let mut compiled_extract: HashMap<String, Regex> = HashMap::new();
        for (name, pat) in &cfg.extract {
            let re = Regex::new(pat).map_err(|e| PollerError::Config {
                job: ctx.job_id.clone(),
                reason: format!("invalid regex for `{name}`: {e}"),
            })?;
            compiled_extract.insert(name.clone(), re);
        }

        let target_channel: agent_auth::Channel = match cfg.deliver.channel.as_str() {
            "whatsapp" => agent_auth::handle::WHATSAPP,
            "telegram" => agent_auth::handle::TELEGRAM,
            "google" => agent_auth::handle::GOOGLE,
            other => {
                return Err(PollerError::Config {
                    job: ctx.job_id.clone(),
                    reason: format!("unknown deliver.channel '{other}'"),
                });
            }
        };

        // Belt-and-suspenders dedup: parity with the legacy
        // `gmail-poller`'s `.seen.json` cache. Persisted in the
        // SQLite cursor as a JSON array of message ids; cap at 5000,
        // drop oldest when over. Catches the rare case where dispatch
        // succeeds but `mark_read` fails — Gmail still returns the
        // message as UNREAD next tick, but `seen` blocks duplicate
        // dispatch.
        let mut seen_set: std::collections::HashSet<String> = ctx
            .cursor
            .as_deref()
            .and_then(|b| serde_json::from_slice::<Vec<String>>(b).ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default();

        let mut deliveries = Vec::new();
        let mut items_seen = 0u32;
        for (idx, m) in messages.iter().take(cfg.max_per_tick).enumerate() {
            items_seen += 1;
            let Some(id) = m.get("id").and_then(Value::as_str) else {
                continue;
            };
            if seen_set.contains(id) {
                tracing::debug!(
                    job = %ctx.job_id,
                    message_id = %id,
                    "gmail tick: skip (already in seen cache)"
                );
                continue;
            }
            match self
                .process_one(
                    id,
                    &cfg,
                    &compiled_extract,
                    target_channel,
                    &client,
                )
                .await
            {
                Ok(Some(d)) => {
                    deliveries.push(d);
                    seen_set.insert(id.to_string());
                    if idx + 1 < messages.len() && cfg.dispatch_delay_ms > 0 {
                        tokio::select! {
                            _ = tokio::time::sleep(
                                std::time::Duration::from_millis(cfg.dispatch_delay_ms)
                            ) => {}
                            _ = ctx.cancel.cancelled() => break,
                        }
                    }
                }
                Ok(None) => {
                    // Filter miss / required-field empty — remember
                    // the id so a future tick does not re-attempt
                    // (mirrors legacy `remember_seen` after
                    // `try_mark_read`).
                    seen_set.insert(id.to_string());
                }
                Err(e) => {
                    tracing::warn!(
                        job = %ctx.job_id,
                        message_id = %id,
                        error = %e,
                        "gmail tick: process_one failed"
                    );
                }
            }
        }

        // Cap the seen set, drop oldest 1000 by string sort (Gmail
        // ids are roughly monotonic). Same trim threshold as the
        // legacy implementation to keep memory bounded.
        if seen_set.len() > 5000 {
            let mut ids: Vec<String> = seen_set.iter().cloned().collect();
            ids.sort();
            for id in ids.into_iter().take(1000) {
                seen_set.remove(&id);
            }
        }
        let next_cursor =
            serde_json::to_vec(&seen_set.into_iter().collect::<Vec<_>>()).ok();

        let dispatched = deliveries.len() as u32;
        Ok(TickOutcome {
            items_seen,
            items_dispatched: dispatched,
            deliver: deliveries,
            next_cursor,
            next_interval_hint: None,
        })
    }
}

impl GmailPoller {
    /// Lazy-cached client per Google account id. The first job to ask
    /// for `ana@gmail.com` builds the GoogleAuthClient + load_from_disk;
    /// subsequent jobs (other gmail polls for the same agent) reuse it.
    async fn client_for(
        &self,
        ctx: &PollContext,
        handle: &agent_auth::CredentialHandle,
    ) -> Result<Arc<GoogleAuthClient>, PollerError> {
        let account_id = handle.account_id_raw().to_string();
        if let Some(existing) = self.clients.get(&account_id) {
            return Ok(existing.clone());
        }

        let stores = ctx.stores.as_ref().ok_or_else(|| PollerError::Config {
            job: ctx.job_id.clone(),
            reason: "PollContext.stores is None — wire CredentialsBundle into PollerRunner".into(),
        })?;
        let account = stores.google.account(&account_id).ok_or_else(|| {
            PollerError::CredentialsMissing {
                agent: ctx.agent_id.clone(),
                channel: GOOGLE,
            }
        })?;

        let client_id = read_trim(&account.client_id_path).map_err(PollerError::Transient)?;
        let client_secret =
            read_trim(&account.client_secret_path).map_err(PollerError::Transient)?;

        let cfg = GoogleAuthConfig {
            client_id,
            client_secret,
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
        client
            .load_from_disk()
            .await
            .map_err(|e| PollerError::Permanent(e.context("google: load_from_disk")))?;

        self.clients.insert(account_id.clone(), client.clone());
        Ok(client)
    }

    async fn process_one(
        &self,
        id: &str,
        cfg: &GmailJobConfig,
        extract: &HashMap<String, Regex>,
        target_channel: agent_auth::Channel,
        client: &Arc<GoogleAuthClient>,
    ) -> Result<Option<OutboundDelivery>, anyhow::Error> {
        let url = format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full"
        );
        let msg: Value = client
            .authorized_call("GET", &url, None)
            .await
            .context("get message detail")?;

        let subject = header_value(&msg, "Subject").unwrap_or_default();
        let from = header_value(&msg, "From").unwrap_or_default();
        let snippet = msg
            .get("snippet")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let body = extract_body(&msg);

        if !cfg.sender_allowlist.is_empty() {
            let from_l = from.to_lowercase();
            let allowed = cfg
                .sender_allowlist
                .iter()
                .any(|s| from_l.contains(&s.to_lowercase()));
            if !allowed {
                return Ok(None);
            }
        }

        let mut fields: HashMap<String, String> = HashMap::new();
        fields.insert("subject".into(), subject.clone());
        fields.insert("snippet".into(), snippet.clone());
        fields.insert("from".into(), from.clone());
        for (name, re) in extract {
            let captured = re
                .captures(&body)
                .or_else(|| re.captures(&snippet))
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            fields.insert(name.clone(), captured);
        }

        for req in &cfg.require_fields {
            let v = fields.get(req).map(String::as_str).unwrap_or("");
            if v.is_empty() {
                if cfg.mark_read_on_dispatch {
                    mark_read(client, id).await.ok();
                }
                return Ok(None);
            }
        }

        let text = render_template(&cfg.message_template, &fields);
        if cfg.mark_read_on_dispatch {
            mark_read(client, id).await.ok();
        }

        Ok(Some(OutboundDelivery {
            channel: target_channel,
            recipient: cfg.deliver.to.clone(),
            payload: json!({ "text": text }),
        }))
    }
}

fn read_trim(p: &std::path::Path) -> anyhow::Result<String> {
    Ok(std::fs::read_to_string(p)?.trim().to_string())
}

fn classify_google_err(err: anyhow::Error, ctx: &str) -> PollerError {
    let msg = err.to_string();
    if msg.contains("invalid_grant") || msg.contains("revoked") || msg.contains("401") {
        PollerError::Permanent(err.context(format!("google: {ctx}")))
    } else {
        PollerError::Transient(err.context(format!("google: {ctx}")))
    }
}

async fn mark_read(client: &Arc<GoogleAuthClient>, id: &str) -> anyhow::Result<()> {
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}/modify"
    );
    let body = json!({ "removeLabelIds": ["UNREAD"] });
    client
        .authorized_call("POST", &url, Some(body))
        .await
        .context("gmail: mark_read")?;
    Ok(())
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

fn header_value(msg: &Value, name: &str) -> Option<String> {
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
    if let Some(text) = find_body(payload, "text/plain") {
        return text;
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

fn render_template(template: &str, fields: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = template[i + 1..].find('}') {
                let key = &template[i + 1..i + 1 + end];
                if let Some(v) = fields.get(key) {
                    out.push_str(v);
                    i += 1 + end + 1;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_minimal() {
        let p = GmailPoller::new();
        let cfg = json!({
            "query": "is:unread",
            "message_template": "{snippet}",
            "deliver": { "channel": "whatsapp", "to": "57300@s.whatsapp.net" },
        });
        p.validate(&cfg).unwrap();
    }

    #[test]
    fn validate_rejects_missing_template() {
        let p = GmailPoller::new();
        let cfg = json!({ "query": "is:unread", "deliver": { "channel": "whatsapp", "to": "x" }});
        assert!(p.validate(&cfg).is_err());
    }

    #[test]
    fn validate_rejects_unknown_field() {
        let p = GmailPoller::new();
        let cfg = json!({
            "query": "is:unread",
            "message_template": "x",
            "deliver": { "channel": "whatsapp", "to": "y" },
            "wat": "no",
        });
        assert!(p.validate(&cfg).is_err());
    }

    #[test]
    fn render_template_substitutes_known_keys() {
        let mut f = HashMap::new();
        f.insert("name".into(), "Ana".into());
        f.insert("phone".into(), "+57300".into());
        let s = render_template("Hi {name} ({phone})", &f);
        assert_eq!(s, "Hi Ana (+57300)");
    }

    #[test]
    fn render_template_keeps_unknown_braces() {
        let f = HashMap::new();
        let s = render_template("{unknown}", &f);
        assert_eq!(s, "{unknown}");
    }

    #[test]
    fn strip_html_removes_tags() {
        assert_eq!(strip_html("<p>Hi <b>there</b></p>"), "Hi there");
    }

    #[test]
    fn classify_revoked_is_permanent() {
        let e = anyhow::anyhow!("invalid_grant: revoked");
        assert!(matches!(
            classify_google_err(e, "ctx"),
            PollerError::Permanent(_)
        ));
    }

    #[test]
    fn classify_5xx_is_transient() {
        let e = anyhow::anyhow!("503 backend error");
        assert!(matches!(
            classify_google_err(e, "ctx"),
            PollerError::Transient(_)
        ));
    }
}
