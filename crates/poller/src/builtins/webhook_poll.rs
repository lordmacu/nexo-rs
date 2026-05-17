//! `kind: webhook_poll` — generic JSON HTTP poller.
//!
//! Configurable `url`, optional headers, optional `items_path` (dotted
//! JSON path that locates the array of items in the response), and an
//! `id_field` to dedup with. Useful for any service that exposes
//! "list since cursor" endpoints (Slack, Linear, custom internal APIs)
//! and doesn't need a bespoke built-in.
//!
//! Post Phase 96 outbound goes through `ctx.host.broker_publish`. The
//! poller resolves the target account via `host.credentials_get` and
//! builds the topic `plugin.outbound.<channel>.<account_id>` itself —
//! the runner is channel-agnostic.

use std::collections::HashSet;

use async_trait::async_trait;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::deliver::{render_template, DeliverCfg};
use crate::error::PollerError;
use crate::host::{TickAck, TickMetrics};
use crate::poller::{PollContext, Poller};

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WebhookJobConfig {
    pub url: String,
    /// Default GET. POST allowed for cursor-paginated APIs.
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    /// JSON body for POST requests. Ignored on GET.
    #[serde(default)]
    pub body: Option<Value>,
    /// Dotted path to the items array (`"data.items"`, `"events"`,
    /// empty for "the response IS the array").
    #[serde(default)]
    pub items_path: String,
    /// Field within each item used as a stable id for dedup.
    #[serde(default = "default_id_field")]
    pub id_field: String,
    #[serde(default = "default_max")]
    pub max_per_tick: usize,
    /// Mustache-light template. The whole item is exposed under
    /// `{json}` (debug only) and individual fields via their key.
    pub message_template: String,
    pub deliver: DeliverCfg,
    /// Reject hosts in RFC1918 / loopback ranges to avoid SSRF. Set
    /// to true only for internal services.
    #[serde(default)]
    pub allow_private_networks: bool,
}

fn default_method() -> String {
    "GET".into()
}
fn default_id_field() -> String {
    "id".into()
}
fn default_max() -> usize {
    20
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CursorState {
    seen_ids: Vec<String>,
}

const SEEN_CAP: usize = 500;

pub struct WebhookPoller {
    http: reqwest::Client,
}

impl WebhookPoller {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest"),
        }
    }
}

impl Default for WebhookPoller {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Poller for WebhookPoller {
    fn kind(&self) -> &'static str {
        "webhook_poll"
    }

    fn description(&self) -> &'static str {
        "Generic HTTP-poller: hits a URL, parses JSON, dispatches new items."
    }

    fn validate(&self, config: &Value) -> Result<(), PollerError> {
        let cfg: WebhookJobConfig =
            serde_json::from_value(config.clone()).map_err(|e| PollerError::Config {
                job: "<webhook_poll>".into(),
                reason: e.to_string(),
            })?;
        if !cfg.allow_private_networks && url_targets_private(&cfg.url) {
            return Err(PollerError::Config {
                job: "<webhook_poll>".into(),
                reason: format!(
                    "url '{}' targets a private/loopback host; set allow_private_networks: true to opt in",
                    cfg.url
                ),
            });
        }
        Ok(())
    }

    async fn tick(&self, ctx: &PollContext) -> Result<TickAck, PollerError> {
        let cfg: WebhookJobConfig =
            serde_json::from_value(ctx.config.clone()).map_err(|e| PollerError::Config {
                job: ctx.job_id.clone(),
                reason: e.to_string(),
            })?;
        let mut state: CursorState = ctx
            .cursor
            .as_deref()
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or_default();

        let method =
            Method::from_bytes(cfg.method.as_bytes()).map_err(|e| PollerError::Config {
                job: ctx.job_id.clone(),
                reason: format!("invalid method '{}': {e}", cfg.method),
            })?;
        let mut req = self.http.request(method.clone(), &cfg.url);
        for (k, v) in &cfg.headers {
            req = req.header(k, v);
        }
        if method == Method::POST {
            if let Some(body) = &cfg.body {
                req = req.json(body);
            }
        }
        let resp = req
            .send()
            .await
            .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;
        let status = resp.status();
        let body: Value = if status.is_success() {
            resp.json()
                .await
                .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?
        } else if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(PollerError::Permanent(anyhow::anyhow!(
                "HTTP {status} — credentials may need rotation"
            )));
        } else if status.is_client_error() {
            return Err(PollerError::Permanent(anyhow::anyhow!("HTTP {status}")));
        } else {
            return Err(PollerError::Transient(anyhow::anyhow!("HTTP {status}")));
        };

        // Resolve the outbound channel's account id via the host so
        // the runner stays channel-agnostic. Topic format follows the
        // plugin.outbound.<channel>.<instance> contract that the
        // WhatsApp / Telegram plugins subscribe to.
        let cred = ctx
            .host
            .credentials_get(cfg.deliver.channel.clone())
            .await
            .map_err(|e| PollerError::Permanent(anyhow::anyhow!("credentials_get: {e}")))?;
        let account_id = cred
            .get("account_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                PollerError::Permanent(anyhow::anyhow!(
                    "credentials_get('{}') returned no `account_id`",
                    cfg.deliver.channel
                ))
            })?
            .to_string();
        let topic = format!("plugin.outbound.{}.{}", cfg.deliver.channel, account_id);

        let items = pluck_items(&body, &cfg.items_path);
        let known: HashSet<String> = state.seen_ids.iter().cloned().collect();

        let mut new_ids = Vec::new();
        let mut items_seen = 0u32;
        let mut items_dispatched = 0u32;
        for item in items.iter().take(cfg.max_per_tick) {
            items_seen += 1;
            let id = match item.get(&cfg.id_field) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                _ => continue,
            };
            if known.contains(&id) {
                continue;
            }
            let text = render_template(&cfg.message_template, item);
            let payload = json!({ "to": cfg.deliver.to, "text": text });
            let payload_bytes = serde_json::to_vec(&payload)
                .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;
            ctx.host
                .broker_publish(topic.clone(), payload_bytes)
                .await
                .map_err(|e| PollerError::Transient(anyhow::anyhow!("broker_publish: {e}")))?;
            items_dispatched += 1;
            new_ids.push(id);
        }

        state.seen_ids.extend(new_ids);
        if state.seen_ids.len() > SEEN_CAP {
            let drop = state.seen_ids.len() - SEEN_CAP;
            state.seen_ids.drain(0..drop);
        }
        let cursor = serde_json::to_vec(&state).ok();
        Ok(TickAck {
            next_cursor: cursor,
            next_interval_hint: None,
            metrics: Some(TickMetrics {
                items_seen,
                items_dispatched,
            }),
        })
    }
}

fn pluck_items<'a>(body: &'a Value, path: &str) -> Vec<&'a Value> {
    if path.is_empty() {
        return body
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default();
    }
    let mut cur = body;
    for seg in path.split('.') {
        match cur.get(seg) {
            Some(v) => cur = v,
            None => return Vec::new(),
        }
    }
    cur.as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn url_targets_private(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let after_scheme = lower.split("://").nth(1).unwrap_or(&lower);
    let host = after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    if host == "localhost" || host == "127.0.0.1" || host == "::1" || host.starts_with("fd") {
        return true;
    }
    if let Some(rest) = host.strip_prefix("10.") {
        return rest.split('.').count() >= 2;
    }
    if let Some(rest) = host.strip_prefix("192.168.") {
        return rest.split('.').count() >= 1;
    }
    if let Some(rest) = host.strip_prefix("172.") {
        if let Some(second) = rest.split('.').next() {
            if let Ok(n) = second.parse::<u8>() {
                return (16..=31).contains(&n);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pluck_root_array() {
        let body = json!([1, 2, 3]);
        let items = pluck_items(&body, "");
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn pluck_nested_path() {
        let body = json!({"data": {"events": [{"id": "a"}, {"id": "b"}]}});
        let items = pluck_items(&body, "data.events");
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn pluck_missing_path_returns_empty() {
        let body = json!({"foo": "bar"});
        assert!(pluck_items(&body, "missing").is_empty());
    }

    #[test]
    fn ssrf_rejects_loopback() {
        assert!(url_targets_private("http://localhost:8080"));
        assert!(url_targets_private("http://127.0.0.1:8080"));
        assert!(url_targets_private("http://10.0.0.5/api"));
        assert!(url_targets_private("http://192.168.1.1"));
        assert!(url_targets_private("http://172.16.5.5"));
        assert!(!url_targets_private("https://api.example.com"));
        assert!(!url_targets_private("https://172.32.0.1"));
    }

    #[test]
    fn validate_rejects_loopback_without_optin() {
        let p = WebhookPoller::new();
        let cfg = json!({
            "url": "http://127.0.0.1:8080",
            "message_template": "x",
            "deliver": { "channel": "telegram", "to": "1" },
        });
        assert!(p.validate(&cfg).is_err());
    }

    #[test]
    fn validate_accepts_loopback_with_optin() {
        let p = WebhookPoller::new();
        let cfg = json!({
            "url": "http://127.0.0.1:8080",
            "message_template": "x",
            "deliver": { "channel": "telegram", "to": "1" },
            "allow_private_networks": true,
        });
        p.validate(&cfg).unwrap();
    }

    #[test]
    fn validate_accepts_recipient_alias_via_deliver() {
        let p = WebhookPoller::new();
        let cfg = json!({
            "url": "https://api.example.com",
            "message_template": "x",
            "deliver": { "channel": "telegram", "recipient": "1" },
        });
        p.validate(&cfg).unwrap();
    }
}
