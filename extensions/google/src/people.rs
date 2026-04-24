//! Google People API (contactos).
//!
//! Personal fields de interés en los responses: `names`, `emailAddresses`,
//! `phoneNumbers`, `organizations`, `biographies`, `birthdays`, `addresses`.
//! El campo `resourceName` (`people/c1234567890`) identifica cada contacto
//! y es lo que usan get/update/delete.

use reqwest::Method;
use serde_json::{json, Value};

use crate::client::{self, bad_input, AuthorizedRequest, GoogleError};

/// Proyección default — suficiente para identificar + contactar.
const DEFAULT_PERSON_FIELDS: &str =
    "names,emailAddresses,phoneNumbers,organizations,biographies,metadata";

pub fn list_connections(
    page_size: u32,
    sort_order: Option<&str>,
    page_token: Option<&str>,
    person_fields: Option<&str>,
) -> Result<Value, GoogleError> {
    if !(1..=1000).contains(&page_size) {
        return Err(bad_input("page_size must be 1..=1000"));
    }
    if let Some(order) = sort_order {
        if !matches!(
            order,
            "LAST_MODIFIED_ASCENDING"
                | "LAST_MODIFIED_DESCENDING"
                | "FIRST_NAME_ASCENDING"
                | "LAST_NAME_ASCENDING"
        ) {
            return Err(bad_input(format!("unknown sort_order `{order}`")));
        }
    }
    let url = format!("{}/people/me/connections", client::people_base_url());
    let mut q = vec![
        ("pageSize", page_size.to_string()),
        (
            "personFields",
            person_fields.unwrap_or(DEFAULT_PERSON_FIELDS).to_string(),
        ),
    ];
    if let Some(o) = sort_order {
        q.push(("sortOrder", o.to_string()));
    }
    if let Some(t) = page_token {
        q.push(("pageToken", t.to_string()));
    }
    let v = call_and_summarize(&url, &q, "connections")?;
    Ok(v)
}

pub fn search(query: &str, page_size: u32) -> Result<Value, GoogleError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(bad_input("`query` cannot be empty"));
    }
    if !(1..=30).contains(&page_size) {
        return Err(bad_input("page_size must be 1..=30"));
    }
    // People API search requires you to first "warm" it with an empty query
    // ONCE before it will return results; we keep it simple and just issue
    // the real query — if Google gives zero, it still returns `{}`.
    let url = format!("{}/people:searchContacts", client::people_base_url());
    let q = vec![
        ("query", query.to_string()),
        ("pageSize", page_size.to_string()),
        (
            "readMask",
            DEFAULT_PERSON_FIELDS.to_string(),
        ),
    ];
    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &q,
        body: None,
    })?
    .unwrap_or_else(|| json!({}));
    let results = v
        .get("results")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let people: Vec<Value> = results
        .into_iter()
        .filter_map(|r| r.get("person").cloned())
        .map(render_person)
        .collect();
    Ok(json!({
        "ok": true,
        "query": query,
        "count": people.len(),
        "results": people,
    }))
}

pub fn get_person(resource_name: &str, person_fields: Option<&str>) -> Result<Value, GoogleError> {
    let resource_name = validate_resource_name(resource_name)?;
    let url = format!("{}/{}", client::people_base_url(), resource_name);
    let q = vec![(
        "personFields",
        person_fields.unwrap_or(DEFAULT_PERSON_FIELDS).to_string(),
    )];
    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &q,
        body: None,
    })?
    .unwrap_or_else(|| json!({}));
    Ok(json!({
        "ok": true,
        "person": render_person(v),
    }))
}

pub fn list_other_contacts(
    page_size: u32,
    page_token: Option<&str>,
    read_mask: Option<&str>,
) -> Result<Value, GoogleError> {
    if !(1..=1000).contains(&page_size) {
        return Err(bad_input("page_size must be 1..=1000"));
    }
    let url = format!("{}/otherContacts", client::people_base_url());
    let mut q = vec![
        ("pageSize", page_size.to_string()),
        (
            "readMask",
            read_mask
                .unwrap_or("emailAddresses,names,phoneNumbers,metadata")
                .to_string(),
        ),
    ];
    if let Some(t) = page_token {
        q.push(("pageToken", t.to_string()));
    }
    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &q,
        body: None,
    })?
    .unwrap_or_else(|| json!({}));
    let items = v
        .get("otherContacts")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let people: Vec<Value> = items.into_iter().map(render_person).collect();
    Ok(json!({
        "ok": true,
        "count": people.len(),
        "results": people,
        "next_page_token": v.get("nextPageToken"),
    }))
}

pub struct CreateDraft<'a> {
    pub given_name: &'a str,
    pub family_name: Option<&'a str>,
    pub emails: Vec<(String, String)>,  // (label, value)
    pub phones: Vec<(String, String)>,
    pub organization: Option<&'a str>,
    pub job_title: Option<&'a str>,
    pub notes: Option<&'a str>,
}

pub fn create(draft: CreateDraft<'_>) -> Result<Value, GoogleError> {
    if draft.given_name.trim().is_empty() {
        return Err(bad_input("`given_name` cannot be empty"));
    }
    let url = format!("{}/people:createContact", client::people_base_url());
    let mut body = serde_json::Map::new();
    body.insert(
        "names".into(),
        Value::Array(vec![json!({
            "givenName": draft.given_name,
            "familyName": draft.family_name,
        })]),
    );
    if !draft.emails.is_empty() {
        let arr: Vec<Value> = draft
            .emails
            .iter()
            .map(|(label, value)| json!({ "value": value, "type": label }))
            .collect();
        body.insert("emailAddresses".into(), Value::Array(arr));
    }
    if !draft.phones.is_empty() {
        let arr: Vec<Value> = draft
            .phones
            .iter()
            .map(|(label, value)| json!({ "value": value, "type": label }))
            .collect();
        body.insert("phoneNumbers".into(), Value::Array(arr));
    }
    if let Some(org) = draft.organization {
        body.insert(
            "organizations".into(),
            Value::Array(vec![json!({
                "name": org,
                "title": draft.job_title,
            })]),
        );
    }
    if let Some(notes) = draft.notes {
        body.insert(
            "biographies".into(),
            Value::Array(vec![json!({ "value": notes, "contentType": "TEXT_PLAIN" })]),
        );
    }
    let v = client::call(AuthorizedRequest {
        method: Method::POST,
        url: &url,
        query: &[],
        body: Some(Value::Object(body)),
    })?
    .unwrap_or_else(|| json!({}));
    Ok(json!({ "ok": true, "person": render_person(v) }))
}

pub fn update(
    resource_name: &str,
    patch: Value,
    update_fields: Vec<String>,
) -> Result<Value, GoogleError> {
    let resource_name = validate_resource_name(resource_name)?;
    if update_fields.is_empty() {
        return Err(bad_input(
            "`update_fields` must list the personFields being patched (e.g. ['emailAddresses'])",
        ));
    }
    // Google requires the contact's etag for optimistic locking. We fetch it first.
    let current = get_person(&resource_name, Some("metadata"))?;
    let etag = current
        .pointer("/person/etag")
        .and_then(|v| v.as_str())
        .ok_or_else(|| GoogleError::InvalidJson("no etag in current person".into()))?
        .to_string();

    // Merge the etag into the patch body.
    let mut body = patch.as_object().cloned().unwrap_or_default();
    body.insert("etag".into(), Value::String(etag));

    let url = format!("{}/{}:updateContact", client::people_base_url(), resource_name);
    let q = vec![("updatePersonFields", update_fields.join(","))];
    let v = client::call(AuthorizedRequest {
        method: Method::PATCH,
        url: &url,
        query: &q,
        body: Some(Value::Object(body)),
    })?
    .unwrap_or_else(|| json!({}));
    Ok(json!({ "ok": true, "person": render_person(v) }))
}

pub fn delete(resource_name: &str) -> Result<Value, GoogleError> {
    let resource_name = validate_resource_name(resource_name)?;
    let url = format!("{}/{}:deleteContact", client::people_base_url(), resource_name);
    client::call(AuthorizedRequest {
        method: Method::DELETE,
        url: &url,
        query: &[],
        body: None,
    })?;
    Ok(json!({ "ok": true, "deleted": resource_name }))
}

fn validate_resource_name(raw: &str) -> Result<String, GoogleError> {
    let s = raw.trim();
    if !s.starts_with("people/") && !s.starts_with("otherContacts/") {
        return Err(bad_input(format!(
            "resource_name must start with `people/` or `otherContacts/`: got `{s}`"
        )));
    }
    if s.len() > 200 {
        return Err(bad_input("resource_name too long"));
    }
    Ok(s.to_string())
}

/// Shared summary for list/search/get/other responses. Flattens the most
/// useful fields so the LLM doesn't have to traverse arrays of arrays.
fn render_person(p: Value) -> Value {
    let resource_name = p.get("resourceName").cloned();
    let display_name = p
        .pointer("/names/0/displayName")
        .cloned()
        .or_else(|| {
            let given = p.pointer("/names/0/givenName").and_then(|v| v.as_str()).unwrap_or("");
            let family = p.pointer("/names/0/familyName").and_then(|v| v.as_str()).unwrap_or("");
            let combined = format!("{given} {family}").trim().to_string();
            if combined.is_empty() { None } else { Some(Value::String(combined)) }
        });
    let emails: Vec<Value> = p
        .get("emailAddresses")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            json!({
                "value": e.get("value"),
                "type": e.get("type"),
                "formattedType": e.get("formattedType"),
                "primary": e.pointer("/metadata/primary"),
            })
        })
        .collect();
    let phones: Vec<Value> = p
        .get("phoneNumbers")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|ph| {
            json!({
                "value": ph.get("value"),
                "canonical": ph.get("canonicalForm"),
                "type": ph.get("type"),
            })
        })
        .collect();
    let org = p
        .pointer("/organizations/0/name")
        .cloned()
        .and_then(|v| v.as_str().map(String::from).map(Value::String));
    let title = p
        .pointer("/organizations/0/title")
        .cloned()
        .and_then(|v| v.as_str().map(String::from).map(Value::String));
    let notes = p
        .pointer("/biographies/0/value")
        .cloned()
        .and_then(|v| v.as_str().map(String::from).map(Value::String));
    json!({
        "resource_name": resource_name,
        "display_name": display_name,
        "emails": emails,
        "phones": phones,
        "organization": org,
        "job_title": title,
        "notes": notes,
    })
}

fn call_and_summarize(
    url: &str,
    query: &[(&str, String)],
    items_key: &str,
) -> Result<Value, GoogleError> {
    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url,
        query,
        body: None,
    })?
    .unwrap_or_else(|| json!({}));
    let items = v
        .get(items_key)
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let people: Vec<Value> = items.into_iter().map(render_person).collect();
    Ok(json!({
        "ok": true,
        "count": people.len(),
        "results": people,
        "next_page_token": v.get("nextPageToken"),
        "total_items": v.get("totalItems").or_else(|| v.get("totalPeople")),
    }))
}
