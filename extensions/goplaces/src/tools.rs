use std::process::Command;

use serde_json::{json, Value};

const DEFAULT_GOOGLE_BASE_URL: &str = "https://places.googleapis.com/v1";
const DEFAULT_OSM_BASE_URL: &str = "https://nominatim.openstreetmap.org";
const DEFAULT_SEARCH_FIELDS: &str =
    "places.id,places.displayName,places.formattedAddress,places.rating,places.userRatingCount";
const DEFAULT_DETAILS_FIELDS: &str =
    "id,displayName,formattedAddress,rating,userRatingCount,googleMapsUri,location";
const DEFAULT_LIMIT: u64 = 5;
const MAX_LIMIT: u64 = 20;
const OSM_USER_AGENT: &str = "proyecto-goplaces-extension/0.1.0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    Google,
    OpenStreetMap,
}

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "status",
            "description": "Returns extension status and provider readiness (Google Places + OpenStreetMap fallback)",
            "input_schema": {
                "type": "object",
                "additionalProperties": false
            }
        },
        {
            "name": "search_text",
            "description": "Search places with provider routing: auto (default), google, or openstreetmap",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Human query like 'coffee near Retiro Madrid'"
                    },
                    "provider": {
                        "type": "string",
                        "description": "auto|google|openstreetmap"
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 20,
                        "description": "Maximum number of places to return"
                    },
                    "language_code": {
                        "type": "string",
                        "description": "Optional language code, e.g. en or es"
                    },
                    "region_code": {
                        "type": "string",
                        "description": "Optional CLDR region code (Google) or country code (OSM filter)"
                    },
                    "field_mask": {
                        "type": "string",
                        "description": "Optional Google field mask (ignored for OpenStreetMap)"
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        },
        {
            "name": "place_details",
            "description": "Get place details with provider routing: google or openstreetmap",
            "input_schema": {
                "type": "object",
                "properties": {
                    "place_id": {
                        "type": "string",
                        "description": "Google place id or OSM id like node:123, way:456, relation:789, N123, W456, R789"
                    },
                    "osm_type": {
                        "type": "string",
                        "description": "Optional OSM type node|way|relation (or N|W|R) for openstreetmap provider"
                    },
                    "osm_id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional OSM numeric id for openstreetmap provider"
                    },
                    "provider": {
                        "type": "string",
                        "description": "auto|google|openstreetmap"
                    },
                    "language_code": {
                        "type": "string",
                        "description": "Optional language code"
                    },
                    "region_code": {
                        "type": "string",
                        "description": "Optional CLDR region code (Google)"
                    },
                    "field_mask": {
                        "type": "string",
                        "description": "Optional Google field mask (ignored for OpenStreetMap)"
                    }
                },
                "required": ["place_id"],
                "additionalProperties": false
            }
        }
    ])
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "status" => status(),
        "search_text" => search_text(args),
        "place_details" => place_details(args),
        other => Err(format!("unknown tool `{other}`")),
    }
}

fn status() -> Result<Value, String> {
    let curl_version = Command::new("curl")
        .arg("--version")
        .output()
        .map_err(|e| format!("curl not available: {e}"))?;
    let curl_line = String::from_utf8_lossy(&curl_version.stdout)
        .lines()
        .next()
        .unwrap_or("unknown")
        .to_string();

    Ok(json!({
        "ok": true,
        "provider_default": "auto",
        "google": {
            "base_url": google_base_url(),
            "has_api_key": has_google_api_key()
        },
        "openstreetmap": {
            "base_url": osm_base_url(),
            "requires_api_key": false
        },
        "requires": {
            "bins": ["curl"],
            "env_optional": ["GOOGLE_PLACES_API_KEY"],
            "env_google_only": ["GOOGLE_PLACES_API_KEY"]
        },
        "curl_version": curl_line
    }))
}

fn search_text(args: &Value) -> Result<Value, String> {
    let query = required_string(args, "query")?;
    let provider = select_provider(args)?;
    let max_results = optional_u64(args, "max_results")?.unwrap_or(DEFAULT_LIMIT);
    if max_results == 0 || max_results > MAX_LIMIT {
        return Err(format!("`max_results` must be between 1 and {MAX_LIMIT}"));
    }

    match provider {
        Provider::Google => google_search_text(args, &query, max_results),
        Provider::OpenStreetMap => osm_search_text(args, &query, max_results),
    }
}

fn place_details(args: &Value) -> Result<Value, String> {
    let place_id = optional_string(args, "place_id")?;
    let provider = select_provider(args)?;
    match provider {
        Provider::Google => {
            let place_id = place_id
                .ok_or_else(|| "missing or invalid `place_id` for google provider".to_string())?;
            google_place_details(args, &place_id)
        }
        Provider::OpenStreetMap => osm_place_details(args, place_id),
    }
}

fn google_search_text(args: &Value, query: &str, max_results: u64) -> Result<Value, String> {
    ensure_google_api_key()?;
    let language_code = optional_string(args, "language_code")?;
    let region_code = optional_string(args, "region_code")?;
    let field_mask =
        optional_string(args, "field_mask")?.unwrap_or_else(|| DEFAULT_SEARCH_FIELDS.to_string());

    let mut payload = json!({
        "textQuery": query,
        "maxResultCount": max_results
    });
    if let Some(language_code) = language_code {
        payload["languageCode"] = json!(language_code);
    }
    if let Some(region_code) = region_code {
        payload["regionCode"] = json!(region_code);
    }

    let response = call_google_places("places:searchText", &field_mask, &payload)?;
    Ok(json!({
        "provider": "google",
        "query": query,
        "max_results": max_results,
        "field_mask": field_mask,
        "data": response
    }))
}

fn google_place_details(args: &Value, place_id: &str) -> Result<Value, String> {
    ensure_google_api_key()?;
    let mut place_path = place_id.to_string();
    if !place_path.starts_with("places/") {
        place_path = format!("places/{place_path}");
    }

    let field_mask =
        optional_string(args, "field_mask")?.unwrap_or_else(|| DEFAULT_DETAILS_FIELDS.to_string());
    let language_code = optional_string(args, "language_code")?;
    let region_code = optional_string(args, "region_code")?;
    let mut endpoint = format!("{}/{}", google_base_url().trim_end_matches('/'), place_path);
    let mut query_params = Vec::new();
    if let Some(language_code) = language_code {
        query_params.push(format!("languageCode={}", encode_component(&language_code)));
    }
    if let Some(region_code) = region_code {
        query_params.push(format!("regionCode={}", encode_component(&region_code)));
    }
    if !query_params.is_empty() {
        endpoint.push('?');
        endpoint.push_str(&query_params.join("&"));
    }

    let output = Command::new("curl")
        .arg("-fsSL")
        .arg(&endpoint)
        .arg("-H")
        .arg(format!("X-Goog-Api-Key: {}", google_api_key()?))
        .arg("-H")
        .arg(format!("X-Goog-FieldMask: {field_mask}"))
        .output()
        .map_err(|e| format!("failed to execute curl: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "google place details request failed: {}",
            stderr.trim()
        ));
    }
    let body =
        String::from_utf8(output.stdout).map_err(|e| format!("invalid utf-8 response: {e}"))?;
    let json: Value =
        serde_json::from_str(&body).map_err(|e| format!("invalid json response: {e}"))?;
    Ok(json!({
        "provider": "google",
        "place_id": place_path,
        "field_mask": field_mask,
        "data": json
    }))
}

fn osm_search_text(args: &Value, query: &str, max_results: u64) -> Result<Value, String> {
    let country_codes = optional_string(args, "region_code")?;
    let mut params = vec![
        ("q".to_string(), query.to_string()),
        ("format".to_string(), "jsonv2".to_string()),
        ("addressdetails".to_string(), "1".to_string()),
        ("limit".to_string(), max_results.to_string()),
    ];
    if let Some(country_codes) = country_codes {
        params.push(("countrycodes".to_string(), country_codes.to_lowercase()));
    }
    let url = format!("{}/search?{}", osm_base_url(), encode_query(&params));
    let raw = osm_get_json(&url)?;
    let data = attach_osm_lookup_ids(raw);
    Ok(json!({
        "provider": "openstreetmap",
        "query": query,
        "max_results": max_results,
        "data": data
    }))
}

fn osm_place_details(args: &Value, place_id: Option<String>) -> Result<Value, String> {
    let osm_id = if let Some(value) = resolve_osm_from_pair(args)? {
        value
    } else if let Some(place_id) = place_id.as_ref() {
        normalize_osm_id(&place_id)?
    } else {
        return Err(
            "missing openstreetmap identifier: provide `place_id` or `osm_type` + `osm_id`"
                .to_string(),
        );
    };
    let url = format!(
        "{}/lookup?format=jsonv2&addressdetails=1&osm_ids={}",
        osm_base_url(),
        encode_component(&osm_id)
    );
    let data = osm_get_json(&url)?;
    Ok(json!({
        "provider": "openstreetmap",
        "place_id": place_id,
        "osm_id": osm_id,
        "data": data
    }))
}

fn attach_osm_lookup_ids(raw: Value) -> Value {
    let Some(items) = raw.as_array() else {
        return raw;
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let mut item = item.clone();
        if let Some(osm_id) = item.get("osm_id").and_then(|v| v.as_i64()) {
            if let Some(osm_type) = item.get("osm_type").and_then(|v| v.as_str()) {
                let prefix = match osm_type {
                    "node" => "N",
                    "way" => "W",
                    "relation" => "R",
                    _ => "",
                };
                if !prefix.is_empty() {
                    item["lookup_id"] = json!(format!("{prefix}{osm_id}"));
                }
            }
        }
        out.push(item);
    }
    Value::Array(out)
}

fn resolve_osm_from_pair(args: &Value) -> Result<Option<String>, String> {
    let osm_id = optional_u64(args, "osm_id")?;
    let osm_type = optional_string(args, "osm_type")?;
    match (osm_type, osm_id) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => {
            Err("when using OSM pair, provide both `osm_type` and `osm_id`".to_string())
        }
        (Some(osm_type), Some(osm_id)) => {
            let prefix = normalize_osm_type(&osm_type)?;
            Ok(Some(format!("{prefix}{osm_id}")))
        }
    }
}

fn normalize_osm_type(value: &str) -> Result<&'static str, String> {
    match value.to_ascii_lowercase().as_str() {
        "node" | "n" => Ok("N"),
        "way" | "w" => Ok("W"),
        "relation" | "r" => Ok("R"),
        _ => Err("`osm_type` must be node|way|relation (or N|W|R)".to_string()),
    }
}

fn select_provider(args: &Value) -> Result<Provider, String> {
    let raw = optional_string(args, "provider")?.unwrap_or_else(|| "auto".to_string());
    match raw.as_str() {
        "google" => Ok(Provider::Google),
        "openstreetmap" | "osm" => Ok(Provider::OpenStreetMap),
        "auto" => {
            if has_google_api_key() {
                Ok(Provider::Google)
            } else {
                Ok(Provider::OpenStreetMap)
            }
        }
        _ => Err("`provider` must be one of: auto, google, openstreetmap".to_string()),
    }
}

fn call_google_places(path: &str, field_mask: &str, payload: &Value) -> Result<Value, String> {
    let endpoint = format!("{}/{}", google_base_url().trim_end_matches('/'), path);
    let output = Command::new("curl")
        .arg("-fsSL")
        .arg(&endpoint)
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-H")
        .arg(format!("X-Goog-Api-Key: {}", google_api_key()?))
        .arg("-H")
        .arg(format!("X-Goog-FieldMask: {field_mask}"))
        .arg("-d")
        .arg(payload.to_string())
        .output()
        .map_err(|e| format!("failed to execute curl: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("google places request failed: {}", stderr.trim()));
    }
    let body =
        String::from_utf8(output.stdout).map_err(|e| format!("invalid utf-8 response: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("invalid json response: {e}"))
}

fn osm_get_json(url: &str) -> Result<Value, String> {
    let output = Command::new("curl")
        .arg("-fsSL")
        .arg(url)
        .arg("-H")
        .arg(format!("User-Agent: {OSM_USER_AGENT}"))
        .output()
        .map_err(|e| format!("failed to execute curl: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("openstreetmap request failed: {}", stderr.trim()));
    }
    let body =
        String::from_utf8(output.stdout).map_err(|e| format!("invalid utf-8 response: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("invalid json response: {e}"))
}

fn google_base_url() -> String {
    std::env::var("GOOGLE_PLACES_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_GOOGLE_BASE_URL.to_string())
}

fn osm_base_url() -> String {
    std::env::var("OPENSTREETMAP_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_OSM_BASE_URL.to_string())
}

fn has_google_api_key() -> bool {
    std::env::var("GOOGLE_PLACES_API_KEY")
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn ensure_google_api_key() -> Result<(), String> {
    if has_google_api_key() {
        Ok(())
    } else {
        Err("GOOGLE_PLACES_API_KEY is missing (or use provider=openstreetmap/auto)".to_string())
    }
}

fn google_api_key() -> Result<String, String> {
    std::env::var("GOOGLE_PLACES_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| "GOOGLE_PLACES_API_KEY is missing".to_string())
}

fn normalize_osm_id(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("`place_id` cannot be empty".to_string());
    }
    if trimmed.starts_with('N') || trimmed.starts_with('W') || trimmed.starts_with('R') {
        return Ok(trimmed.to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    if let Some(id) = lower.strip_prefix("node:") {
        return Ok(format!("N{id}"));
    }
    if let Some(id) = lower.strip_prefix("way:") {
        return Ok(format!("W{id}"));
    }
    if let Some(id) = lower.strip_prefix("relation:") {
        return Ok(format!("R{id}"));
    }
    Err(
        "OpenStreetMap `place_id` must be N123/W123/R123 or node:123/way:123/relation:123"
            .to_string(),
    )
}

fn encode_query(items: &[(String, String)]) -> String {
    let mut out = Vec::new();
    for (k, v) in items {
        out.push(format!("{}={}", encode_component(k), encode_component(v)));
    }
    out.join("&")
}

fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn required_string(args: &Value, key: &str) -> Result<String, String> {
    let value = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing or invalid `{key}`"))?
        .trim()
        .to_string();
    if value.is_empty() {
        return Err(format!("`{key}` cannot be empty"));
    }
    Ok(value)
}

fn optional_string(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) => Ok(Some(s.to_string())),
            None => Err(format!("`{key}` must be a string")),
        },
    }
}

fn optional_u64(args: &Value, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) => Ok(Some(n)),
            None => Err(format!("`{key}` must be an integer")),
        },
    }
}
