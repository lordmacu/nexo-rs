//! One tick of Gmail polling for a single [`JobConfig`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use agent_broker::{AnyBroker, BrokerHandle, Event};
use agent_plugin_google::GoogleAuthClient;
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::config::JobConfig;

/// Precompiled regex set + persistent dedup cache for one job. Built
/// once at plugin init; the cache acts as belt-and-suspenders on top
/// of Gmail's UNREAD flag.
pub struct CompiledJob {
    pub cfg: JobConfig,
    pub extract: HashMap<String, Regex>,
    /// Substring/domain filters for the `From:` header (lowercased).
    pub sender_allowlist: Vec<String>,
    /// In-memory mirror of `<state_dir>/<job>.seen.json`. Survives
    /// restarts so if `mark_read` fails mid-flow we still don't
    /// re-dispatch on boot.
    pub seen: Arc<Mutex<HashSet<String>>>,
    pub seen_path: PathBuf,
}

impl CompiledJob {
    pub fn new(cfg: JobConfig, state_dir: &std::path::Path) -> Result<Self> {
        let mut extract = HashMap::new();
        for (field, pattern) in &cfg.extract {
            let re = Regex::new(pattern)
                .with_context(|| format!("invalid regex for field `{field}`: {pattern}"))?;
            extract.insert(field.clone(), re);
        }
        let sender_allowlist = cfg
            .sender_allowlist
            .iter()
            .map(|s| s.to_lowercase())
            .collect();
        let seen_path = state_dir.join(format!("gmail-poller-{}.seen.json", cfg.name));
        let seen = load_seen(&seen_path);
        Ok(Self {
            cfg,
            extract,
            sender_allowlist,
            seen: Arc::new(Mutex::new(seen)),
            seen_path,
        })
    }
}

fn load_seen(path: &std::path::Path) -> HashSet<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<HashSet<String>>(&s).ok())
        .unwrap_or_default()
}

async fn persist_seen(path: &std::path::Path, seen: &HashSet<String>) {
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Ok(json) = serde_json::to_vec(seen) {
        let _ = tokio::fs::write(path, &json).await;
    }
}

/// Run one pass of the poller for a compiled job. Emits 0..N outbound
/// events, marks each matched message as read. Errors short-circuit
/// the tick but leave state consistent — Gmail labels are the source
/// of truth for dedup.
pub async fn run_once(
    job: &CompiledJob,
    google: &Arc<GoogleAuthClient>,
    broker: &AnyBroker,
) -> Result<usize> {
    // Compose the effective query — base + optional `newer_than` bound.
    // `newer_than` is a separate config field (not inline in `query`)
    // so it's obvious on first deploy and easy to remove later.
    let mut effective_q = job.cfg.query.clone();
    if let Some(bound) = job.cfg.newer_than.as_deref() {
        if !bound.trim().is_empty() {
            effective_q.push_str(&format!(" newer_than:{bound}"));
        }
    }
    let list_url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages?q={}&maxResults={}",
        urlencode(&effective_q),
        job.cfg.max_per_tick
    );
    let list: Value = google
        .authorized_call("GET", &list_url, None)
        .await
        .context("list messages")?;
    let messages = list.get("messages").and_then(Value::as_array);
    let Some(messages) = messages else {
        return Ok(0);
    };

    let mut dispatched = 0usize;
    for (idx, m) in messages.iter().take(job.cfg.max_per_tick).enumerate() {
        let Some(id) = m.get("id").and_then(Value::as_str) else {
            continue;
        };
        {
            let seen = job.seen.lock().await;
            if seen.contains(id) {
                // Local cache hit → Gmail mark_read must have failed
                // last time. Skip to avoid duplicate dispatch.
                tracing::debug!(
                    job = %job.cfg.name,
                    message_id = %id,
                    "gmail-poller: skip (already in seen cache)"
                );
                continue;
            }
        }
        match process_one(id, job, google, broker).await {
            Ok(true) => {
                dispatched += 1;
                // Throttle between dispatches so a spike doesn't
                // hammer the downstream channel (WhatsApp rate limits
                // sit around 1 msg/sec per chat). Skip the sleep for
                // the last one.
                if idx + 1 < messages.len() {
                    tokio::time::sleep(std::time::Duration::from_millis(
                        job.cfg.dispatch_delay_ms,
                    ))
                    .await;
                }
            }
            Ok(false) => {
                // Skipped intentionally (missing required field,
                // sender not in allowlist). Don't count, don't retry.
            }
            Err(e) => {
                tracing::warn!(
                    job = %job.cfg.name,
                    message_id = %id,
                    error = %e,
                    "gmail-poller: failed to process message"
                );
                continue;
            }
        }
    }
    Ok(dispatched)
}

/// Returns `Ok(true)` when the message was dispatched, `Ok(false)`
/// when it was intentionally skipped (filter miss, missing required
/// field), `Err` on transient failures the caller should log + retry.
async fn process_one(
    id: &str,
    job: &CompiledJob,
    google: &Arc<GoogleAuthClient>,
    broker: &AnyBroker,
) -> Result<bool> {
    let msg_url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full"
    );
    let msg: Value = google
        .authorized_call("GET", &msg_url, None)
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

    // Sender allowlist — early skip (no dispatch, no mark-read).
    if !job.sender_allowlist.is_empty() {
        let from_l = from.to_lowercase();
        let allowed = job.sender_allowlist.iter().any(|needle| from_l.contains(needle));
        if !allowed {
            tracing::debug!(
                job = %job.cfg.name,
                message_id = %id,
                from = %from,
                "gmail-poller: sender not in allowlist, skip"
            );
            return Ok(false);
        }
    }

    // Apply extraction regexes against body first, fall back to
    // snippet, finally empty string so template rendering never fails.
    let mut fields: HashMap<String, String> = HashMap::new();
    fields.insert("subject".to_string(), subject.clone());
    fields.insert("snippet".to_string(), snippet.clone());
    fields.insert("from".to_string(), from.clone());
    for (name, re) in &job.extract {
        let captured = re
            .captures(&body)
            .or_else(|| re.captures(&snippet))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        fields.insert(name.clone(), captured);
    }

    // Required-fields gate — skip dispatch if the regexes didn't find
    // what the template depends on. The message still gets marked
    // read so we don't pile up re-attempts on a malformed email.
    for req in &job.cfg.require_fields {
        let v = fields.get(req).map(String::as_str).unwrap_or("");
        if v.is_empty() {
            tracing::warn!(
                job = %job.cfg.name,
                message_id = %id,
                required = %req,
                "gmail-poller: required field empty, skip dispatch"
            );
            if job.cfg.mark_read_on_dispatch {
                try_mark_read(id, google).await;
            }
            remember_seen(job, id).await;
            return Ok(false);
        }
    }

    let text = render_template(&job.cfg.message_template, &fields);

    // Dispatch to the configured channel. We publish a plain outbound
    // event — the channel plugin handles delivery + retries.
    let payload = json!({
        "to": job.cfg.forward_to,
        "text": text,
    });
    let event = Event::new(&job.cfg.forward_to_subject, "gmail-poller", payload);
    broker
        .publish(&job.cfg.forward_to_subject, event)
        .await
        .context("publish outbound event")?;

    remember_seen(job, id).await;

    // Dedup flip. Failures here don't bail — `seen` cache already
    // prevents re-dispatch next tick. Worst case: the email stays
    // UNREAD in Gmail but we won't send it again.
    if job.cfg.mark_read_on_dispatch {
        try_mark_read(id, google).await;
    }

    tracing::info!(
        job = %job.cfg.name,
        message_id = %id,
        subject = %subject,
        "gmail-poller: dispatched"
    );
    Ok(true)
}

async fn try_mark_read(id: &str, google: &Arc<GoogleAuthClient>) {
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}/modify"
    );
    let body = json!({ "removeLabelIds": ["UNREAD"] });
    if let Err(e) = google.authorized_call("POST", &url, Some(body)).await {
        tracing::warn!(
            message_id = %id,
            error = %e,
            "gmail-poller: failed to mark read"
        );
    }
}

async fn remember_seen(job: &CompiledJob, id: &str) {
    let mut seen = job.seen.lock().await;
    seen.insert(id.to_string());
    // Cap the cache — Gmail message ids are monotonic enough that
    // dropping old ones is safe once messages hit the `cap` line.
    if seen.len() > 5000 {
        // Take a snapshot, drop the 1000 lexicographically smallest.
        let mut ids: Vec<String> = seen.iter().cloned().collect();
        ids.sort();
        for id in ids.into_iter().take(1000) {
            seen.remove(&id);
        }
    }
    persist_seen(&job.seen_path, &seen).await;
}

fn header_value(msg: &Value, name: &str) -> Option<String> {
    let headers = msg
        .get("payload")?
        .get("headers")?
        .as_array()?;
    for h in headers {
        if h.get("name").and_then(Value::as_str)? == name {
            return h.get("value").and_then(Value::as_str).map(str::to_string);
        }
    }
    None
}

/// Depth-first walk the MIME tree looking for a `text/plain` part
/// (fall back to the first `text/html` stripped of tags). Gmail
/// returns payloads base64url-encoded.
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

fn find_body(part: &Value, want_mime: &str) -> Option<String> {
    let mime = part.get("mimeType").and_then(Value::as_str).unwrap_or("");
    if mime == want_mime {
        if let Some(data) = part
            .get("body")
            .and_then(|b| b.get("data"))
            .and_then(Value::as_str)
        {
            return decode_b64url(data).ok();
        }
    }
    if let Some(parts) = part.get("parts").and_then(Value::as_array) {
        for p in parts {
            if let Some(text) = find_body(p, want_mime) {
                return Some(text);
            }
        }
    }
    None
}

fn decode_b64url(s: &str) -> Result<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .context("base64url decode")?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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

fn render_template(tmpl: &str, fields: &HashMap<String, String>) -> String {
    let mut out = tmpl.to_string();
    for (k, v) in fields {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.bytes() {
        match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(c as char)
            }
            _ => out.push_str(&format!("%{c:02X}")),
        }
    }
    out
}
