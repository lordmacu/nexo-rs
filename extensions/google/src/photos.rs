//! Google Photos Library API (readonly).
//!
//! Scope policy note: Google restricted some Photos Library scopes in 2025 —
//! `photoslibrary.readonly.appcreateddata` can only see media the app
//! uploaded itself. The full `photoslibrary.readonly` scope still returns
//! the user's whole library but goes through Google's verification queue.
//! For personal use in Testing mode the full scope works; document this
//! for operators shipping to prod.

use reqwest::Method;
use serde_json::{json, Value};

use crate::client::{self, bad_input, AuthorizedRequest, GoogleError};

pub fn list_media(page_size: u32, page_token: Option<&str>) -> Result<Value, GoogleError> {
    if !(1..=100).contains(&page_size) {
        return Err(bad_input("page_size must be 1..=100"));
    }
    let url = format!("{}/mediaItems", client::photos_base_url());
    let mut q = vec![("pageSize", page_size.to_string())];
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
    summarize_media(v)
}

pub struct SearchFilters<'a> {
    pub album_id: Option<&'a str>,
    pub page_size: u32,
    pub page_token: Option<&'a str>,
    pub date_from: Option<&'a str>, // "YYYY-MM-DD"
    pub date_to: Option<&'a str>,
    pub content_categories: Vec<String>, // e.g. ["LANDSCAPES","FOOD","SELFIES"]
    pub media_types: Vec<String>,        // "PHOTO"|"VIDEO"|"ALL_MEDIA"
    pub favorites_only: bool,
    pub include_archived: bool,
}

pub fn search(filters: SearchFilters<'_>) -> Result<Value, GoogleError> {
    if !(1..=100).contains(&filters.page_size) {
        return Err(bad_input("page_size must be 1..=100"));
    }
    // Album + filters are mutually exclusive per the API.
    let uses_filters = filters.date_from.is_some()
        || filters.date_to.is_some()
        || !filters.content_categories.is_empty()
        || !filters.media_types.is_empty()
        || filters.favorites_only
        || filters.include_archived;
    if filters.album_id.is_some() && uses_filters {
        return Err(bad_input(
            "album_id cannot be combined with date/content/media filters",
        ));
    }

    let url = format!("{}/mediaItems:search", client::photos_base_url());
    let mut body = serde_json::Map::new();
    body.insert("pageSize".into(), json!(filters.page_size));
    if let Some(tok) = filters.page_token {
        body.insert("pageToken".into(), json!(tok));
    }
    if let Some(album_id) = filters.album_id {
        body.insert("albumId".into(), json!(album_id));
    } else if uses_filters {
        let mut f = serde_json::Map::new();
        if let (Some(from), Some(to)) = (filters.date_from, filters.date_to) {
            let (from_y, from_m, from_d) = parse_ymd(from)?;
            let (to_y, to_m, to_d) = parse_ymd(to)?;
            f.insert(
                "dateFilter".into(),
                json!({
                    "ranges": [{
                        "startDate": { "year": from_y, "month": from_m, "day": from_d },
                        "endDate":   { "year": to_y,   "month": to_m,   "day": to_d   },
                    }]
                }),
            );
        } else if filters.date_from.is_some() || filters.date_to.is_some() {
            return Err(bad_input(
                "date_from and date_to must both be set together",
            ));
        }
        if !filters.content_categories.is_empty() {
            f.insert(
                "contentFilter".into(),
                json!({ "includedContentCategories": filters.content_categories }),
            );
        }
        if !filters.media_types.is_empty() {
            let valid: Vec<String> = filters
                .media_types
                .iter()
                .map(|m| m.to_ascii_uppercase())
                .collect();
            for t in &valid {
                if !["PHOTO", "VIDEO", "ALL_MEDIA"].contains(&t.as_str()) {
                    return Err(bad_input(format!("unknown media_type `{t}`")));
                }
            }
            f.insert("mediaTypeFilter".into(), json!({ "mediaTypes": valid }));
        }
        if filters.favorites_only {
            f.insert(
                "featureFilter".into(),
                json!({ "includedFeatures": ["FAVORITES"] }),
            );
        }
        if filters.include_archived {
            f.insert("includeArchivedMedia".into(), json!(true));
        }
        body.insert("filters".into(), Value::Object(f));
    }

    let v = client::call(AuthorizedRequest {
        method: Method::POST,
        url: &url,
        query: &[],
        body: Some(Value::Object(body)),
    })?
    .unwrap_or_else(|| json!({}));
    summarize_media(v)
}

pub fn get_media(id: &str) -> Result<Value, GoogleError> {
    if id.trim().is_empty() {
        return Err(bad_input("`id` cannot be empty"));
    }
    let url = format!("{}/mediaItems/{}", client::photos_base_url(), id);
    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &[],
        body: None,
    })?
    .unwrap_or_else(|| json!({}));
    Ok(json!({ "ok": true, "media": render_media_item(v) }))
}

pub fn list_albums(
    page_size: u32,
    page_token: Option<&str>,
    exclude_non_app_created: bool,
) -> Result<Value, GoogleError> {
    if !(1..=50).contains(&page_size) {
        return Err(bad_input("page_size must be 1..=50"));
    }
    let url = format!("{}/albums", client::photos_base_url());
    let mut q = vec![("pageSize", page_size.to_string())];
    if let Some(t) = page_token {
        q.push(("pageToken", t.to_string()));
    }
    if exclude_non_app_created {
        q.push(("excludeNonAppCreatedData", "true".into()));
    }
    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &q,
        body: None,
    })?
    .unwrap_or_else(|| json!({}));
    let items = v
        .get("albums")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let albums: Vec<Value> = items
        .into_iter()
        .map(|a| {
            json!({
                "id": a.get("id"),
                "title": a.get("title"),
                "media_items_count": a.get("mediaItemsCount"),
                "cover_media_base_url": a.get("coverPhotoBaseUrl"),
                "product_url": a.get("productUrl"),
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "count": albums.len(),
        "albums": albums,
        "next_page_token": v.get("nextPageToken"),
    }))
}

fn summarize_media(v: Value) -> Result<Value, GoogleError> {
    let items = v
        .get("mediaItems")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let media: Vec<Value> = items.into_iter().map(render_media_item).collect();
    Ok(json!({
        "ok": true,
        "count": media.len(),
        "media": media,
        "next_page_token": v.get("nextPageToken"),
    }))
}

fn render_media_item(m: Value) -> Value {
    json!({
        "id": m.get("id"),
        "description": m.get("description"),
        "base_url": m.get("baseUrl"),
        "product_url": m.get("productUrl"),
        "mime_type": m.get("mimeType"),
        "filename": m.get("filename"),
        "media_metadata": m.get("mediaMetadata"),
    })
}

fn parse_ymd(s: &str) -> Result<(u32, u32, u32), GoogleError> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err(bad_input(format!("date `{s}` must be YYYY-MM-DD")));
    }
    let y: u32 = parts[0].parse().map_err(|_| bad_input("bad year"))?;
    let m: u32 = parts[1].parse().map_err(|_| bad_input("bad month"))?;
    let d: u32 = parts[2].parse().map_err(|_| bad_input("bad day"))?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1900..=2200).contains(&y) {
        return Err(bad_input(format!("date `{s}` out of range")));
    }
    Ok((y, m, d))
}
