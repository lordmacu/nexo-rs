use std::path::PathBuf;

use serde_json::{json, Value};

use crate::calendar::{self, EventDraft, ListEventsParams};
use crate::client::{self as gclient, bad_input, GoogleError};
use crate::drive::{self, UploadParams};
use crate::gmail::{self, ListParams};
use crate::oauth;
use crate::people::{self, CreateDraft};
use crate::photos::{self, SearchFilters};
use crate::tasks::{self, ListTasksParams, TaskDraft};

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Credential presence, endpoints, write-flag state.",
          "input_schema":{"type":"object","additionalProperties":false}},

        { "name":"gmail_list",
          "description":"List messages (ids + thread_ids). Supports Gmail query language in `query`.",
          "input_schema":{"type":"object","properties":{
              "query":{"type":"string"},
              "label_ids":{"type":"string","description":"comma-separated"},
              "max_results":{"type":"integer","minimum":1,"maximum":500,"description":"default 20"},
              "include_spam_trash":{"type":"boolean"},
              "page_token":{"type":"string"}
          },"additionalProperties":false}},
        { "name":"gmail_read",
          "description":"Fetch a full message by id. Returns headers, decoded body_text, labels, and raw envelope.",
          "input_schema":{"type":"object","properties":{
              "id":{"type":"string"},
              "format":{"type":"string","enum":["full","metadata","minimal","raw"],"description":"default full"}
          },"required":["id"],"additionalProperties":false}},
        { "name":"gmail_search",
          "description":"Alias for gmail_list with a required `query` (Gmail search syntax: `from:`, `to:`, `subject:`, `is:unread`, `newer_than:7d`, ...).",
          "input_schema":{"type":"object","properties":{
              "query":{"type":"string"},
              "max_results":{"type":"integer","minimum":1,"maximum":500}
          },"required":["query"],"additionalProperties":false}},
        { "name":"gmail_send",
          "description":"Send a plain-text email. Requires gmail.send scope AND GOOGLE_ALLOW_SEND=true env flag.",
          "input_schema":{"type":"object","properties":{
              "to":{"type":"string"},
              "subject":{"type":"string"},
              "body":{"type":"string"}
          },"required":["to","subject","body"],"additionalProperties":false}},
        { "name":"gmail_modify_labels",
          "description":"Add/remove labels. Common: mark_read (remove UNREAD), archive (remove INBOX), trash (add TRASH). Requires GOOGLE_ALLOW_SEND=true.",
          "input_schema":{"type":"object","properties":{
              "id":{"type":"string"},
              "add_labels":{"type":"array","items":{"type":"string"}},
              "remove_labels":{"type":"array","items":{"type":"string"}}
          },"required":["id"],"additionalProperties":false}},

        { "name":"calendar_list_calendars","description":"List calendars the user has access to.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"calendar_list_events",
          "description":"List events on a calendar. Use RFC 3339 timestamps for time_min/time_max.",
          "input_schema":{"type":"object","properties":{
              "calendar_id":{"type":"string","description":"default `primary`"},
              "time_min":{"type":"string"},
              "time_max":{"type":"string"},
              "q":{"type":"string"},
              "max_results":{"type":"integer","minimum":1,"maximum":2500,"description":"default 25"},
              "single_events":{"type":"boolean","description":"default true"},
              "order_by":{"type":"string","enum":["startTime","updated"]}
          },"additionalProperties":false}},
        { "name":"calendar_create_event",
          "description":"Create an event. Date-only `start`/`end` (YYYY-MM-DD) = all-day event; full RFC 3339 = timed event. Requires GOOGLE_ALLOW_CALENDAR_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "calendar_id":{"type":"string"},
              "summary":{"type":"string"},
              "description":{"type":"string"},
              "location":{"type":"string"},
              "start":{"type":"string"},
              "end":{"type":"string"},
              "time_zone":{"type":"string"},
              "attendees":{"type":"array","items":{"type":"string"}}
          },"required":["summary","start","end"],"additionalProperties":false}},
        { "name":"calendar_update_event",
          "description":"PATCH an event. Requires GOOGLE_ALLOW_CALENDAR_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "calendar_id":{"type":"string"},
              "event_id":{"type":"string"},
              "patch":{"type":"object"}
          },"required":["event_id","patch"],"additionalProperties":false}},
        { "name":"calendar_delete_event",
          "description":"Delete an event. Requires GOOGLE_ALLOW_CALENDAR_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "calendar_id":{"type":"string"},
              "event_id":{"type":"string"}
          },"required":["event_id"],"additionalProperties":false}},

        { "name":"tasks_list_lists","description":"List task lists.",
          "input_schema":{"type":"object","properties":{
              "max_results":{"type":"integer","minimum":1,"maximum":100}
          },"additionalProperties":false}},
        { "name":"tasks_list_tasks","description":"List tasks in a list.",
          "input_schema":{"type":"object","properties":{
              "list_id":{"type":"string"},
              "show_completed":{"type":"boolean"},
              "show_hidden":{"type":"boolean"},
              "max_results":{"type":"integer","minimum":1,"maximum":100}
          },"required":["list_id"],"additionalProperties":false}},
        { "name":"tasks_add","description":"Add a task. Requires GOOGLE_ALLOW_TASKS_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "list_id":{"type":"string"},
              "title":{"type":"string"},
              "notes":{"type":"string"},
              "due":{"type":"string"}
          },"required":["list_id","title"],"additionalProperties":false}},
        { "name":"tasks_complete","description":"Mark a task completed. Requires GOOGLE_ALLOW_TASKS_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "list_id":{"type":"string"},
              "task_id":{"type":"string"}
          },"required":["list_id","task_id"],"additionalProperties":false}},
        { "name":"tasks_delete","description":"Delete a task. Requires GOOGLE_ALLOW_TASKS_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "list_id":{"type":"string"},
              "task_id":{"type":"string"}
          },"required":["list_id","task_id"],"additionalProperties":false}},

        // --- Drive --------------------------------------------------------
        { "name":"drive_list",
          "description":"List files in Drive. Supports Drive query language in `q`, e.g. `mimeType='application/pdf' and 'me' in owners`.",
          "input_schema":{"type":"object","properties":{
              "q":{"type":"string"},
              "page_size":{"type":"integer","minimum":1,"maximum":1000,"description":"default 20"},
              "fields":{"type":"string","description":"override default projection"},
              "page_token":{"type":"string"},
              "spaces":{"type":"string","enum":["drive","appDataFolder","photos"]}
          },"additionalProperties":false}},
        { "name":"drive_get",
          "description":"Get metadata for a single file.",
          "input_schema":{"type":"object","properties":{
              "id":{"type":"string"},
              "fields":{"type":"string"}
          },"required":["id"],"additionalProperties":false}},
        { "name":"drive_download",
          "description":"Download a Drive file's bytes to `output_path` (must lie under GOOGLE_DRIVE_SANDBOX_ROOT, default temp).",
          "input_schema":{"type":"object","properties":{
              "id":{"type":"string"},
              "output_path":{"type":"string"}
          },"required":["id","output_path"],"additionalProperties":false}},
        { "name":"drive_upload",
          "description":"Upload a local file to Drive. Source must be under GOOGLE_DRIVE_SANDBOX_ROOT. Requires GOOGLE_ALLOW_DRIVE_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "source_path":{"type":"string"},
              "name":{"type":"string","description":"default: source filename"},
              "parent_id":{"type":"string","description":"folder id"},
              "mime_type":{"type":"string","description":"default application/octet-stream"}
          },"required":["source_path"],"additionalProperties":false}},
        { "name":"drive_create_folder",
          "description":"Create a folder. Requires GOOGLE_ALLOW_DRIVE_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "name":{"type":"string"},
              "parent_id":{"type":"string"}
          },"required":["name"],"additionalProperties":false}},
        { "name":"drive_delete",
          "description":"Delete a file (moves to trash if default). Requires GOOGLE_ALLOW_DRIVE_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "id":{"type":"string"}
          },"required":["id"],"additionalProperties":false}},

        // --- People (contacts) -------------------------------------------
        { "name":"contacts_list",
          "description":"List the user's own contacts. Returns display_name, emails, phones, organization, job_title, notes.",
          "input_schema":{"type":"object","properties":{
              "page_size":{"type":"integer","minimum":1,"maximum":1000,"description":"default 50"},
              "sort_order":{"type":"string","enum":["LAST_MODIFIED_ASCENDING","LAST_MODIFIED_DESCENDING","FIRST_NAME_ASCENDING","LAST_NAME_ASCENDING"]},
              "page_token":{"type":"string"},
              "person_fields":{"type":"string","description":"override default projection"}
          },"additionalProperties":false}},
        { "name":"contacts_search",
          "description":"Fuzzy-search contacts by name/email/phone/organization. Use this first when the user mentions someone by name; pick the right resource_name from the result and use contacts_get or email directly.",
          "input_schema":{"type":"object","properties":{
              "query":{"type":"string"},
              "page_size":{"type":"integer","minimum":1,"maximum":30,"description":"default 10"}
          },"required":["query"],"additionalProperties":false}},
        { "name":"contacts_get",
          "description":"Get a single contact by resource_name (format `people/c1234567890`).",
          "input_schema":{"type":"object","properties":{
              "resource_name":{"type":"string"},
              "person_fields":{"type":"string"}
          },"required":["resource_name"],"additionalProperties":false}},
        { "name":"contacts_other_list",
          "description":"List `Other Contacts` — people the user emailed but never explicitly saved. Useful when a search against the main book returns nothing.",
          "input_schema":{"type":"object","properties":{
              "page_size":{"type":"integer","minimum":1,"maximum":1000},
              "page_token":{"type":"string"},
              "read_mask":{"type":"string"}
          },"additionalProperties":false}},
        { "name":"contacts_create",
          "description":"Create a contact. Requires GOOGLE_ALLOW_CONTACTS_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "given_name":{"type":"string"},
              "family_name":{"type":"string"},
              "emails":{"type":"array","items":{"type":"object","properties":{
                  "label":{"type":"string","description":"e.g. home|work|other"},
                  "value":{"type":"string"}
              },"required":["value"]}},
              "phones":{"type":"array","items":{"type":"object","properties":{
                  "label":{"type":"string","description":"mobile|home|work"},
                  "value":{"type":"string"}
              },"required":["value"]}},
              "organization":{"type":"string"},
              "job_title":{"type":"string"},
              "notes":{"type":"string"}
          },"required":["given_name"],"additionalProperties":false}},
        { "name":"contacts_update",
          "description":"PATCH a contact. `update_fields` must list which personFields you are modifying (e.g. ['emailAddresses','phoneNumbers']). Requires GOOGLE_ALLOW_CONTACTS_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "resource_name":{"type":"string"},
              "patch":{"type":"object","description":"People API Person resource fragment"},
              "update_fields":{"type":"array","items":{"type":"string"}}
          },"required":["resource_name","patch","update_fields"],"additionalProperties":false}},
        { "name":"contacts_delete",
          "description":"Delete a contact. Requires GOOGLE_ALLOW_CONTACTS_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "resource_name":{"type":"string"}
          },"required":["resource_name"],"additionalProperties":false}},

        // --- Photos (readonly) -------------------------------------------
        { "name":"photos_list_media",
          "description":"List the user's recent photos/videos. Returns id, base_url, filename, mime_type, media_metadata.",
          "input_schema":{"type":"object","properties":{
              "page_size":{"type":"integer","minimum":1,"maximum":100,"description":"default 25"},
              "page_token":{"type":"string"}
          },"additionalProperties":false}},
        { "name":"photos_search",
          "description":"Filtered search. Combine date_from+date_to, content_categories, media_types, favorites. album_id is EXCLUSIVE with other filters.",
          "input_schema":{"type":"object","properties":{
              "album_id":{"type":"string","description":"mutually exclusive with date/content filters"},
              "page_size":{"type":"integer","minimum":1,"maximum":100,"description":"default 25"},
              "page_token":{"type":"string"},
              "date_from":{"type":"string","description":"YYYY-MM-DD"},
              "date_to":{"type":"string","description":"YYYY-MM-DD"},
              "content_categories":{"type":"array","items":{"type":"string"},"description":"e.g. LANDSCAPES, FOOD, SELFIES, SPORT, PETS, PEOPLE, CITYSCAPES"},
              "media_types":{"type":"array","items":{"type":"string","enum":["PHOTO","VIDEO","ALL_MEDIA"]}},
              "favorites_only":{"type":"boolean"},
              "include_archived":{"type":"boolean"}
          },"additionalProperties":false}},
        { "name":"photos_get_media",
          "description":"Get a single media item by id (full metadata + fresh base_url).",
          "input_schema":{"type":"object","properties":{
              "id":{"type":"string"}
          },"required":["id"],"additionalProperties":false}},
        { "name":"photos_list_albums",
          "description":"List the user's albums.",
          "input_schema":{"type":"object","properties":{
              "page_size":{"type":"integer","minimum":1,"maximum":50,"description":"default 20"},
              "page_token":{"type":"string"},
              "exclude_non_app_created":{"type":"boolean","description":"default false"}
          },"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError {
    pub code: i32,
    pub message: String,
}

impl From<GoogleError> for ToolError {
    fn from(e: GoogleError) -> Self {
        Self { code: e.rpc_code(), message: e.message() }
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "gmail_list" => gmail_list(args),
        "gmail_read" => gmail_read(args),
        "gmail_search" => gmail_search(args),
        "gmail_send" => gmail_send(args),
        "gmail_modify_labels" => gmail_modify_labels(args),
        "calendar_list_calendars" => Ok(calendar::list_calendars()?),
        "calendar_list_events" => calendar_list_events(args),
        "calendar_create_event" => calendar_create_event(args),
        "calendar_update_event" => calendar_update_event(args),
        "calendar_delete_event" => calendar_delete_event(args),
        "tasks_list_lists" => tasks_list_lists(args),
        "tasks_list_tasks" => tasks_list_tasks(args),
        "tasks_add" => tasks_add(args),
        "tasks_complete" => tasks_complete(args),
        "tasks_delete" => tasks_delete(args),
        "drive_list" => drive_list(args),
        "drive_get" => drive_get(args),
        "drive_download" => drive_download(args),
        "drive_upload" => drive_upload(args),
        "drive_create_folder" => drive_create_folder(args),
        "drive_delete" => drive_delete(args),
        "contacts_list" => contacts_list(args),
        "contacts_search" => contacts_search(args),
        "contacts_get" => contacts_get(args),
        "contacts_other_list" => contacts_other_list(args),
        "contacts_create" => contacts_create(args),
        "contacts_update" => contacts_update(args),
        "contacts_delete" => contacts_delete(args),
        "photos_list_media" => photos_list_media(args),
        "photos_search" => photos_search(args),
        "photos_get_media" => photos_get_media(args),
        "photos_list_albums" => photos_list_albums(args),
        other => Err(ToolError { code: -32601, message: format!("unknown tool `{other}`") }),
    }
}

// ---- env gates -------------------------------------------------------------

const ENV_SEND: &str = "GOOGLE_ALLOW_SEND";
const ENV_CAL_WRITE: &str = "GOOGLE_ALLOW_CALENDAR_WRITE";
const ENV_TASKS_WRITE: &str = "GOOGLE_ALLOW_TASKS_WRITE";
const ENV_DRIVE_WRITE: &str = "GOOGLE_ALLOW_DRIVE_WRITE";
const ENV_CONTACTS_WRITE: &str = "GOOGLE_ALLOW_CONTACTS_WRITE";

fn flag_on(env_key: &str) -> bool {
    matches!(
        std::env::var(env_key).ok().map(|s| s.trim().to_ascii_lowercase()),
        Some(ref s) if s == "true" || s == "1" || s == "yes"
    )
}

fn require_flag(flag: &'static str) -> Result<(), ToolError> {
    if flag_on(flag) {
        Ok(())
    } else {
        Err(ToolError {
            code: -32043,
            message: format!(
                "write denied: set {flag}=true on the agent process to allow this action"
            ),
        })
    }
}

// ---- status ---------------------------------------------------------------

fn status() -> Value {
    json!({
        "ok": oauth::has_credentials(),
        "provider": "google-oauth-user",
        "client_version": crate::client::CLIENT_VERSION,
        "user_agent": crate::client::user_agent(),
        "credentials_present": oauth::has_credentials(),
        "endpoints": {
            "token": oauth::token_endpoint(),
            "gmail": crate::client::gmail_base_url(),
            "calendar": crate::client::calendar_base_url(),
            "tasks": crate::client::tasks_base_url(),
        },
        "write_flags": {
            "gmail_send": flag_on(ENV_SEND),
            "calendar_write": flag_on(ENV_CAL_WRITE),
            "tasks_write": flag_on(ENV_TASKS_WRITE),
            "drive_write": flag_on(ENV_DRIVE_WRITE),
            "contacts_write": flag_on(ENV_CONTACTS_WRITE),
        },
        "tools_count": 33,
        "requires": {
            "bins": [],
            "env": ["GOOGLE_CLIENT_ID","GOOGLE_CLIENT_SECRET","GOOGLE_REFRESH_TOKEN"]
        }
    })
}

// ---- gmail ----------------------------------------------------------------

fn gmail_list(args: &Value) -> Result<Value, ToolError> {
    let max_results = args.get("max_results").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(20);
    let query_s = optional_string(args, "query");
    let label_s = optional_string(args, "label_ids");
    let page_s = optional_string(args, "page_token");
    let include_spam_trash = args.get("include_spam_trash").and_then(|v| v.as_bool()).unwrap_or(false);
    if max_results == 0 {
        return Err(bad_input("max_results must be >= 1").into());
    }
    Ok(gmail::list_messages(ListParams {
        query: query_s.as_deref(),
        label_ids: label_s.as_deref(),
        max_results,
        include_spam_trash,
        page_token: page_s.as_deref(),
    })?)
}

fn gmail_read(args: &Value) -> Result<Value, ToolError> {
    let id = required_string(args, "id")?;
    let format = optional_string(args, "format").unwrap_or_else(|| "full".into());
    Ok(gmail::read_message(&id, &format)?)
}

fn gmail_search(args: &Value) -> Result<Value, ToolError> {
    let query = required_string(args, "query")?;
    let max_results = args.get("max_results").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(20);
    Ok(gmail::list_messages(ListParams {
        query: Some(&query),
        label_ids: None,
        max_results,
        include_spam_trash: false,
        page_token: None,
    })?)
}

fn gmail_send(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_SEND)?;
    let to = required_string(args, "to")?;
    let subject = required_string(args, "subject")?;
    let body = required_string(args, "body")?;
    Ok(gmail::send_message(&to, &subject, &body)?)
}

fn gmail_modify_labels(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_SEND)?;
    let id = required_string(args, "id")?;
    let add = string_array(args, "add_labels");
    let remove = string_array(args, "remove_labels");
    Ok(gmail::modify_labels(&id, add, remove)?)
}

// ---- calendar -------------------------------------------------------------

fn calendar_list_events(args: &Value) -> Result<Value, ToolError> {
    let calendar_id = optional_string(args, "calendar_id").unwrap_or_else(|| "primary".into());
    let time_min = optional_string(args, "time_min");
    let time_max = optional_string(args, "time_max");
    let q = optional_string(args, "q");
    let order_by = optional_string(args, "order_by");
    let max_results = args.get("max_results").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(25);
    let single_events = args.get("single_events").and_then(|v| v.as_bool()).unwrap_or(true);
    Ok(calendar::list_events(ListEventsParams {
        calendar_id: &calendar_id,
        time_min: time_min.as_deref(),
        time_max: time_max.as_deref(),
        max_results,
        q: q.as_deref(),
        single_events,
        order_by: order_by.as_deref(),
    })?)
}

fn calendar_create_event(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_CAL_WRITE)?;
    let calendar_id = optional_string(args, "calendar_id").unwrap_or_else(|| "primary".into());
    let summary = required_string(args, "summary")?;
    let start = required_string(args, "start")?;
    let end = required_string(args, "end")?;
    let description = optional_string(args, "description");
    let location = optional_string(args, "location");
    let time_zone = optional_string(args, "time_zone");
    let attendees = string_array(args, "attendees");
    Ok(calendar::create_event(EventDraft {
        calendar_id: &calendar_id,
        summary: &summary,
        description: description.as_deref(),
        location: location.as_deref(),
        start: &start,
        end: &end,
        time_zone: time_zone.as_deref(),
        attendees,
    })?)
}

fn calendar_update_event(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_CAL_WRITE)?;
    let calendar_id = optional_string(args, "calendar_id").unwrap_or_else(|| "primary".into());
    let event_id = required_string(args, "event_id")?;
    let patch = args.get("patch").cloned().ok_or_else(|| ToolError::from(bad_input("`patch` is required")))?;
    Ok(calendar::update_event(&calendar_id, &event_id, patch)?)
}

fn calendar_delete_event(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_CAL_WRITE)?;
    let calendar_id = optional_string(args, "calendar_id").unwrap_or_else(|| "primary".into());
    let event_id = required_string(args, "event_id")?;
    Ok(calendar::delete_event(&calendar_id, &event_id)?)
}

// ---- tasks ----------------------------------------------------------------

fn tasks_list_lists(args: &Value) -> Result<Value, ToolError> {
    let max_results = args.get("max_results").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(20);
    Ok(tasks::list_lists(max_results)?)
}

fn tasks_list_tasks(args: &Value) -> Result<Value, ToolError> {
    let list_id = required_string(args, "list_id")?;
    let show_completed = args.get("show_completed").and_then(|v| v.as_bool()).unwrap_or(false);
    let show_hidden = args.get("show_hidden").and_then(|v| v.as_bool()).unwrap_or(false);
    let max_results = args.get("max_results").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(50);
    Ok(tasks::list_tasks(ListTasksParams {
        list_id: &list_id,
        show_completed,
        show_hidden,
        max_results,
    })?)
}

fn tasks_add(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_TASKS_WRITE)?;
    let list_id = required_string(args, "list_id")?;
    let title = required_string(args, "title")?;
    let notes = optional_string(args, "notes");
    let due = optional_string(args, "due");
    Ok(tasks::add_task(TaskDraft {
        list_id: &list_id,
        title: &title,
        notes: notes.as_deref(),
        due: due.as_deref(),
    })?)
}

fn tasks_complete(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_TASKS_WRITE)?;
    let list_id = required_string(args, "list_id")?;
    let task_id = required_string(args, "task_id")?;
    Ok(tasks::complete_task(&list_id, &task_id)?)
}

fn tasks_delete(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_TASKS_WRITE)?;
    let list_id = required_string(args, "list_id")?;
    let task_id = required_string(args, "task_id")?;
    Ok(tasks::delete_task(&list_id, &task_id)?)
}

// ---- helpers --------------------------------------------------------------

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args.get(key).and_then(|v| v.as_str())
        .ok_or_else(|| ToolError { code: -32602, message: format!("missing or invalid `{key}`") })?
        .trim().to_string();
    if s.is_empty() {
        return Err(ToolError { code: -32602, message: format!("`{key}` cannot be empty") });
    }
    Ok(s)
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

// ---- drive ----------------------------------------------------------------

fn drive_list(args: &Value) -> Result<Value, ToolError> {
    let page_size = args.get("page_size").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(20);
    let q = optional_string(args, "q");
    let fields = optional_string(args, "fields");
    let page_token = optional_string(args, "page_token");
    let spaces = optional_string(args, "spaces");
    Ok(drive::list_files(drive::ListParams {
        q: q.as_deref(),
        page_size,
        fields: fields.as_deref(),
        page_token: page_token.as_deref(),
        spaces: spaces.as_deref(),
    })?)
}

fn drive_get(args: &Value) -> Result<Value, ToolError> {
    let id = required_string(args, "id")?;
    let fields = optional_string(args, "fields");
    Ok(drive::get_metadata(&id, fields.as_deref())?)
}

fn drive_download(args: &Value) -> Result<Value, ToolError> {
    let id = required_string(args, "id")?;
    let raw_path = required_string(args, "output_path")?;
    let sandboxed: PathBuf = gclient::sandbox_drive_path(std::path::Path::new(&raw_path))?;
    Ok(drive::download(&id, sandboxed)?)
}

fn drive_upload(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_DRIVE_WRITE)?;
    let raw_path = required_string(args, "source_path")?;
    let src = gclient::sandbox_drive_path(std::path::Path::new(&raw_path))?;
    let name = optional_string(args, "name");
    let parent_id = optional_string(args, "parent_id");
    let mime_type = optional_string(args, "mime_type");
    Ok(drive::upload(UploadParams {
        source_path: src,
        name: name.as_deref(),
        parent_id: parent_id.as_deref(),
        mime_type: mime_type.as_deref(),
    })?)
}

fn drive_create_folder(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_DRIVE_WRITE)?;
    let name = required_string(args, "name")?;
    let parent_id = optional_string(args, "parent_id");
    Ok(drive::create_folder(&name, parent_id.as_deref())?)
}

fn drive_delete(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_DRIVE_WRITE)?;
    let id = required_string(args, "id")?;
    Ok(drive::delete_file(&id)?)
}

// ---- people (contacts) ----------------------------------------------------

fn contacts_list(args: &Value) -> Result<Value, ToolError> {
    let page_size = args.get("page_size").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(50);
    let sort_order = optional_string(args, "sort_order");
    let page_token = optional_string(args, "page_token");
    let person_fields = optional_string(args, "person_fields");
    Ok(people::list_connections(
        page_size,
        sort_order.as_deref(),
        page_token.as_deref(),
        person_fields.as_deref(),
    )?)
}

fn contacts_search(args: &Value) -> Result<Value, ToolError> {
    let query = required_string(args, "query")?;
    let page_size = args.get("page_size").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(10);
    Ok(people::search(&query, page_size)?)
}

fn contacts_get(args: &Value) -> Result<Value, ToolError> {
    let resource_name = required_string(args, "resource_name")?;
    let person_fields = optional_string(args, "person_fields");
    Ok(people::get_person(&resource_name, person_fields.as_deref())?)
}

fn contacts_other_list(args: &Value) -> Result<Value, ToolError> {
    let page_size = args.get("page_size").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(50);
    let page_token = optional_string(args, "page_token");
    let read_mask = optional_string(args, "read_mask");
    Ok(people::list_other_contacts(
        page_size,
        page_token.as_deref(),
        read_mask.as_deref(),
    )?)
}

fn contacts_create(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_CONTACTS_WRITE)?;
    let given_name = required_string(args, "given_name")?;
    let family_name = optional_string(args, "family_name");
    let organization = optional_string(args, "organization");
    let job_title = optional_string(args, "job_title");
    let notes = optional_string(args, "notes");
    let emails = parse_labeled_array(args, "emails");
    let phones = parse_labeled_array(args, "phones");
    Ok(people::create(CreateDraft {
        given_name: &given_name,
        family_name: family_name.as_deref(),
        emails,
        phones,
        organization: organization.as_deref(),
        job_title: job_title.as_deref(),
        notes: notes.as_deref(),
    })?)
}

fn contacts_update(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_CONTACTS_WRITE)?;
    let resource_name = required_string(args, "resource_name")?;
    let patch = args
        .get("patch")
        .cloned()
        .ok_or_else(|| ToolError::from(bad_input("`patch` is required")))?;
    let update_fields = string_array(args, "update_fields");
    Ok(people::update(&resource_name, patch, update_fields)?)
}

fn contacts_delete(args: &Value) -> Result<Value, ToolError> {
    require_flag(ENV_CONTACTS_WRITE)?;
    let resource_name = required_string(args, "resource_name")?;
    Ok(people::delete(&resource_name)?)
}

fn parse_labeled_array(args: &Value, key: &str) -> Vec<(String, String)> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let value = v.get("value").and_then(|s| s.as_str())?.trim();
                    if value.is_empty() {
                        return None;
                    }
                    let label = v.get("label").and_then(|s| s.as_str()).unwrap_or("other");
                    Some((label.to_string(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---- photos --------------------------------------------------------------

fn photos_list_media(args: &Value) -> Result<Value, ToolError> {
    let page_size = args.get("page_size").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(25);
    let page_token = optional_string(args, "page_token");
    Ok(photos::list_media(page_size, page_token.as_deref())?)
}

fn photos_search(args: &Value) -> Result<Value, ToolError> {
    let page_size = args.get("page_size").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(25);
    let album_id = optional_string(args, "album_id");
    let page_token = optional_string(args, "page_token");
    let date_from = optional_string(args, "date_from");
    let date_to = optional_string(args, "date_to");
    let content_categories = string_array(args, "content_categories");
    let media_types = string_array(args, "media_types");
    let favorites_only = args.get("favorites_only").and_then(|v| v.as_bool()).unwrap_or(false);
    let include_archived = args.get("include_archived").and_then(|v| v.as_bool()).unwrap_or(false);
    Ok(photos::search(SearchFilters {
        album_id: album_id.as_deref(),
        page_size,
        page_token: page_token.as_deref(),
        date_from: date_from.as_deref(),
        date_to: date_to.as_deref(),
        content_categories,
        media_types,
        favorites_only,
        include_archived,
    })?)
}

fn photos_get_media(args: &Value) -> Result<Value, ToolError> {
    let id = required_string(args, "id")?;
    Ok(photos::get_media(&id)?)
}

fn photos_list_albums(args: &Value) -> Result<Value, ToolError> {
    let page_size = args.get("page_size").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(20);
    let page_token = optional_string(args, "page_token");
    let exclude_non_app_created = args
        .get("exclude_non_app_created")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(photos::list_albums(
        page_size,
        page_token.as_deref(),
        exclude_non_app_created,
    )?)
}
