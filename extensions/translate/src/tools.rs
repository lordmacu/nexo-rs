use std::sync::OnceLock;
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::{json, Value};

pub const CLIENT_VERSION: &str = "translate-0.1.0";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Provider { LibreTranslate, DeepL }

fn provider() -> Provider {
    match std::env::var("TRANSLATE_PROVIDER").ok().as_deref() {
        Some("deepl") => Provider::DeepL,
        _ => Provider::LibreTranslate,
    }
}

fn libre_url() -> String {
    std::env::var("LIBRETRANSLATE_URL").unwrap_or_else(|_| "https://libretranslate.com".into())
}
fn libre_key() -> Option<String> {
    std::env::var("LIBRETRANSLATE_API_KEY").ok().filter(|s| !s.trim().is_empty())
}
fn deepl_key() -> Option<String> {
    std::env::var("DEEPL_API_KEY").ok().filter(|s| !s.trim().is_empty())
}
fn deepl_url() -> String {
    std::env::var("DEEPL_URL").unwrap_or_else(|_|
        if deepl_key().as_deref().map(|k| k.ends_with(":fx")).unwrap_or(false) {
            "https://api-free.deepl.com".into()
        } else {
            "https://api.deepl.com".into()
        })
}

fn http() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("agent-rs-translate/0.1")
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap()
    })
}

#[doc(hidden)]
pub fn reset_state() {}

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Provider + endpoint.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"languages","description":"List supported language codes.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"translate",
          "description":"Translate text. `source` defaults to `auto` (LibreTranslate) or detected (DeepL).",
          "input_schema":{"type":"object","properties":{
              "text":{"type":"string"},
              "target":{"type":"string","description":"ISO code, e.g. en, es, fr, de"},
              "source":{"type":"string","description":"default auto"},
              "format":{"type":"string","enum":["text","html"],"description":"default text"}
          },"required":["text","target"],"additionalProperties":false}},
        { "name":"detect","description":"Detect language of a text (LibreTranslate only).",
          "input_schema":{"type":"object","properties":{
              "text":{"type":"string"}
          },"required":["text"],"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError { pub code: i32, pub message: String }

fn bad_input(m: impl Into<String>) -> ToolError { ToolError{code:-32602,message:m.into()} }

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "languages" => languages(),
        "translate" => translate(args),
        "detect" => detect(args),
        other => Err(ToolError{code:-32601,message:format!("unknown tool `{other}`")}),
    }
}

fn status() -> Value {
    let p = provider();
    json!({
        "ok": match p { Provider::LibreTranslate => true, Provider::DeepL => deepl_key().is_some() },
        "provider": match p { Provider::LibreTranslate => "libretranslate", Provider::DeepL => "deepl" },
        "client_version": CLIENT_VERSION,
        "endpoint": match p { Provider::LibreTranslate => libre_url(), Provider::DeepL => deepl_url() },
        "libretranslate_key_present": libre_key().is_some(),
        "deepl_key_present": deepl_key().is_some(),
        "tools": ["status","languages","translate","detect"],
        "requires": {"bins":[],"env":["LIBRETRANSLATE_URL / DEEPL_API_KEY"]}
    })
}

fn languages() -> Result<Value, ToolError> {
    match provider() {
        Provider::LibreTranslate => {
            let url = format!("{}/languages", libre_url());
            let resp = http().get(&url).send().map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
            if !resp.status().is_success() {
                return Err(ToolError{code:-32003,message:format!("HTTP {}", resp.status())});
            }
            let body: Value = resp.json().map_err(|e| ToolError{code:-32006,message:e.to_string()})?;
            Ok(json!({"ok": true, "provider": "libretranslate", "languages": body}))
        }
        Provider::DeepL => {
            let key = deepl_key().ok_or_else(|| ToolError{code:-32041,message:"DEEPL_API_KEY not set".into()})?;
            let url = format!("{}/v2/languages?type=target", deepl_url());
            let resp = http().get(&url).header("Authorization", format!("DeepL-Auth-Key {key}"))
                .send().map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
            if !resp.status().is_success() {
                return Err(ToolError{code:-32003,message:format!("HTTP {}", resp.status())});
            }
            let body: Value = resp.json().map_err(|e| ToolError{code:-32006,message:e.to_string()})?;
            Ok(json!({"ok": true, "provider": "deepl", "languages": body}))
        }
    }
}

fn translate(args: &Value) -> Result<Value, ToolError> {
    let text = required_string(args, "text")?;
    let target = required_string(args, "target")?;
    let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("auto").to_string();
    let format = args.get("format").and_then(|v| v.as_str()).unwrap_or("text").to_string();

    match provider() {
        Provider::LibreTranslate => {
            let url = format!("{}/translate", libre_url());
            let mut payload = json!({"q": text, "source": source, "target": target, "format": format});
            if let Some(k) = libre_key() { payload["api_key"] = json!(k); }
            let resp = http().post(&url).json(&payload).send().map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
            let status = resp.status().as_u16();
            let body: Value = resp.json().unwrap_or(json!({}));
            if !(200..300).contains(&status) {
                let msg = body.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
                return Err(ToolError{code:-32003,message:format!("HTTP {status}: {msg}")});
            }
            Ok(json!({
                "ok": true, "provider": "libretranslate",
                "source": source, "target": target,
                "translated": body.get("translatedText"),
                "detected": body.pointer("/detectedLanguage/language"),
            }))
        }
        Provider::DeepL => {
            let key = deepl_key().ok_or_else(|| ToolError{code:-32041,message:"DEEPL_API_KEY not set".into()})?;
            let url = format!("{}/v2/translate", deepl_url());
            let mut form = vec![("text", text.clone()), ("target_lang", target.to_uppercase())];
            if source != "auto" { form.push(("source_lang", source.to_uppercase())); }
            if format == "html" { form.push(("tag_handling", "html".into())); }
            let resp = http().post(&url).header("Authorization", format!("DeepL-Auth-Key {key}"))
                .form(&form).send().map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                let b = resp.text().unwrap_or_default();
                return Err(ToolError{code:-32003,message:format!("HTTP {status}: {}", truncate(&b, 200))});
            }
            let body: Value = resp.json().map_err(|e| ToolError{code:-32006,message:e.to_string()})?;
            let first = body.pointer("/translations/0").cloned().unwrap_or(json!({}));
            Ok(json!({
                "ok": true, "provider": "deepl",
                "source": source, "target": target,
                "translated": first.get("text"),
                "detected": first.get("detected_source_language"),
            }))
        }
    }
}

fn detect(args: &Value) -> Result<Value, ToolError> {
    if provider() != Provider::LibreTranslate {
        return Err(ToolError{code:-32601,message:"detect is only available with LibreTranslate".into()});
    }
    let text = required_string(args, "text")?;
    let url = format!("{}/detect", libre_url());
    let mut payload = json!({"q": text});
    if let Some(k) = libre_key() { payload["api_key"] = json!(k); }
    let resp = http().post(&url).json(&payload).send().map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
    if !resp.status().is_success() {
        return Err(ToolError{code:-32003,message:format!("HTTP {}", resp.status())});
    }
    let body: Value = resp.json().map_err(|e| ToolError{code:-32006,message:e.to_string()})?;
    Ok(json!({"ok": true, "candidates": body}))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args.get(key).and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing `{key}`")))?
        .trim().to_string();
    if s.is_empty() { return Err(bad_input(format!("`{key}` cannot be empty"))); }
    Ok(s)
}
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}
