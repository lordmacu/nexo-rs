//! Google Tasks v1 — task lists and task items.

use reqwest::Method;
use serde_json::{json, Value};

use crate::client::{self, bad_input, AuthorizedRequest, GoogleError};

pub fn list_lists(max_results: u32) -> Result<Value, GoogleError> {
    if !(1..=100).contains(&max_results) {
        return Err(bad_input("max_results must be 1..=100"));
    }
    let url = format!("{}/users/@me/lists", client::tasks_base_url());
    let q = vec![("maxResults", max_results.to_string())];
    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &q,
        body: None,
    })?
    .unwrap_or_else(|| json!({}));
    let items = v.get("items").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let summary: Vec<Value> = items
        .iter()
        .map(|t| {
            json!({
                "id": t.get("id"),
                "title": t.get("title"),
                "updated": t.get("updated"),
            })
        })
        .collect();
    Ok(json!({ "ok": true, "count": summary.len(), "lists": summary }))
}

pub struct ListTasksParams<'a> {
    pub list_id: &'a str,
    pub show_completed: bool,
    pub show_hidden: bool,
    pub max_results: u32,
}

pub fn list_tasks(params: ListTasksParams<'_>) -> Result<Value, GoogleError> {
    if params.list_id.trim().is_empty() {
        return Err(bad_input("`list_id` cannot be empty"));
    }
    if !(1..=100).contains(&params.max_results) {
        return Err(bad_input("max_results must be 1..=100"));
    }
    let url = format!(
        "{}/lists/{}/tasks",
        client::tasks_base_url(),
        params.list_id
    );
    let mut q = vec![
        ("maxResults", params.max_results.to_string()),
        ("showCompleted", params.show_completed.to_string()),
        ("showHidden", params.show_hidden.to_string()),
    ];
    // Include due dates + deleted flag for operator clarity.
    q.push(("showDeleted", "false".into()));

    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &q,
        body: None,
    })?
    .unwrap_or_else(|| json!({}));
    let items = v.get("items").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let summary: Vec<Value> = items
        .iter()
        .map(|t| {
            json!({
                "id": t.get("id"),
                "title": t.get("title"),
                "notes": t.get("notes"),
                "status": t.get("status"),
                "due": t.get("due"),
                "updated": t.get("updated"),
                "completed": t.get("completed"),
                "parent": t.get("parent"),
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "list_id": params.list_id,
        "count": summary.len(),
        "tasks": summary,
        "next_page_token": v.get("nextPageToken"),
    }))
}

pub struct TaskDraft<'a> {
    pub list_id: &'a str,
    pub title: &'a str,
    pub notes: Option<&'a str>,
    pub due: Option<&'a str>,
}

pub fn add_task(draft: TaskDraft<'_>) -> Result<Value, GoogleError> {
    if draft.title.trim().is_empty() {
        return Err(bad_input("`title` cannot be empty"));
    }
    let url = format!("{}/lists/{}/tasks", client::tasks_base_url(), draft.list_id);
    let mut body = serde_json::Map::new();
    body.insert("title".into(), Value::String(draft.title.to_string()));
    if let Some(n) = draft.notes {
        body.insert("notes".into(), Value::String(n.to_string()));
    }
    if let Some(d) = draft.due {
        body.insert("due".into(), Value::String(d.to_string()));
    }
    let v = client::call(AuthorizedRequest {
        method: Method::POST,
        url: &url,
        query: &[],
        body: Some(Value::Object(body)),
    })?
    .unwrap_or_else(|| json!({}));
    Ok(json!({
        "ok": true,
        "id": v.get("id"),
        "title": v.get("title"),
        "status": v.get("status"),
        "due": v.get("due"),
    }))
}

pub fn complete_task(list_id: &str, task_id: &str) -> Result<Value, GoogleError> {
    if list_id.trim().is_empty() || task_id.trim().is_empty() {
        return Err(bad_input("both `list_id` and `task_id` required"));
    }
    let url = format!(
        "{}/lists/{}/tasks/{}",
        client::tasks_base_url(),
        list_id,
        task_id
    );
    let body = json!({
        "status": "completed",
        "completed": chrono_rfc3339_now(),
    });
    let v = client::call(AuthorizedRequest {
        method: Method::PATCH,
        url: &url,
        query: &[],
        body: Some(body),
    })?
    .unwrap_or_else(|| json!({}));
    Ok(json!({ "ok": true, "id": v.get("id"), "status": v.get("status") }))
}

pub fn delete_task(list_id: &str, task_id: &str) -> Result<Value, GoogleError> {
    if list_id.trim().is_empty() || task_id.trim().is_empty() {
        return Err(bad_input("both `list_id` and `task_id` required"));
    }
    let url = format!(
        "{}/lists/{}/tasks/{}",
        client::tasks_base_url(),
        list_id,
        task_id
    );
    client::call(AuthorizedRequest {
        method: Method::DELETE,
        url: &url,
        query: &[],
        body: None,
    })?;
    Ok(json!({ "ok": true, "deleted": task_id }))
}

/// Tiny RFC 3339 "now" without pulling chrono as a dep. Uses system time.
fn chrono_rfc3339_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Convert to rough UTC ISO-8601. Tasks API accepts any RFC 3339; we just
    // need a stable timestamp. Full formatting below:
    // seconds → y-m-d h:m:s via a bounded algorithm.
    format_unix_as_rfc3339(secs)
}

fn format_unix_as_rfc3339(mut secs: u64) -> String {
    let s = (secs % 60) as u32; secs /= 60;
    let m = (secs % 60) as u32; secs /= 60;
    let h = (secs % 24) as u32; let mut days = (secs / 24) as i64;
    // Days since epoch → date (Howard Hinnant's "days from civil" inverse).
    days += 719_468;
    let era = if days >= 0 { days / 146_097 } else { (days - 146_096) / 146_097 };
    let doe = (days - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (y + if mo <= 2 { 1 } else { 0 }) as i64;
    format!("{year:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}
