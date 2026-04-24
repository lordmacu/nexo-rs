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
    let bin_path = bin();
    let probe = Command::new(&bin_path).arg("--version").output();
    let (version, status_error) = match probe {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            let version = first_non_empty_line(&stdout)
                .or_else(|| first_non_empty_line(&stderr));
            let err = if out.status.success() {
                None
            } else {
                first_non_empty_line(&stderr)
                    .or_else(|| first_non_empty_line(&stdout))
                    .or_else(|| Some(format!("exit status {}", out.status)))
            };
            (version, err)
        }
        Err(e) => (None, Some(format!("spawn failed: {e}"))),
    };
    json!({
        "ok": version.is_some(),
        "provider": "tesseract-ocr",
        "client_version": CLIENT_VERSION,
        "bin": bin_path,
        "bin_version": version,
        "status_error": status_error,
        "install_hint": if version.is_none() { Some(install_hint_for_current_os()) } else { None::<&str> },
        "tools": ["status","languages","ocr"],
        "requires": {"bins":["tesseract"],"env":[]}
    })
}

fn first_non_empty_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

fn install_hint_for_current_os() -> &'static str {
    if cfg!(target_os = "linux") {
        "Install tesseract. Debian/Ubuntu: `sudo apt-get install tesseract-ocr`; Fedora: `sudo dnf install tesseract`; Alpine: `apk add tesseract-ocr`."
    } else if cfg!(target_os = "macos") {
        "Install tesseract with Homebrew: `brew install tesseract`."
    } else if cfg!(target_os = "windows") {
        "Install Tesseract OCR and ensure `tesseract.exe` is in PATH (or set `TESSERACT_BIN`)."
    } else {
        "Install Tesseract OCR and ensure the `tesseract` binary is available in PATH (or set `TESSERACT_BIN`)."
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_non_empty_line_skips_blanks() {
        let out = first_non_empty_line("\n  \n  tesseract 5.4.0 \n");
        assert_eq!(out.as_deref(), Some("tesseract 5.4.0"));
    }

    #[test]
    fn install_hint_is_non_empty() {
        assert!(!install_hint_for_current_os().trim().is_empty());
    }
}
