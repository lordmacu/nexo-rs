//! Agent-callable browser tools.
//!
//! Wraps every `BrowserCmd` variant as an individual `ToolHandler` so
//! the LLM can drive Chrome directly instead of the host code having
//! to round-trip through the broker. Each tool holds an
//! `Arc<BrowserPlugin>` and calls `plugin.execute(...)` to reach the
//! live CDP session.
//!
//! Registration happens in `main.rs` once per agent that has
//! `plugins: [browser]` in its yaml — same gating model as
//! `MemoryTool`.
//!
//! Naming convention: `browser_<verb>` so the relevance filter and
//! per-agent `allowed_tools` globs ("browser_*") work cleanly.

use std::sync::Arc;

use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::ToolHandler;
use nexo_llm::ToolDef;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::command::{BrowserCmd, BrowserResult};
use crate::plugin::BrowserPlugin;

/// Shared constructor helper — every tool gets the same plugin handle.
fn shared_plugin_arg(plugin: &Arc<BrowserPlugin>) -> Arc<BrowserPlugin> {
    Arc::clone(plugin)
}

/// Translate a `BrowserResult` into a JSON value the LLM can consume.
/// Errors surface as `{ok: false, error: "..."}` so the model can
/// react (retry, ask the user, abandon) instead of the tool call
/// disappearing into an opaque failure.
fn result_to_json(res: BrowserResult) -> Value {
    if !res.ok {
        return json!({
            "ok": false,
            "error": res.error.unwrap_or_else(|| "unknown error".into()),
        });
    }
    let mut out = json!({ "ok": true });
    let obj = out.as_object_mut().unwrap();
    if let Some(r) = res.result {
        obj.insert("result".into(), r);
    }
    if let Some(d) = res.data {
        obj.insert("png_base64".into(), Value::String(d));
    }
    if let Some(s) = res.snapshot {
        obj.insert("snapshot".into(), Value::String(s));
    }
    out
}

// ── Navigate ─────────────────────────────────────────────────────────

pub struct BrowserNavigateTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserNavigateTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_navigate".into(),
            description: "Navigate the managed Chrome session to the given URL. \
                Waits for the page's load event. Returns `{ok, error?}`. \
                Use this before `browser_snapshot` / `browser_click` / \
                `browser_fill` on a new page."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Absolute URL (http/https). Relative URLs are rejected."
                    }
                },
                "required": ["url"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for BrowserNavigateTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let url = args["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("browser_navigate requires `url`"))?
            .to_string();
        Ok(result_to_json(self.plugin.execute(BrowserCmd::Navigate { url }).await))
    }
}

// ── Click ────────────────────────────────────────────────────────────

pub struct BrowserClickTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserClickTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_click".into(),
            description: "Click a DOM element. `target` is either an element \
                reference emitted by `browser_snapshot` (e.g. `@e12`) or a \
                CSS selector (e.g. `button[type=submit]`). Prefer element \
                refs — they're stable across DOM mutations within a single \
                snapshot turn."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Element ref (`@eN`) or CSS selector."
                    }
                },
                "required": ["target"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for BrowserClickTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let target = args["target"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("browser_click requires `target`"))?
            .to_string();
        Ok(result_to_json(self.plugin.execute(BrowserCmd::Click { target }).await))
    }
}

// ── Fill ─────────────────────────────────────────────────────────────

pub struct BrowserFillTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserFillTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_fill".into(),
            description: "Type a value into an input / textarea / \
                contenteditable element. `target` follows the same rules \
                as `browser_click`. `value` replaces the element's current \
                contents — there's no append mode. For multi-step forms, \
                call once per field."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Element ref (`@eN`) or CSS selector for the input."
                    },
                    "value": {
                        "type": "string",
                        "description": "Text to type. Special keys (Enter, Tab) are NOT interpreted — use `browser_evaluate` for keyboard shortcuts."
                    }
                },
                "required": ["target", "value"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for BrowserFillTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let target = args["target"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("browser_fill requires `target`"))?
            .to_string();
        let value = args["value"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("browser_fill requires `value`"))?
            .to_string();
        Ok(result_to_json(self.plugin.execute(BrowserCmd::Fill { target, value }).await))
    }
}

// ── Screenshot ──────────────────────────────────────────────────────

pub struct BrowserScreenshotTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserScreenshotTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_screenshot".into(),
            description: "Capture the current viewport as a base64-encoded \
                PNG. Returned in the `png_base64` field — vision-capable \
                models receive it directly. Use `browser_snapshot` instead \
                when you only need text-level DOM info (cheaper, faster)."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }
}

#[async_trait]
impl ToolHandler for BrowserScreenshotTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        Ok(result_to_json(self.plugin.execute(BrowserCmd::Screenshot).await))
    }
}

// ── Evaluate ────────────────────────────────────────────────────────

pub struct BrowserEvaluateTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserEvaluateTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_evaluate".into(),
            description: "Run arbitrary JavaScript in the page context and \
                return its value as JSON (`result` field). Useful for \
                reading computed state (`document.title`, \
                `window.location.href`, form values), triggering \
                keyboard events, scrolling, etc. The expression's return \
                value must be JSON-serialisable — wrap complex objects in \
                `JSON.stringify` if needed."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "script": {
                        "type": "string",
                        "description": "JS expression or statement. Multi-line OK. Implicit return on the last expression."
                    }
                },
                "required": ["script"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for BrowserEvaluateTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let script = args["script"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("browser_evaluate requires `script`"))?
            .to_string();
        Ok(result_to_json(self.plugin.execute(BrowserCmd::Evaluate { script }).await))
    }
}

// ── Snapshot ────────────────────────────────────────────────────────

pub struct BrowserSnapshotTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserSnapshotTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_snapshot".into(),
            description: "Return a text representation of the current DOM \
                with element references (`@e1`, `@e2`, …) you can pass to \
                `browser_click` / `browser_fill` / `browser_scroll_to`. \
                Includes visible text, semantic role, and hrefs. This is \
                the primary way for the model to 'see' the page when \
                vision isn't available. Call after navigate / click to \
                refresh the refs — they're only stable within one snapshot."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }
}

#[async_trait]
impl ToolHandler for BrowserSnapshotTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        Ok(result_to_json(self.plugin.execute(BrowserCmd::Snapshot).await))
    }
}

// ── Scroll ──────────────────────────────────────────────────────────

pub struct BrowserScrollToTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserScrollToTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_scroll_to".into(),
            description: "Scroll an element into view. Useful when the \
                snapshot shows refs to items below the fold — `click` \
                works on off-screen elements but many UIs only bind \
                handlers after the element scrolls into view."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Element ref (`@eN`) or CSS selector."
                    }
                },
                "required": ["target"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for BrowserScrollToTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let target = args["target"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("browser_scroll_to requires `target`"))?
            .to_string();
        Ok(result_to_json(self.plugin.execute(BrowserCmd::ScrollTo { target }).await))
    }
}

// ── Derived helpers (JS-backed) ─────────────────────────────────────

/// `browser_current_url` — reads `location.href` via evaluate. Not a
/// separate CDP command; implemented on top of `Evaluate` for symmetry
/// with the other tools. Saves the LLM one round-trip of figuring out
/// the right script.
pub struct BrowserCurrentUrlTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserCurrentUrlTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_current_url".into(),
            description: "Return the current page URL. Implemented as a \
                shortcut over `browser_evaluate` so the model doesn't \
                have to remember `location.href`. Returned in `result`."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }
}

#[async_trait]
impl ToolHandler for BrowserCurrentUrlTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        Ok(result_to_json(
            self.plugin
                .execute(BrowserCmd::Evaluate { script: "location.href".into() })
                .await,
        ))
    }
}

/// `browser_wait_for` — polls a CSS selector until it appears (or
/// timeout). Built on evaluate with a bounded retry loop so we don't
/// add a new CDP command just for this.
pub struct BrowserWaitForTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserWaitForTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_wait_for".into(),
            description: "Poll a CSS selector every 250 ms until it \
                appears in the DOM, up to `timeout_ms` (default 5000). \
                Returns `{ok: true, found: bool}`. Use before \
                `browser_click` on elements that arrive after an XHR / \
                SPA navigation."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector the element must match."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Max wait in milliseconds. Default 5000, capped at 30000."
                    }
                },
                "required": ["selector"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for BrowserWaitForTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let selector = args["selector"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("browser_wait_for requires `selector`"))?
            .to_string();
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(5_000).min(30_000);
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(timeout_ms);
        loop {
            let script = format!("!!document.querySelector({})", serde_json::to_string(&selector)?);
            let res = self.plugin.execute(BrowserCmd::Evaluate { script }).await;
            if res.ok {
                if let Some(Value::Bool(true)) = res.result.as_ref() {
                    return Ok(json!({"ok": true, "found": true}));
                }
            }
            if std::time::Instant::now() >= deadline {
                return Ok(json!({"ok": true, "found": false}));
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
}

/// `browser_go_back` / `browser_go_forward` — history navigation via
/// `evaluate`. Explicit tools save the LLM from having to know the
/// `history.back()` / `history.forward()` snippets.
pub struct BrowserGoBackTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserGoBackTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_go_back".into(),
            description: "Navigate one step back in the browser history \
                (equivalent to clicking the back button). Returns `{ok}`."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }
}

#[async_trait]
impl ToolHandler for BrowserGoBackTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        Ok(result_to_json(
            self.plugin
                .execute(BrowserCmd::Evaluate { script: "history.back()".into() })
                .await,
        ))
    }
}

pub struct BrowserGoForwardTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserGoForwardTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_go_forward".into(),
            description: "Navigate one step forward in the browser history. \
                Only succeeds if the user (or a prior tool call) just went \
                back. Returns `{ok}`."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }
}

#[async_trait]
impl ToolHandler for BrowserGoForwardTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        Ok(result_to_json(
            self.plugin
                .execute(BrowserCmd::Evaluate { script: "history.forward()".into() })
                .await,
        ))
    }
}

/// `browser_press_key` — keyboard synthesis via `evaluate`. Useful for
/// Enter-to-submit, Tab-next-field, Escape-to-close modal, etc.
pub struct BrowserPressKeyTool {
    plugin: Arc<BrowserPlugin>,
}

impl BrowserPressKeyTool {
    pub fn new(plugin: &Arc<BrowserPlugin>) -> Self {
        Self { plugin: shared_plugin_arg(plugin) }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "browser_press_key".into(),
            description: "Dispatch a synthetic KeyboardEvent (keydown + \
                keyup) on the currently focused element. Handles common \
                keys by name: `Enter`, `Tab`, `Escape`, `ArrowUp`, \
                `ArrowDown`, `ArrowLeft`, `ArrowRight`. Any other value \
                is treated as a literal character."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Key name (Enter, Tab, Escape, Arrow*) or literal character."
                    }
                },
                "required": ["key"]
            }),
        }
    }
}

const ALLOWED_KEY_NAMES: &[&str] = &[
    "Enter",
    "Tab",
    "Escape",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Backspace",
    "Delete",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Space",
];

#[async_trait]
impl ToolHandler for BrowserPressKeyTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let key = args["key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("browser_press_key requires `key`"))?;
        // Restrict `key` to either a known named key (above) or a
        // single Unicode character. Embedding an arbitrary LLM-supplied
        // string into the eval'd JS body was a script-injection vector
        // — serialization protected quote breakout but not larger
        // sequences like `"x';alert(1);//"`.
        let named = ALLOWED_KEY_NAMES.contains(&key);
        let is_single_char = key.chars().count() == 1
            && key
                .chars()
                .next()
                .map(|c| !c.is_control())
                .unwrap_or(false);
        if !named && !is_single_char {
            anyhow::bail!(
                "browser_press_key rejected: expected a known key name or a single character, got `{key}`"
            );
        }
        let script = format!(
            r#"(function() {{
                const k = {};
                const map = {{
                    Enter:      {{ key: 'Enter',      code: 'Enter',      keyCode: 13 }},
                    Tab:        {{ key: 'Tab',        code: 'Tab',        keyCode: 9  }},
                    Escape:     {{ key: 'Escape',     code: 'Escape',     keyCode: 27 }},
                    ArrowUp:    {{ key: 'ArrowUp',    code: 'ArrowUp',    keyCode: 38 }},
                    ArrowDown:  {{ key: 'ArrowDown',  code: 'ArrowDown',  keyCode: 40 }},
                    ArrowLeft:  {{ key: 'ArrowLeft',  code: 'ArrowLeft',  keyCode: 37 }},
                    ArrowRight: {{ key: 'ArrowRight', code: 'ArrowRight', keyCode: 39 }}
                }};
                const spec = map[k] || {{ key: k, code: k, keyCode: k.charCodeAt(0) }};
                const target = document.activeElement || document.body;
                target.dispatchEvent(new KeyboardEvent('keydown', {{ ...spec, bubbles: true }}));
                target.dispatchEvent(new KeyboardEvent('keyup',   {{ ...spec, bubbles: true }}));
                return true;
            }})()"#,
            serde_json::to_string(key)?
        );
        Ok(result_to_json(self.plugin.execute(BrowserCmd::Evaluate { script }).await))
    }
}

// ── Registration helper ─────────────────────────────────────────────

/// Register every browser tool on the given `ToolRegistry`. Call once
/// per agent whose yaml lists `browser` in `plugins:`. Bundling the
/// registration here means adding a new tool only requires editing this
/// file, not every call site.
pub fn register_all(
    registry: &nexo_core::agent::tool_registry::ToolRegistry,
    plugin: &Arc<BrowserPlugin>,
) {
    registry.register(BrowserNavigateTool::tool_def(), BrowserNavigateTool::new(plugin));
    registry.register(BrowserClickTool::tool_def(), BrowserClickTool::new(plugin));
    registry.register(BrowserFillTool::tool_def(), BrowserFillTool::new(plugin));
    registry.register(BrowserScreenshotTool::tool_def(), BrowserScreenshotTool::new(plugin));
    registry.register(BrowserEvaluateTool::tool_def(), BrowserEvaluateTool::new(plugin));
    registry.register(BrowserSnapshotTool::tool_def(), BrowserSnapshotTool::new(plugin));
    registry.register(BrowserScrollToTool::tool_def(), BrowserScrollToTool::new(plugin));
    registry.register(BrowserCurrentUrlTool::tool_def(), BrowserCurrentUrlTool::new(plugin));
    registry.register(BrowserWaitForTool::tool_def(), BrowserWaitForTool::new(plugin));
    registry.register(BrowserGoBackTool::tool_def(), BrowserGoBackTool::new(plugin));
    registry.register(BrowserGoForwardTool::tool_def(), BrowserGoForwardTool::new(plugin));
    registry.register(BrowserPressKeyTool::tool_def(), BrowserPressKeyTool::new(plugin));
}
