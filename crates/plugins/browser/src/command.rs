use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum BrowserCmd {
    Navigate { url: String },
    Click { target: String },
    Fill { target: String, value: String },
    Screenshot,
    Evaluate { script: String },
    Snapshot,
    ScrollTo { target: String },
}

#[derive(Debug, Serialize)]
pub struct BrowserResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>, // base64 PNG for screenshot
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>, // text snapshot with @eN refs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BrowserResult {
    pub fn ok() -> Self {
        Self {
            ok: true,
            result: None,
            data: None,
            snapshot: None,
            error: None,
        }
    }

    pub fn ok_value(v: Value) -> Self {
        Self {
            ok: true,
            result: Some(v),
            data: None,
            snapshot: None,
            error: None,
        }
    }

    pub fn ok_screenshot(data: String) -> Self {
        Self {
            ok: true,
            result: None,
            data: Some(data),
            snapshot: None,
            error: None,
        }
    }

    pub fn ok_snapshot(snapshot: String) -> Self {
        Self {
            ok: true,
            result: None,
            data: None,
            snapshot: Some(snapshot),
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            data: None,
            snapshot: None,
            error: Some(msg.into()),
        }
    }
}
