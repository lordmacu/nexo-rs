//! `kind: rss` — fetch an RSS / Atom feed, dispatch new items.
//!
//! Cursor encodes the highest-seen `<guid>` (or `<id>` for Atom) plus
//! the last `ETag` we received. Server returns `304 Not Modified` →
//! tick reports `items_seen=0` and updates nothing.

use std::collections::HashSet;

use async_trait::async_trait;
use reqwest::header::{ETAG, IF_NONE_MATCH};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::PollerError;
use crate::poller::{OutboundDelivery, PollContext, Poller, TickOutcome};

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RssJobConfig {
    pub feed_url: String,
    /// Maximum items per tick (most-recent first).
    #[serde(default = "default_max")]
    pub max_per_tick: usize,
    /// Mustache-light template. Fields: `{title}`, `{link}`, `{summary}`.
    #[serde(default = "default_template")]
    pub message_template: String,
    pub deliver: super::gmail::DeliverCfg,
}

fn default_max() -> usize { 5 }
fn default_template() -> String { "{title}\n{link}".to_string() }

#[derive(Debug, Default, Serialize, Deserialize)]
struct CursorState {
    seen_ids: Vec<String>, // bounded ring; oldest dropped first
    etag: Option<String>,
}

const SEEN_CAP: usize = 200;

pub struct RssPoller {
    http: reqwest::Client,
}

impl RssPoller {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest"),
        }
    }
}

impl Default for RssPoller {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl Poller for RssPoller {
    fn kind(&self) -> &'static str { "rss" }

    fn description(&self) -> &'static str {
        "Polls an RSS / Atom feed and dispatches new items via the agent's bound channel."
    }

    fn validate(&self, config: &Value) -> Result<(), PollerError> {
        let _: RssJobConfig = serde_json::from_value(config.clone()).map_err(|e| {
            PollerError::Config {
                job: "<rss>".into(),
                reason: e.to_string(),
            }
        })?;
        Ok(())
    }

    async fn tick(&self, ctx: &PollContext) -> Result<TickOutcome, PollerError> {
        let cfg: RssJobConfig =
            serde_json::from_value(ctx.config.clone()).map_err(|e| PollerError::Config {
                job: ctx.job_id.clone(),
                reason: e.to_string(),
            })?;

        let mut state: CursorState = ctx
            .cursor
            .as_deref()
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or_default();

        let mut req = self.http.get(&cfg.feed_url);
        if let Some(etag) = state.etag.as_deref() {
            req = req.header(IF_NONE_MATCH, etag);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;

        // Server says nothing changed.
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(TickOutcome::default());
        }
        if !resp.status().is_success() {
            let s = resp.status();
            return Err(if s.is_client_error() {
                PollerError::Permanent(anyhow::anyhow!("HTTP {s}"))
            } else {
                PollerError::Transient(anyhow::anyhow!("HTTP {s}"))
            });
        }
        let new_etag = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp
            .text()
            .await
            .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;

        let items = parse_feed(&body);
        let known: HashSet<String> = state.seen_ids.iter().cloned().collect();

        let target_channel: agent_auth::Channel = match cfg.deliver.channel.as_str() {
            "whatsapp" => agent_auth::handle::WHATSAPP,
            "telegram" => agent_auth::handle::TELEGRAM,
            other => {
                return Err(PollerError::Config {
                    job: ctx.job_id.clone(),
                    reason: format!("unknown deliver.channel '{other}'"),
                });
            }
        };

        let mut deliver = Vec::new();
        let mut newly_seen: Vec<String> = Vec::new();
        let mut items_seen = 0u32;
        for it in items.iter().take(cfg.max_per_tick) {
            items_seen += 1;
            if known.contains(&it.id) {
                continue;
            }
            let text = render(&cfg.message_template, it);
            deliver.push(OutboundDelivery {
                channel: target_channel,
                recipient: cfg.deliver.to.clone(),
                payload: json!({ "text": text }),
            });
            newly_seen.push(it.id.clone());
        }

        // Bounded ring of ids: append new, trim oldest.
        for id in newly_seen {
            state.seen_ids.push(id);
        }
        if state.seen_ids.len() > SEEN_CAP {
            let drop = state.seen_ids.len() - SEEN_CAP;
            state.seen_ids.drain(0..drop);
        }
        state.etag = new_etag;
        let cursor = serde_json::to_vec(&state).ok();
        let dispatched = deliver.len() as u32;
        Ok(TickOutcome {
            items_seen,
            items_dispatched: dispatched,
            deliver,
            next_cursor: cursor,
            next_interval_hint: None,
        })
    }
}

#[derive(Debug, Clone)]
struct FeedItem {
    id: String,
    title: String,
    link: String,
    summary: String,
}

fn parse_feed(body: &str) -> Vec<FeedItem> {
    // Hand-rolled lite parser — depends on no XML crate. Captures
    // <item> / <entry> blocks and pulls <title>, <link>, <guid>/<id>,
    // <description>/<summary>. Skips malformed blocks silently.
    let mut out = Vec::new();
    for chunk in split_blocks(body) {
        let title = tag_text(&chunk, "title").unwrap_or_default();
        let link = tag_attr(&chunk, "link", "href")
            .or_else(|| tag_text(&chunk, "link"))
            .unwrap_or_default();
        let id = tag_text(&chunk, "guid")
            .or_else(|| tag_text(&chunk, "id"))
            .unwrap_or_else(|| link.clone());
        let summary = tag_text(&chunk, "description")
            .or_else(|| tag_text(&chunk, "summary"))
            .unwrap_or_default();
        if !id.is_empty() {
            out.push(FeedItem {
                id,
                title,
                link,
                summary,
            });
        }
    }
    out
}

fn split_blocks(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for tag in ["<item", "<entry"] {
        let close: &str = if tag == "<item" { "</item>" } else { "</entry>" };
        let mut idx = 0;
        while let Some(open) = body[idx..].find(tag) {
            let abs_open = idx + open;
            let after = abs_open + tag.len();
            let Some(end) = body[after..].find(close) else { break };
            let abs_end = after + end + close.len();
            out.push(&body[abs_open..abs_end]);
            idx = abs_end;
        }
    }
    out
}

fn tag_text(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let i = s.find(&open)?;
    let after_open = s[i..].find('>')? + i + 1;
    let j = s[after_open..].find(&close)? + after_open;
    let raw = &s[after_open..j];
    Some(strip_cdata(raw).trim().to_string())
}

fn tag_attr(s: &str, tag: &str, attr: &str) -> Option<String> {
    let open = format!("<{tag}");
    let i = s.find(&open)?;
    let close = s[i..].find('>')? + i + 1;
    let header = &s[i..close];
    let needle = format!("{attr}=\"");
    let j = header.find(&needle)? + needle.len();
    let end = header[j..].find('"')? + j;
    Some(header[j..end].to_string())
}

fn strip_cdata(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix("<![CDATA[").unwrap_or(s);
    s.strip_suffix("]]>").unwrap_or(s)
}

fn render(template: &str, item: &FeedItem) -> String {
    let mut out = template.replace("{title}", &item.title);
    out = out.replace("{link}", &item.link);
    out = out.replace("{summary}", &item.summary);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rss_minimal() {
        let body = r#"
            <rss><channel>
              <item>
                <title>Hello</title>
                <link>https://example.com/1</link>
                <guid>id-1</guid>
              </item>
              <item>
                <title>Two</title>
                <link>https://example.com/2</link>
                <guid>id-2</guid>
              </item>
            </channel></rss>
        "#;
        let items = parse_feed(body);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "id-1");
        assert_eq!(items[1].title, "Two");
    }

    #[test]
    fn parse_atom_minimal() {
        let body = r#"
            <feed>
              <entry>
                <title>Alpha</title>
                <link href="https://example.com/a" />
                <id>tag:example,2026:a</id>
                <summary>first</summary>
              </entry>
            </feed>
        "#;
        let items = parse_feed(body);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].link, "https://example.com/a");
        assert_eq!(items[0].id, "tag:example,2026:a");
    }

    #[test]
    fn cdata_is_stripped() {
        let body = "<item><title><![CDATA[Hi <b>there</b>]]></title><guid>g</guid></item>";
        let items = parse_feed(body);
        assert_eq!(items[0].title, "Hi <b>there</b>");
    }

    #[test]
    fn validate_rejects_missing_url() {
        let p = RssPoller::new();
        let cfg = json!({ "deliver": { "channel": "telegram", "to": "1" } });
        assert!(p.validate(&cfg).is_err());
    }

    #[test]
    fn render_substitutes_fields() {
        let item = FeedItem {
            id: "g".into(),
            title: "T".into(),
            link: "L".into(),
            summary: "S".into(),
        };
        assert_eq!(render("{title} | {link} | {summary}", &item), "T | L | S");
    }
}
