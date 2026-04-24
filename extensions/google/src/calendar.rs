//! Google Calendar v3 — list calendars, list/create/update/delete events.

use reqwest::Method;
use serde_json::{json, Value};

use crate::client::{self, bad_input, AuthorizedRequest, GoogleError};

pub fn list_calendars() -> Result<Value, GoogleError> {
    let url = format!("{}/users/me/calendarList", client::calendar_base_url());
    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &[],
        body: None,
    })?
    .unwrap_or_else(|| json!({}));
    let items = v.get("items").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let out: Vec<Value> = items
        .iter()
        .map(|c| {
            json!({
                "id": c.get("id"),
                "summary": c.get("summary"),
                "primary": c.get("primary"),
                "access_role": c.get("accessRole"),
                "time_zone": c.get("timeZone"),
                "color": c.get("backgroundColor"),
            })
        })
        .collect();
    Ok(json!({ "ok": true, "count": out.len(), "calendars": out }))
}

pub struct ListEventsParams<'a> {
    pub calendar_id: &'a str,
    pub time_min: Option<&'a str>,
    pub time_max: Option<&'a str>,
    pub max_results: u32,
    pub q: Option<&'a str>,
    pub single_events: bool,
    pub order_by: Option<&'a str>,
}

pub fn list_events(params: ListEventsParams<'_>) -> Result<Value, GoogleError> {
    if !(1..=2500).contains(&params.max_results) {
        return Err(bad_input("max_results must be 1..=2500"));
    }
    let url = format!(
        "{}/calendars/{}/events",
        client::calendar_base_url(),
        urlencoding_minimal(params.calendar_id)
    );
    let mut q: Vec<(&str, String)> = vec![("maxResults", params.max_results.to_string())];
    if let Some(t) = params.time_min {
        q.push(("timeMin", t.to_string()));
    }
    if let Some(t) = params.time_max {
        q.push(("timeMax", t.to_string()));
    }
    if let Some(text) = params.q {
        q.push(("q", text.to_string()));
    }
    if params.single_events {
        q.push(("singleEvents", "true".into()));
    }
    if let Some(o) = params.order_by {
        q.push(("orderBy", o.to_string()));
    }
    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &q,
        body: None,
    })?
    .unwrap_or_else(|| json!({}));

    let events = v.get("items").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let summary: Vec<Value> = events
        .iter()
        .map(|e| {
            json!({
                "id": e.get("id"),
                "summary": e.get("summary"),
                "description": e.get("description"),
                "location": e.get("location"),
                "start": e.get("start"),
                "end": e.get("end"),
                "status": e.get("status"),
                "attendees": e.get("attendees"),
                "hangout_link": e.get("hangoutLink"),
                "html_link": e.get("htmlLink"),
                "recurring_event_id": e.get("recurringEventId"),
            })
        })
        .collect();

    Ok(json!({
        "ok": true,
        "calendar_id": params.calendar_id,
        "count": summary.len(),
        "events": summary,
        "next_page_token": v.get("nextPageToken"),
    }))
}

pub struct EventDraft<'a> {
    pub calendar_id: &'a str,
    pub summary: &'a str,
    pub description: Option<&'a str>,
    pub location: Option<&'a str>,
    pub start: &'a str,
    pub end: &'a str,
    pub time_zone: Option<&'a str>,
    pub attendees: Vec<String>,
}

pub fn create_event(draft: EventDraft<'_>) -> Result<Value, GoogleError> {
    if draft.summary.trim().is_empty() {
        return Err(bad_input("`summary` is required"));
    }
    let url = format!(
        "{}/calendars/{}/events",
        client::calendar_base_url(),
        urlencoding_minimal(draft.calendar_id)
    );
    let body = build_event_body(
        Some(draft.summary),
        draft.description,
        draft.location,
        Some(draft.start),
        Some(draft.end),
        draft.time_zone,
        Some(draft.attendees),
    );
    let v = client::call(AuthorizedRequest {
        method: Method::POST,
        url: &url,
        query: &[],
        body: Some(body),
    })?
    .unwrap_or_else(|| json!({}));
    Ok(json!({ "ok": true, "id": v.get("id"), "html_link": v.get("htmlLink"), "status": v.get("status") }))
}

pub fn update_event(
    calendar_id: &str,
    event_id: &str,
    patch: Value,
) -> Result<Value, GoogleError> {
    if event_id.trim().is_empty() {
        return Err(bad_input("`event_id` cannot be empty"));
    }
    let url = format!(
        "{}/calendars/{}/events/{}",
        client::calendar_base_url(),
        urlencoding_minimal(calendar_id),
        urlencoding_minimal(event_id)
    );
    let v = client::call(AuthorizedRequest {
        method: Method::PATCH,
        url: &url,
        query: &[],
        body: Some(patch),
    })?
    .unwrap_or_else(|| json!({}));
    Ok(json!({ "ok": true, "id": v.get("id"), "html_link": v.get("htmlLink"), "status": v.get("status") }))
}

pub fn delete_event(calendar_id: &str, event_id: &str) -> Result<Value, GoogleError> {
    if event_id.trim().is_empty() {
        return Err(bad_input("`event_id` cannot be empty"));
    }
    let url = format!(
        "{}/calendars/{}/events/{}",
        client::calendar_base_url(),
        urlencoding_minimal(calendar_id),
        urlencoding_minimal(event_id)
    );
    client::call(AuthorizedRequest {
        method: Method::DELETE,
        url: &url,
        query: &[],
        body: None,
    })?;
    Ok(json!({ "ok": true, "deleted": event_id }))
}

fn build_event_body(
    summary: Option<&str>,
    description: Option<&str>,
    location: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
    tz: Option<&str>,
    attendees: Option<Vec<String>>,
) -> Value {
    let mut body = serde_json::Map::new();
    if let Some(s) = summary {
        body.insert("summary".into(), Value::String(s.to_string()));
    }
    if let Some(s) = description {
        body.insert("description".into(), Value::String(s.to_string()));
    }
    if let Some(s) = location {
        body.insert("location".into(), Value::String(s.to_string()));
    }
    if let Some(s) = start {
        body.insert("start".into(), start_end_object(s, tz));
    }
    if let Some(s) = end {
        body.insert("end".into(), start_end_object(s, tz));
    }
    if let Some(list) = attendees {
        if !list.is_empty() {
            let arr: Vec<Value> = list
                .into_iter()
                .map(|email| json!({ "email": email }))
                .collect();
            body.insert("attendees".into(), Value::Array(arr));
        }
    }
    Value::Object(body)
}

/// Infer whether the caller passed a date-only (`2026-04-30`) or full RFC
/// 3339 timestamp. Date-only → all-day event shape (`date` key); otherwise
/// `dateTime` + optional `timeZone`.
fn start_end_object(value: &str, tz: Option<&str>) -> Value {
    let looks_date_only = value.len() == 10 && value.chars().nth(4) == Some('-');
    if looks_date_only {
        json!({ "date": value })
    } else {
        let mut m = serde_json::Map::new();
        m.insert("dateTime".into(), Value::String(value.to_string()));
        if let Some(z) = tz {
            m.insert("timeZone".into(), Value::String(z.to_string()));
        }
        Value::Object(m)
    }
}

/// Minimal percent-encoder for path segments. Google accepts most calendar
/// ids verbatim (including the `@` in `foo@group.calendar.google.com`), but
/// we still escape `/`, `?`, `#`, and whitespace for safety.
fn urlencoding_minimal(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for ch in seg.chars() {
        match ch {
            '/' | '?' | '#' | ' ' => {
                let mut buf = [0u8; 4];
                for b in ch.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            _ => out.push(ch),
        }
    }
    out
}
