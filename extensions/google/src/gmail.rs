//! Gmail API — list, read, search, send, modify_labels.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Method;
use serde_json::{json, Value};

use crate::client::{self, bad_input, AuthorizedRequest, GoogleError};

pub struct ListParams<'a> {
    pub query: Option<&'a str>,
    pub label_ids: Option<&'a str>, // comma-separated list
    pub max_results: u32,
    pub include_spam_trash: bool,
    pub page_token: Option<&'a str>,
}

/// Thin wrapper over GET /users/me/messages. The Gmail API returns only
/// `{id, threadId}` per message in this endpoint; the caller fetches full
/// payloads with a subsequent read tool (not in this piloto).
pub fn list_messages(params: ListParams<'_>) -> Result<Value, GoogleError> {
    if !(1..=500).contains(&params.max_results) {
        return Err(bad_input("max_results must be 1..=500"));
    }
    let url = format!("{}/users/me/messages", client::gmail_base_url());
    let mut query: Vec<(&str, String)> = Vec::new();
    query.push(("maxResults", params.max_results.to_string()));
    if let Some(q) = params.query {
        query.push(("q", q.to_string()));
    }
    if let Some(labels) = params.label_ids {
        for l in labels.split(',') {
            let trimmed = l.trim();
            if !trimmed.is_empty() {
                query.push(("labelIds", trimmed.to_string()));
            }
        }
    }
    if params.include_spam_trash {
        query.push(("includeSpamTrash", "true".into()));
    }
    if let Some(tok) = params.page_token {
        query.push(("pageToken", tok.to_string()));
    }

    let resp = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &query,
        body: None,
    })?;

    let v = resp.unwrap_or_else(|| json!({}));
    let messages = v
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    let summary: Vec<Value> = messages
        .iter()
        .map(|m| {
            json!({
                "id": m.get("id"),
                "thread_id": m.get("threadId"),
            })
        })
        .collect();

    Ok(json!({
        "ok": true,
        "count": summary.len(),
        "messages": summary,
        "next_page_token": v.get("nextPageToken"),
        "estimate_total": v.get("resultSizeEstimate"),
    }))
}

/// GET /users/me/messages/{id}?format=full — returns the full Gmail message
/// including headers and decoded body parts.
pub fn read_message(id: &str, format: &str) -> Result<Value, GoogleError> {
    if id.trim().is_empty() {
        return Err(bad_input("`id` cannot be empty"));
    }
    if !["full", "metadata", "minimal", "raw"].contains(&format) {
        return Err(bad_input("`format` must be full|metadata|minimal|raw"));
    }
    let url = format!("{}/users/me/messages/{}", client::gmail_base_url(), id);
    let q = vec![("format", format.to_string())];
    let resp = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &q,
        body: None,
    })?;
    let v = resp.ok_or_else(|| GoogleError::InvalidJson("empty read response".into()))?;

    // Flatten the most useful pieces so the LLM doesn't have to traverse the
    // Gmail envelope every time. Preserve the raw payload under `_raw` for
    // escape-hatch use.
    let headers_map = v
        .pointer("/payload/headers")
        .and_then(|h| h.as_array())
        .map(|arr| {
            let mut m = serde_json::Map::new();
            for h in arr {
                if let (Some(name), Some(value)) = (
                    h.get("name").and_then(|v| v.as_str()),
                    h.get("value").and_then(|v| v.as_str()),
                ) {
                    m.insert(name.to_ascii_lowercase(), Value::String(value.to_string()));
                }
            }
            Value::Object(m)
        })
        .unwrap_or(Value::Object(Default::default()));

    let body_text = extract_body_text(&v);

    Ok(json!({
        "ok": true,
        "id": v.get("id"),
        "thread_id": v.get("threadId"),
        "snippet": v.get("snippet"),
        "labels": v.get("labelIds"),
        "internal_date_ms": v.get("internalDate"),
        "headers": headers_map,
        "body_text": body_text,
        "_raw": v,
    }))
}

fn extract_body_text(msg: &Value) -> Option<String> {
    // If single part: payload.body.data is base64url-encoded.
    if let Some(data) = msg.pointer("/payload/body/data").and_then(|v| v.as_str()) {
        if let Some(text) = decode_base64_url(data) {
            return Some(text);
        }
    }
    // Multi-part: walk payload.parts looking for text/plain first, then text/html.
    fn walk(parts: &[Value]) -> Option<String> {
        let mut html_fallback: Option<String> = None;
        for p in parts {
            let mime = p.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
            if mime.starts_with("multipart/") {
                if let Some(inner) = p.get("parts").and_then(|v| v.as_array()) {
                    if let Some(t) = walk(inner) {
                        return Some(t);
                    }
                }
                continue;
            }
            let data = p.pointer("/body/data").and_then(|v| v.as_str());
            if let Some(d) = data {
                if let Some(text) = decode_base64_url(d) {
                    if mime == "text/plain" {
                        return Some(text);
                    }
                    if mime == "text/html" && html_fallback.is_none() {
                        html_fallback = Some(text);
                    }
                }
            }
        }
        html_fallback
    }
    msg.pointer("/payload/parts")
        .and_then(|v| v.as_array())
        .and_then(|arr| walk(arr))
}

fn decode_base64_url(s: &str) -> Option<String> {
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

/// POST /users/me/messages/send — sends a plaintext email. Requires the
/// `gmail.send` scope and `GOOGLE_ALLOW_SEND=true`.
pub fn send_message(to: &str, subject: &str, body: &str) -> Result<Value, GoogleError> {
    let to = to.trim();
    let subject = subject.trim();
    if to.is_empty() || !to.contains('@') {
        return Err(bad_input("`to` must be a valid email address"));
    }
    if subject.is_empty() {
        return Err(bad_input("`subject` cannot be empty"));
    }
    // Reject CR/LF in header fields: otherwise a crafted `to` or
    // `subject` can inject additional headers (BCC, From spoofing,
    // etc.) into the RFC 2822 message we hand to Gmail.
    if to.bytes().any(|b| b == b'\r' || b == b'\n') {
        return Err(bad_input("`to` contains a line break"));
    }
    if subject.bytes().any(|b| b == b'\r' || b == b'\n') {
        return Err(bad_input("`subject` contains a line break"));
    }
    // Compose a minimal RFC 2822 message. Google accepts plain `Subject`/`To`
    // headers without elaborate MIME. `body` is after the blank-line
    // separator, so newlines inside it are expected and safe.
    let raw_email = format!(
        "To: {to}\r\nSubject: {subject}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{body}"
    );
    let encoded = URL_SAFE_NO_PAD.encode(raw_email.as_bytes());
    let url = format!("{}/users/me/messages/send", client::gmail_base_url());
    let body = json!({ "raw": encoded });
    let resp = client::call(AuthorizedRequest {
        method: Method::POST,
        url: &url,
        query: &[],
        body: Some(body),
    })?;
    let v = resp.unwrap_or_else(|| json!({}));
    Ok(json!({
        "ok": true,
        "id": v.get("id"),
        "thread_id": v.get("threadId"),
        "labels": v.get("labelIds"),
    }))
}

/// POST /users/me/messages/{id}/modify — add/remove labels. Classic use:
/// mark as read (`removeLabelIds: ["UNREAD"]`), archive (`removeLabelIds:
/// ["INBOX"]`), move to trash (`addLabelIds: ["TRASH"]`).
pub fn modify_labels(
    id: &str,
    add: Vec<String>,
    remove: Vec<String>,
) -> Result<Value, GoogleError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(bad_input("`id` cannot be empty"));
    }
    if add.is_empty() && remove.is_empty() {
        return Err(bad_input("at least one of `add_labels` / `remove_labels` is required"));
    }
    let url = format!("{}/users/me/messages/{}/modify", client::gmail_base_url(), id);
    let body = json!({
        "addLabelIds": add,
        "removeLabelIds": remove,
    });
    let resp = client::call(AuthorizedRequest {
        method: Method::POST,
        url: &url,
        query: &[],
        body: Some(body),
    })?;
    let v = resp.unwrap_or_else(|| json!({}));
    Ok(json!({
        "ok": true,
        "id": v.get("id"),
        "labels": v.get("labelIds"),
    }))
}
