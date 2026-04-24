//! One tick of Gmail polling for a single [`JobConfig`].

use std::collections::HashMap;
use std::sync::Arc;

use agent_broker::{AnyBroker, BrokerHandle, Event};
use agent_plugin_google::GoogleAuthClient;
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{json, Value};

use crate::config::JobConfig;

/// Precompiled regex set for one job. Built once at plugin init so
/// every tick avoids the regex compile cost.
pub struct CompiledJob {
    pub cfg: JobConfig,
    pub extract: HashMap<String, Regex>,
}

impl CompiledJob {
    pub fn new(cfg: JobConfig) -> Result<Self> {
        let mut extract = HashMap::new();
        for (field, pattern) in &cfg.extract {
            let re = Regex::new(pattern)
                .with_context(|| format!("invalid regex for field `{field}`: {pattern}"))?;
            extract.insert(field.clone(), re);
        }
        Ok(Self { cfg, extract })
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
    let list_url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages?q={}&maxResults=20",
        urlencode(&job.cfg.query)
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
    for m in messages {
        let Some(id) = m.get("id").and_then(Value::as_str) else {
            continue;
        };
        if let Err(e) = process_one(id, job, google, broker).await {
            // Log and continue; other messages in the same tick still
            // get a chance. Gmail-side dedup via UNREAD label means a
            // transient failure retries next tick without duplicating.
            tracing::warn!(
                job = %job.cfg.name,
                message_id = %id,
                error = %e,
                "gmail-poller: failed to process message"
            );
            continue;
        }
        dispatched += 1;
    }
    Ok(dispatched)
}

async fn process_one(
    id: &str,
    job: &CompiledJob,
    google: &Arc<GoogleAuthClient>,
    broker: &AnyBroker,
) -> Result<()> {
    let msg_url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full"
    );
    let msg: Value = google
        .authorized_call("GET", &msg_url, None)
        .await
        .context("get message detail")?;

    let subject = header_value(&msg, "Subject").unwrap_or_default();
    let snippet = msg
        .get("snippet")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let body = extract_body(&msg);

    // Apply extraction regexes against body first, fall back to
    // snippet, finally empty string so template rendering never fails.
    let mut fields: HashMap<String, String> = HashMap::new();
    fields.insert("subject".to_string(), subject.clone());
    fields.insert("snippet".to_string(), snippet.clone());
    for (name, re) in &job.extract {
        let captured = re
            .captures(&body)
            .or_else(|| re.captures(&snippet))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        fields.insert(name.clone(), captured);
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

    // Dedup: flip UNREAD off. Success means next `is:unread` query
    // won't re-match this message. If the modify fails we DO NOT bail
    // — the message already fired, we just log and next tick may
    // re-send once (acceptable: Gmail marks read after the second
    // call anyway). Worst case: one duplicate notification.
    if job.cfg.mark_read_on_dispatch {
        let modify_url = format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}/modify"
        );
        let body = json!({ "removeLabelIds": ["UNREAD"] });
        if let Err(e) = google
            .authorized_call("POST", &modify_url, Some(body))
            .await
        {
            tracing::warn!(
                job = %job.cfg.name,
                message_id = %id,
                error = %e,
                "gmail-poller: dispatched but failed to mark read"
            );
        }
    }

    tracing::info!(
        job = %job.cfg.name,
        message_id = %id,
        subject = %subject,
        "gmail-poller: dispatched"
    );
    Ok(())
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
