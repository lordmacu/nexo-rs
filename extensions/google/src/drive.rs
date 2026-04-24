//! Google Drive v3 — list/get/download + upload/create_folder/delete (gated).

use std::path::PathBuf;

use reqwest::Method;
use serde_json::{json, Value};

use crate::client::{self, bad_input, AuthorizedRequest, GoogleError};

pub struct ListParams<'a> {
    pub q: Option<&'a str>,
    pub page_size: u32,
    pub fields: Option<&'a str>,
    pub page_token: Option<&'a str>,
    pub spaces: Option<&'a str>,
}

pub fn list_files(params: ListParams<'_>) -> Result<Value, GoogleError> {
    if !(1..=1000).contains(&params.page_size) {
        return Err(bad_input("page_size must be 1..=1000"));
    }
    let url = format!("{}/files", client::drive_base_url());
    let mut q = vec![
        ("pageSize", params.page_size.to_string()),
        (
            "fields",
            params
                .fields
                .unwrap_or("nextPageToken, files(id,name,mimeType,size,modifiedTime,parents,trashed)")
                .to_string(),
        ),
    ];
    if let Some(filter) = params.q {
        q.push(("q", filter.to_string()));
    }
    if let Some(tok) = params.page_token {
        q.push(("pageToken", tok.to_string()));
    }
    if let Some(s) = params.spaces {
        q.push(("spaces", s.to_string()));
    }
    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &q,
        body: None,
    })?
    .unwrap_or_else(|| json!({}));
    Ok(json!({
        "ok": true,
        "files": v.get("files"),
        "next_page_token": v.get("nextPageToken"),
    }))
}

pub fn get_metadata(id: &str, fields: Option<&str>) -> Result<Value, GoogleError> {
    if id.trim().is_empty() {
        return Err(bad_input("`id` cannot be empty"));
    }
    let url = format!("{}/files/{}", client::drive_base_url(), id);
    let q = vec![(
        "fields",
        fields
            .unwrap_or("id,name,mimeType,size,modifiedTime,parents,trashed,owners,webViewLink")
            .to_string(),
    )];
    let v = client::call(AuthorizedRequest {
        method: Method::GET,
        url: &url,
        query: &q,
        body: None,
    })?
    .unwrap_or_else(|| json!({}));
    Ok(json!({ "ok": true, "file": v }))
}

pub fn download(id: &str, output_path: PathBuf) -> Result<Value, GoogleError> {
    if id.trim().is_empty() {
        return Err(bad_input("`id` cannot be empty"));
    }
    let url = format!("{}/files/{}", client::drive_base_url(), id);
    let (bytes, content_type) =
        client::call_bytes(Method::GET, &url, &[("alt", "media".into())])?;
    std::fs::write(&output_path, &bytes)
        .map_err(|e| GoogleError::Transport(format!("write failed: {e}")))?;
    Ok(json!({
        "ok": true,
        "id": id,
        "output_path": output_path.display().to_string(),
        "bytes": bytes.len(),
        "content_type": content_type,
    }))
}

pub struct UploadParams<'a> {
    pub source_path: PathBuf,
    pub name: Option<&'a str>,
    pub parent_id: Option<&'a str>,
    pub mime_type: Option<&'a str>,
}

pub fn upload(params: UploadParams<'_>) -> Result<Value, GoogleError> {
    if !params.source_path.is_file() {
        return Err(bad_input(format!(
            "`{}` is not a regular file",
            params.source_path.display()
        )));
    }
    let bytes = std::fs::read(&params.source_path)
        .map_err(|e| GoogleError::Transport(format!("read failed: {e}")))?;
    let filename = params
        .name
        .map(|s| s.to_string())
        .or_else(|| {
            params
                .source_path
                .file_name()
                .and_then(|f| f.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "upload.bin".to_string());
    let content_type = params.mime_type.unwrap_or("application/octet-stream");

    let mut meta = serde_json::Map::new();
    meta.insert("name".into(), Value::String(filename.clone()));
    if let Some(p) = params.parent_id {
        meta.insert("parents".into(), Value::Array(vec![Value::String(p.to_string())]));
    }
    let metadata = Value::Object(meta);

    let url = format!("{}/files?uploadType=multipart", client::drive_upload_url());
    let v = client::multipart_upload(&url, &metadata, bytes.clone(), content_type)?
        .unwrap_or_else(|| json!({}));
    Ok(json!({
        "ok": true,
        "id": v.get("id"),
        "name": v.get("name"),
        "mime_type": v.get("mimeType"),
        "bytes_uploaded": bytes.len(),
    }))
}

pub fn create_folder(name: &str, parent_id: Option<&str>) -> Result<Value, GoogleError> {
    if name.trim().is_empty() {
        return Err(bad_input("`name` cannot be empty"));
    }
    let url = format!("{}/files", client::drive_base_url());
    let mut body = serde_json::Map::new();
    body.insert("name".into(), Value::String(name.to_string()));
    body.insert(
        "mimeType".into(),
        Value::String("application/vnd.google-apps.folder".to_string()),
    );
    if let Some(p) = parent_id {
        body.insert("parents".into(), Value::Array(vec![Value::String(p.to_string())]));
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
        "name": v.get("name"),
    }))
}

pub fn delete_file(id: &str) -> Result<Value, GoogleError> {
    if id.trim().is_empty() {
        return Err(bad_input("`id` cannot be empty"));
    }
    let url = format!("{}/files/{}", client::drive_base_url(), id);
    client::call(AuthorizedRequest {
        method: Method::DELETE,
        url: &url,
        query: &[],
        body: None,
    })?;
    Ok(json!({ "ok": true, "deleted": id }))
}
