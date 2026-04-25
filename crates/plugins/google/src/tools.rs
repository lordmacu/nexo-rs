//! `google_*` tools — agent-callable wrappers around `GoogleAuthClient`.
//!
//! Registration is gated by `AgentConfig.google_auth.is_some()` in
//! `main.rs`; when a refresh_token isn't on file yet, the LLM sees
//! all four tools, so it can drive the setup flow on its own:
//!
//!   1. `google_auth_status` → "authenticated: false"
//!   2. `google_auth_start` → returns the URL; LLM forwards it to the
//!      user via chat and waits.
//!   3. User clicks the URL, Google redirects to the loopback listener,
//!      tokens land on disk.
//!   4. `google_auth_status` → "authenticated: true", fresh access token.
//!   5. `google_call` → issues the actual API request.
//!
//! `google_auth_revoke` is only for "forget everything" — user
//! requested, or the refresh_token leaked. Regenerating tokens is a
//! full re-auth from step 2.

use std::sync::Arc;

use nexo_llm::ToolDef;
use async_trait::async_trait;
use serde_json::{json, Value};

use nexo_core::agent::context::AgentContext;
use nexo_core::agent::tool_registry::ToolHandler;

use crate::client::GoogleAuthClient;

// ── google_auth_start ─────────────────────────────────────────────────────────

pub struct GoogleAuthStartTool {
    client: Arc<GoogleAuthClient>,
}

impl GoogleAuthStartTool {
    pub fn new(client: Arc<GoogleAuthClient>) -> Self {
        Self { client }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "google_auth_start".into(),
            description: "Begin the Google OAuth consent flow. Returns a \
                URL the user must open in a browser to approve access. \
                The agent must forward the URL to the user via chat and \
                then stop calling Google tools until \
                `google_auth_status` reports authenticated. The \
                callback listener binds on 127.0.0.1 inside the agent \
                process — remote hosts need an SSH tunnel \
                (`ssh -L <port>:127.0.0.1:<port> <host>`). Only call \
                this when `google_auth_status.authenticated` is false — \
                re-calling it invalidates any in-flight consent."
                .into(),
            parameters: json!({"type": "object", "properties": {}}),
        }
    }
}

#[async_trait]
impl ToolHandler for GoogleAuthStartTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        let (url, _join) = self.client.start_auth_flow().await?;
        Ok(json!({
            "ok": true,
            "url": url,
            "instructions": "Open this URL in a browser you're logged into \
                your Google account with, approve the scopes, then call \
                google_auth_status to confirm.",
            "redirect_uri": format!("http://127.0.0.1:{}/callback",
                                    self.client.config().redirect_port),
        }))
    }
}

// ── google_auth_status ───────────────────────────────────────────────────────

pub struct GoogleAuthStatusTool {
    client: Arc<GoogleAuthClient>,
}

impl GoogleAuthStatusTool {
    pub fn new(client: Arc<GoogleAuthClient>) -> Self {
        Self { client }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "google_auth_status".into(),
            description: "Report the current Google OAuth state: \
                `authenticated` (bool), `expires_in_secs` (access \
                token TTL), `has_refresh` (can auto-renew), `scopes` \
                (what access was granted). Safe to call repeatedly — \
                does not touch the network; just reads the on-file \
                tokens. When `authenticated` is false, call \
                `google_auth_start` to kick off the consent flow."
                .into(),
            parameters: json!({"type": "object", "properties": {}}),
        }
    }
}

#[async_trait]
impl ToolHandler for GoogleAuthStatusTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        Ok(self.client.snapshot().await)
    }
}

// ── google_call ──────────────────────────────────────────────────────────────

pub struct GoogleCallTool {
    client: Arc<GoogleAuthClient>,
}

impl GoogleCallTool {
    pub fn new(client: Arc<GoogleAuthClient>) -> Self {
        Self { client }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "google_call".into(),
            description: "Make an authenticated HTTP request against any \
                `*.googleapis.com` endpoint. Method is one of GET, POST, \
                PUT, PATCH, DELETE. `body` (optional) is a JSON value \
                sent as the request payload. The access token is attached \
                as `Authorization: Bearer` and refreshed transparently \
                when stale. Returns the parsed JSON response.\n\n\
                Examples:\n\
                - Gmail inbox: `{method: 'GET', url: 'https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=10'}`\n\
                - Calendar insert: `{method: 'POST', url: 'https://www.googleapis.com/calendar/v3/calendars/primary/events', body: {...}}`\n\
                - Drive list: `{method: 'GET', url: 'https://www.googleapis.com/drive/v3/files?pageSize=50'}`\n\n\
                A 401 typically means the refresh_token was revoked — \
                call `google_auth_start` again. A 403 means the scope \
                wasn't granted; update `google_auth.scopes` in the \
                agent config and re-auth."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "method": {
                        "type": "string",
                        "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"],
                        "description": "HTTP method."
                    },
                    "url": {
                        "type": "string",
                        "description": "Full URL. Must be https:// — Google API hosts only."
                    },
                    "body": {
                        "type": "object",
                        "description": "Optional JSON payload (POST/PUT/PATCH)."
                    }
                },
                "required": ["method", "url"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for GoogleCallTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let method = args["method"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("google_call requires `method`"))?;
        let url = args["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("google_call requires `url`"))?;
        if !url.starts_with("https://") || !url.contains("googleapis.com") {
            anyhow::bail!(
                "google_call only accepts https://*.googleapis.com URLs — got `{url}`"
            );
        }
        let body = if args.get("body").is_some() && !args["body"].is_null() {
            Some(args["body"].clone())
        } else {
            None
        };
        let resp = self.client.authorized_call(method, url, body).await?;
        Ok(json!({ "ok": true, "response": resp }))
    }
}

// ── google_auth_revoke ───────────────────────────────────────────────────────

pub struct GoogleAuthRevokeTool {
    client: Arc<GoogleAuthClient>,
}

impl GoogleAuthRevokeTool {
    pub fn new(client: Arc<GoogleAuthClient>) -> Self {
        Self { client }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "google_auth_revoke".into(),
            description: "Revoke the agent's Google refresh_token (so \
                even a leaked copy of the file becomes useless) and \
                delete the local tokens file. The user can still see \
                the agent at myaccount.google.com → Security → Third-\
                party apps until Google's cache clears (minutes). Next \
                `google_call` will fail until the user re-authorises \
                via `google_auth_start`."
                .into(),
            parameters: json!({"type": "object", "properties": {}}),
        }
    }
}

#[async_trait]
impl ToolHandler for GoogleAuthRevokeTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        self.client.revoke().await?;
        Ok(json!({"ok": true, "message": "tokens revoked + wiped"}))
    }
}

// ── Registration helper ──────────────────────────────────────────────────────

/// Register all `google_*` tools against the given registry. Mirrors
/// the `register_browser_tools` convention so `main.rs` stays readable.
pub fn register_all(
    registry: &nexo_core::agent::tool_registry::ToolRegistry,
    client: Arc<GoogleAuthClient>,
) {
    registry.register(
        GoogleAuthStartTool::tool_def(),
        GoogleAuthStartTool::new(Arc::clone(&client)),
    );
    registry.register(
        GoogleAuthStatusTool::tool_def(),
        GoogleAuthStatusTool::new(Arc::clone(&client)),
    );
    registry.register(
        GoogleCallTool::tool_def(),
        GoogleCallTool::new(Arc::clone(&client)),
    );
    registry.register(
        GoogleAuthRevokeTool::tool_def(),
        GoogleAuthRevokeTool::new(client),
    );
}
