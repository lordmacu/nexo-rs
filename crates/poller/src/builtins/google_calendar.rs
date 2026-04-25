//! `kind: google_calendar` — Calendar v3 events incremental sync.
//!
//! Cursor stores the `nextSyncToken` Google returns. First tick fetches
//! a window with `timeMin` (default = now), captures the `nextSyncToken`,
//! and dispatches nothing. Subsequent ticks pass `syncToken=<cursor>`
//! and dispatch only the diff. Token expiry → `Permanent` error so the
//! operator runs `agent pollers reset <id>` to re-baseline.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use nexo_auth::handle::GOOGLE;
use nexo_plugin_google::GoogleAuthClient;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::PollerError;
use crate::poller::{OutboundDelivery, PollContext, Poller, TickOutcome};

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CalendarJobConfig {
    /// Calendar id. `"primary"` resolves to the agent's primary calendar.
    #[serde(default = "default_calendar_id")]
    pub calendar_id: String,
    /// Mustache-light template. Fields: `{summary}`, `{start}`, `{end}`,
    /// `{location}`, `{status}`, `{html_link}`.
    #[serde(default = "default_template")]
    pub message_template: String,
    /// Skip events whose `status` is "cancelled". Default true.
    #[serde(default = "default_skip_cancelled")]
    pub skip_cancelled: bool,
    pub deliver: super::gmail::DeliverCfg,
}

fn default_calendar_id() -> String {
    "primary".into()
}
fn default_skip_cancelled() -> bool {
    true
}
fn default_template() -> String {
    "📅 {summary} — {start}\n{html_link}".to_string()
}

pub struct GoogleCalendarPoller {
    /// Reuse the same client cache as the gmail built-in shape.
    /// Token refresh is shared so calendar + gmail jobs for the same
    /// agent only refresh once between them.
    clients: DashMap<String, Arc<GoogleAuthClient>>,
}

impl GoogleCalendarPoller {
    pub fn new() -> Self {
        Self {
            clients: DashMap::new(),
        }
    }
}

impl Default for GoogleCalendarPoller {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Poller for GoogleCalendarPoller {
    fn kind(&self) -> &'static str {
        "google_calendar"
    }

    fn description(&self) -> &'static str {
        "Polls Google Calendar v3 events with syncToken; dispatches new + updated events."
    }

    fn validate(&self, config: &Value) -> Result<(), PollerError> {
        let _: CalendarJobConfig =
            serde_json::from_value(config.clone()).map_err(|e| PollerError::Config {
                job: "<google_calendar>".into(),
                reason: e.to_string(),
            })?;
        Ok(())
    }

    async fn tick(&self, ctx: &PollContext) -> Result<TickOutcome, PollerError> {
        let cfg: CalendarJobConfig =
            serde_json::from_value(ctx.config.clone()).map_err(|e| PollerError::Config {
                job: ctx.job_id.clone(),
                reason: e.to_string(),
            })?;
        let handle = ctx
            .credentials
            .resolve(&ctx.agent_id, GOOGLE)
            .map_err(|_| PollerError::CredentialsMissing {
                agent: ctx.agent_id.clone(),
                channel: GOOGLE,
            })?;
        let client = build_client(ctx, &handle, &self.clients).await?;

        let sync_token = ctx
            .cursor
            .as_deref()
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(str::to_string);

        let mut url = format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}/events?singleEvents=true&maxResults=250",
            urlencode(&cfg.calendar_id)
        );
        if let Some(t) = sync_token.as_deref() {
            url.push_str("&syncToken=");
            url.push_str(&urlencode(t));
        } else {
            // First tick: only future events. Avoid back-fill of years.
            url.push_str("&timeMin=");
            url.push_str(&urlencode(&Utc::now().to_rfc3339()));
        }

        let resp: Value = client
            .authorized_call("GET", &url, None)
            .await
            .map_err(classify_calendar_err)?;

        // 410 GONE → syncToken expired. Surfaced from authorized_call as
        // an anyhow with the body; classify it as Permanent so the
        // runner pauses and an operator runs `pollers reset`.
        let next_sync = resp
            .get("nextSyncToken")
            .and_then(Value::as_str)
            .map(str::to_string);

        let target_channel: nexo_auth::Channel = match cfg.deliver.channel.as_str() {
            "whatsapp" => nexo_auth::handle::WHATSAPP,
            "telegram" => nexo_auth::handle::TELEGRAM,
            other => {
                return Err(PollerError::Config {
                    job: ctx.job_id.clone(),
                    reason: format!("unknown deliver.channel '{other}'"),
                });
            }
        };

        let events = resp
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let items_seen = events.len() as u32;

        let mut deliver = Vec::new();
        // Don't dispatch anything on the very first tick — we just want
        // to capture nextSyncToken so subsequent ticks see incrementals.
        if sync_token.is_some() {
            for ev in &events {
                if cfg.skip_cancelled
                    && ev.get("status").and_then(Value::as_str) == Some("cancelled")
                {
                    continue;
                }
                let summary = ev
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("(no title)");
                let start = ev
                    .get("start")
                    .and_then(|s| s.get("dateTime").or_else(|| s.get("date")))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let end = ev
                    .get("end")
                    .and_then(|s| s.get("dateTime").or_else(|| s.get("date")))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let location = ev.get("location").and_then(Value::as_str).unwrap_or("");
                let status = ev.get("status").and_then(Value::as_str).unwrap_or("");
                let html_link = ev.get("htmlLink").and_then(Value::as_str).unwrap_or("");

                let mut text = cfg.message_template.clone();
                text = text.replace("{summary}", summary);
                text = text.replace("{start}", start);
                text = text.replace("{end}", end);
                text = text.replace("{location}", location);
                text = text.replace("{status}", status);
                text = text.replace("{html_link}", html_link);

                deliver.push(OutboundDelivery {
                    channel: target_channel,
                    recipient: cfg.deliver.to.clone(),
                    payload: json!({ "text": text }),
                });
            }
        }

        let cursor_bytes = next_sync.map(|t| t.into_bytes());
        let dispatched = deliver.len() as u32;
        Ok(TickOutcome {
            items_seen,
            items_dispatched: dispatched,
            deliver,
            next_cursor: cursor_bytes,
            next_interval_hint: None,
        })
    }
}

async fn build_client(
    ctx: &PollContext,
    handle: &nexo_auth::CredentialHandle,
    cache: &DashMap<String, Arc<GoogleAuthClient>>,
) -> Result<Arc<GoogleAuthClient>, PollerError> {
    let id = handle.account_id_raw().to_string();
    if let Some(c) = cache.get(&id) {
        return Ok(c.clone());
    }
    let stores = ctx.stores.as_ref().ok_or_else(|| PollerError::Config {
        job: ctx.job_id.clone(),
        reason: "PollContext.stores is None".into(),
    })?;
    let account = stores
        .google
        .account(&id)
        .ok_or_else(|| PollerError::CredentialsMissing {
            agent: ctx.agent_id.clone(),
            channel: GOOGLE,
        })?;
    let cid = std::fs::read_to_string(&account.client_id_path)
        .map(|s| s.trim().to_string())
        .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;
    let cs = std::fs::read_to_string(&account.client_secret_path)
        .map(|s| s.trim().to_string())
        .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;
    let cfg = nexo_plugin_google::GoogleAuthConfig {
        client_id: cid,
        client_secret: cs,
        scopes: account.scopes.clone(),
        token_file: account.token_path.to_string_lossy().into_owned(),
        redirect_port: 0,
    };
    let workspace = account
        .token_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let client = GoogleAuthClient::new_with_sources(
        cfg,
        &workspace,
        Some(nexo_plugin_google::SecretSources {
            client_id_path: account.client_id_path.clone(),
            client_secret_path: account.client_secret_path.clone(),
        }),
    );
    client
        .load_from_disk()
        .await
        .map_err(|e| PollerError::Permanent(e.context("calendar: load_from_disk")))?;
    cache.insert(id, client.clone());
    Ok(client)
}

fn classify_calendar_err(err: anyhow::Error) -> PollerError {
    let m = err.to_string();
    if m.contains("410") || m.contains("Gone") || m.contains("invalid_grant") {
        PollerError::Permanent(err.context("calendar"))
    } else if m.contains("401") || m.contains("403") {
        PollerError::Permanent(err.context("calendar: auth"))
    } else {
        PollerError::Transient(err.context("calendar"))
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            out.push(ch);
        } else {
            for b in ch.to_string().as_bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_minimal() {
        let p = GoogleCalendarPoller::new();
        let cfg = json!({
            "deliver": { "channel": "telegram", "to": "1" }
        });
        p.validate(&cfg).unwrap();
    }

    #[test]
    fn validate_rejects_unknown_field() {
        let p = GoogleCalendarPoller::new();
        let cfg = json!({
            "deliver": { "channel": "telegram", "to": "1" },
            "wat": true,
        });
        assert!(p.validate(&cfg).is_err());
    }

    #[test]
    fn classify_410_is_permanent() {
        let e = anyhow::anyhow!("HTTP 410: Gone");
        assert!(matches!(
            classify_calendar_err(e),
            PollerError::Permanent(_)
        ));
    }

    #[test]
    fn classify_500_is_transient() {
        let e = anyhow::anyhow!("HTTP 500");
        assert!(matches!(
            classify_calendar_err(e),
            PollerError::Transient(_)
        ));
    }
}
