use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

pub const CLIENT_VERSION: &str = "tesseract-ocr-0.1.0";

fn bin() -> String { std::env::var("TESSERACT_BIN").unwrap_or_else(|_| "tesseract".into()) }

#[doc(hidden)]
pub fn reset_state() {}

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Tesseract binary + language info.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"languages","description":"List installed language packs.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"ocr",
          "description":"Run OCR on an image file (PNG/JPG/TIFF/PDF with pdf support). Returns extracted text.",
          "input_schema":{"type":"object","properties":{
              "image_path":{"type":"string","description":"Absolute or relative path to the image"},
              "lang":{"type":"string","description":"Tesseract language code(s), e.g. `eng`, `spa`, `eng+spa`. Default `eng`"},
              "psm":{"type":"integer","minimum":0,"maximum":13,"description":"Page segmentation mode. Default 3"},
              "oem":{"type":"integer","minimum":0,"maximum":3,"description":"OCR engine mode. Default 3"}
          },"required":["image_path"],"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError { pub code: i32, pub message: String }

fn bad_input(m: impl Into<String>) -> ToolError { ToolError{code:-32602,message:m.into()} }

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "languages" => languages(),
        "ocr" => ocr(args),
        other => Err(ToolError{code:-32601,message:format!("unknown tool `{other}`")}),
    }
}

fn status() -> Value {
    let version = Command::new(bin()).arg("--version").output().ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stderr).to_string();
            s.lines().next().map(|l| l.trim().to_string())
        });
    json!({
        "ok": version.is_some(),
        "provider": "tesseract-ocr",
        "client_version": CLIENT_VERSION,
        "bin": bin(),
        "bin_version": version,
        "tools": ["status","languages","ocr"],
        "requires": {"bins":["tesseract"],"env":[]}
    })
}

fn languages() -> Result<Value, ToolError> {
    let out = Command::new(bin()).args(["--list-langs"]).output()
        .map_err(|e| ToolError{code:-32003,message:format!("spawn failed: {e}")})?;
    if !out.status.success() {
        return Err(ToolError{code:-32003,message:format!("tesseract failed: {}", String::from_utf8_lossy(&out.stderr))});
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let langs: Vec<String> = text.lines().skip(1).map(|l| l.trim().to_string()).filter(|s| !s.is_empty()).collect();
    Ok(json!({"ok": true, "count": langs.len(), "languages": langs}))
}

fn ocr(args: &Value) -> Result<Value, ToolError> {
    let image_path = required_string(args, "image_path")?;
    if !Path::new(&image_path).exists() {
        return Err(bad_input(format!("image_path does not exist: {image_path}")));
    }
    let lang = args.get("lang").and_then(|v| v.as_str()).unwrap_or("eng").to_string();
    let psm = args.get("psm").and_then(|v| v.as_u64()).unwrap_or(3);
    let oem = args.get("oem").and_then(|v| v.as_u64()).unwrap_or(3);

    if !lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '_') {
        return Err(bad_input("lang must be alphanumeric, `+`, or `_`"));
    }

    let out = Command::new(bin())
        .arg(&image_path)
        .arg("stdout")
        .args(["-l", &lang, "--psm", &psm.to_string(), "--oem", &oem.to_string()])
        .output()
        .map_err(|e| ToolError{code:-32003,message:format!("spawn failed: {e}")})?;
    if !out.status.success() {
        return Err(ToolError{code:-32003,message:format!("tesseract failed: {}", truncate(&String::from_utf8_lossy(&out.stderr), 400))});
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    Ok(json!({
        "ok": true,
        "image_path": image_path,
        "lang": lang,
        "psm": psm,
        "oem": oem,
        "text": text,
        "length": text.len(),
    }))
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
