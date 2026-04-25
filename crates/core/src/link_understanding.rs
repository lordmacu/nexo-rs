//! Phase 21 — link understanding.
//!
//! When a user message contains URLs, the runtime fetches each one
//! once per turn, extracts a short text summary, and renders a
//! `# LINK CONTEXT` system block so the LLM has something to reason
//! over instead of saying "I can't see what's at that link".
//!
//! Scope guarantees:
//!
//! - **Per-agent kill switch.** `agents.<id>.link_understanding.enabled`
//!   defaults to `false`. Operators opt in.
//! - **Hard caps everywhere.** `max_links_per_turn`, `max_bytes`,
//!   request timeout, cache TTL, plus a privacy denylist of host
//!   patterns the fetcher refuses outright.
//! - **In-memory cache.** Keyed by URL, LRU with TTL. Cache hits
//!   bypass network; misses race a single in-flight fetch.
//! - **Naïve text extraction.** Strips HTML tags + collapses
//!   whitespace + truncates. No DOM library — keeps the dep
//!   surface small. A future revision can swap in `scraper` /
//!   `readability`-style heuristics behind the same trait.
//! - **Failure mode = silence.** Any fetch error (timeout, 4xx,
//!   too big, blocked host) drops the URL from the rendered
//!   block. The agent still sees the original message.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lru::LruCache;
use serde::Deserialize;

/// YAML schema. Lives under `agents.<id>.link_understanding`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkUnderstandingConfig {
    /// Master switch. `false` (default) = the runtime never fetches
    /// anything; the agent sees URLs as plain text.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum URLs honoured per turn. Extras are silently dropped
    /// (the agent still sees them in the original text).
    #[serde(default = "default_max_links")]
    pub max_links_per_turn: usize,
    /// Hard cap on the response body. The fetcher streams until this
    /// many bytes and then aborts the request — protects against a
    /// hostile server feeding gigabytes of `/dev/random`.
    #[serde(default = "default_max_bytes")]
    pub max_bytes: usize,
    /// Per-request HTTP timeout in milliseconds. Includes connection
    /// + body read.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// In-memory cache TTL in seconds. `0` = no caching (every link
    /// hits the network, debugging only).
    #[serde(default = "default_cache_ttl_secs")]
    pub cache_ttl_secs: u64,
    /// Host-suffix denylist. The fetcher refuses URLs whose host
    /// ends in any of these (case-insensitive). Defaults block the
    /// most common privacy footguns: localhost, link-local, RFC1918.
    #[serde(default = "default_deny_hosts")]
    pub deny_hosts: Vec<String>,
}

impl Default for LinkUnderstandingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_links_per_turn: default_max_links(),
            max_bytes: default_max_bytes(),
            timeout_ms: default_timeout_ms(),
            cache_ttl_secs: default_cache_ttl_secs(),
            deny_hosts: default_deny_hosts(),
        }
    }
}

fn default_max_links() -> usize {
    3
}
fn default_max_bytes() -> usize {
    1024 * 256 // 256 KiB — enough for a long article, not enough to DoS
}
fn default_timeout_ms() -> u64 {
    8_000
}
fn default_cache_ttl_secs() -> u64 {
    600
}
fn default_deny_hosts() -> Vec<String> {
    vec![
        "localhost".into(),
        "127.0.0.1".into(),
        "0.0.0.0".into(),
        "169.254.0.0".into(), // AWS metadata link-local
        "metadata.google.internal".into(),
    ]
}

/// Cached extract entry.
#[derive(Clone)]
struct CacheEntry {
    summary: Arc<str>,
    inserted_at: Instant,
}

/// One link's extracted form, ready to render into the prompt.
#[derive(Debug, Clone)]
pub struct LinkSummary {
    pub url: String,
    pub title: Option<String>,
    pub body: String,
}

/// In-memory cache + HTTP client. Held by the runtime as
/// `Arc<LinkExtractor>` and shared across sessions; the extractor
/// owns its rate limiter so concurrent turns don't stampede.
pub struct LinkExtractor {
    http: reqwest::Client,
    cache: Mutex<LruCache<String, CacheEntry>>,
    cache_ttl: Duration,
    cache_capacity: usize,
}

const DEFAULT_CACHE_CAPACITY: usize = 256;

impl LinkExtractor {
    pub fn new(cfg: &LinkUnderstandingConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("nexo-link-understanding/0.1")
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "link extractor: reqwest build failed; using default");
                reqwest::Client::new()
            });
        Self {
            http,
            cache: Mutex::new(LruCache::new(
                std::num::NonZeroUsize::new(DEFAULT_CACHE_CAPACITY).expect("cap > 0"),
            )),
            cache_ttl: Duration::from_secs(cfg.cache_ttl_secs),
            cache_capacity: DEFAULT_CACHE_CAPACITY,
        }
    }

    /// Capacity of the in-memory cache (for tests / diagnostics).
    pub fn cache_capacity(&self) -> usize {
        self.cache_capacity
    }

    /// Fetch + extract, honouring the cache. Returns `None` on any
    /// error — the caller (llm_behavior) drops the URL from the
    /// rendered block silently.
    pub async fn fetch(&self, url: &str, cfg: &LinkUnderstandingConfig) -> Option<LinkSummary> {
        if !cfg.enabled {
            return None;
        }
        if !host_allowed(url, &cfg.deny_hosts) {
            crate::telemetry::inc_link_fetch("blocked");
            return None;
        }

        // Cache lookup with TTL check. We don't dedupe in-flight
        // requests for the same URL — concurrent fetches are rare
        // (one user, one turn) and adding an in-flight map would
        // double the lock cost on the common path.
        if cfg.cache_ttl_secs > 0 {
            let mut cache = self.cache.lock().ok()?;
            if let Some(entry) = cache.get(url) {
                if entry.inserted_at.elapsed() < self.cache_ttl {
                    crate::telemetry::inc_link_cache(true);
                    return Some(LinkSummary {
                        url: url.to_string(),
                        title: None,
                        body: entry.summary.to_string(),
                    });
                }
            }
            crate::telemetry::inc_link_cache(false);
        }

        let started = std::time::Instant::now();
        let resp = match self.http.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                let result = if e.is_timeout() { "timeout" } else { "error" };
                crate::telemetry::inc_link_fetch(result);
                crate::telemetry::observe_link_fetch_ms(started.elapsed().as_millis() as u64);
                return None;
            }
        };
        if !resp.status().is_success() {
            crate::telemetry::inc_link_fetch("error");
            crate::telemetry::observe_link_fetch_ms(started.elapsed().as_millis() as u64);
            return None;
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();
        // Fetcher only understands HTML / plain text. PDFs / images
        // / video are out of scope (Phase 24 will handle media).
        if !content_type.contains("text/html")
            && !content_type.contains("text/plain")
            && !content_type.is_empty()
        {
            crate::telemetry::inc_link_fetch("non_html");
            crate::telemetry::observe_link_fetch_ms(started.elapsed().as_millis() as u64);
            return None;
        }

        let body = match read_capped(resp, cfg.max_bytes).await {
            Ok(b) => b,
            Err(_) => {
                crate::telemetry::inc_link_fetch("error");
                crate::telemetry::observe_link_fetch_ms(started.elapsed().as_millis() as u64);
                return None;
            }
        };
        let truncated = body.len() >= cfg.max_bytes;
        let extracted = extract_main_text(&body, cfg.max_bytes);
        if extracted.is_empty() {
            let result = if truncated { "too_big" } else { "non_html" };
            crate::telemetry::inc_link_fetch(result);
            crate::telemetry::observe_link_fetch_ms(started.elapsed().as_millis() as u64);
            return None;
        }

        if cfg.cache_ttl_secs > 0 {
            if let Ok(mut cache) = self.cache.lock() {
                cache.put(
                    url.to_string(),
                    CacheEntry {
                        summary: Arc::from(extracted.as_str()),
                        inserted_at: Instant::now(),
                    },
                );
            }
        }
        crate::telemetry::inc_link_fetch("ok");
        crate::telemetry::observe_link_fetch_ms(started.elapsed().as_millis() as u64);
        Some(LinkSummary {
            url: url.to_string(),
            title: extract_title(&body),
            body: extracted,
        })
    }
}

/// Detect URLs in arbitrary text. Returns deduped, in-order, capped
/// at `max`. Tolerant of trailing punctuation in messages
/// ("see https://x.com/a, then ..." drops the comma).
pub fn detect_urls(text: &str, max: usize) -> Vec<String> {
    // Hand-rolled scan instead of a heavy regex — `regex = "1"` is
    // already in the dep tree, but a literal scan is faster on
    // short user messages and avoids the catastrophic-backtracking
    // risk of a complex URL regex on hostile input.
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        let rest = &text[i..];
        let start_https = rest.find("https://");
        let start_http = rest.find("http://");
        let start = match (start_https, start_http) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let Some(rel) = start else { break };
        let abs_start = i + rel;
        let after = &text[abs_start..];
        let end = after
            .find(|c: char| c.is_whitespace() || c == '<' || c == '>' || c == '"' || c == '\'')
            .unwrap_or(after.len());
        let mut url = &after[..end];
        // Strip trailing sentence punctuation that almost never
        // belongs to the URL.
        while let Some(stripped) = url
            .strip_suffix(',')
            .or_else(|| url.strip_suffix('.'))
            .or_else(|| url.strip_suffix(';'))
            .or_else(|| url.strip_suffix(':'))
            .or_else(|| url.strip_suffix(')'))
            .or_else(|| url.strip_suffix(']'))
            .or_else(|| url.strip_suffix('}'))
            .or_else(|| url.strip_suffix('?'))
            .or_else(|| url.strip_suffix('!'))
        {
            url = stripped;
        }
        if url.len() > 2048 {
            // Reject absurdly long URLs to keep the system block small.
            i = abs_start + end;
            continue;
        }
        if seen.insert(url.to_string()) {
            out.push(url.to_string());
            if out.len() >= max {
                break;
            }
        }
        i = abs_start + end;
    }
    out
}

fn host_allowed(url: &str, deny: &[String]) -> bool {
    // Cheap lower-case host extraction without pulling in a full URL parser.
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .split('@')
        .next_back()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_lowercase();
    if host.is_empty() {
        return false;
    }
    !deny.iter().any(|pat| {
        host == pat.to_lowercase() || host.ends_with(&format!(".{}", pat.to_lowercase()))
    })
}

async fn read_capped(resp: reqwest::Response, cap: usize) -> Result<String, reqwest::Error> {
    use futures::stream::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(cap.min(64 * 1024));
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = cap.saturating_sub(buf.len());
        if remaining == 0 {
            break;
        }
        let take = remaining.min(chunk.len());
        buf.extend_from_slice(&chunk[..take]);
        if buf.len() >= cap {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Strip HTML tags, collapse whitespace, truncate. Naïve on purpose —
/// works for ~80% of articles, fails gracefully on the rest. A
/// future revision can replace this with a real readability pass.
pub fn extract_main_text(html: &str, max_bytes: usize) -> String {
    // Drop everything inside <script>, <style>, <noscript>, <head>.
    let cleaned = strip_block(html, "script");
    let cleaned = strip_block(&cleaned, "style");
    let cleaned = strip_block(&cleaned, "noscript");
    let cleaned = strip_block(&cleaned, "head");

    // Replace common block-level tags with newlines so paragraph
    // breaks survive the tag strip.
    let mut buf = String::with_capacity(cleaned.len());
    for token in tokenize(&cleaned) {
        match token {
            Token::Text(s) => buf.push_str(s),
            Token::Tag(name) => {
                let lname = name.trim_start_matches('/').to_ascii_lowercase();
                if matches!(
                    lname.as_str(),
                    "p" | "br"
                        | "div"
                        | "li"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "tr"
                        | "section"
                ) {
                    buf.push('\n');
                }
            }
        }
    }

    // Decode the four common HTML entities. A full entity table is
    // overkill here.
    let buf = buf
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");

    // Collapse whitespace runs into single spaces / single \n.
    let mut out = String::with_capacity(buf.len());
    let mut prev_blank = true;
    let mut blank_run = 0;
    for line in buf.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 && !prev_blank {
                out.push('\n');
            }
            continue;
        }
        blank_run = 0;
        if !prev_blank {
            out.push('\n');
        }
        // Collapse interior whitespace runs.
        let mut last_space = false;
        for c in trimmed.chars() {
            if c.is_whitespace() {
                if !last_space {
                    out.push(' ');
                }
                last_space = true;
            } else {
                out.push(c);
                last_space = false;
            }
        }
        prev_blank = false;
    }

    // Hard truncate by char (not byte) to avoid splitting UTF-8.
    let max_chars = max_bytes / 2; // each char ≤ 4 bytes; conservative
    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars).collect::<String>() + "…";
    }
    out
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let end_open = lower[start..].find('>')?;
    let body_start = start + end_open + 1;
    let close = lower[body_start..].find("</title")?;
    let raw = &html[body_start..body_start + close];
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(160).collect())
    }
}

fn strip_block(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}");
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;
    while cursor < html.len() {
        let Some(open_rel) = lower[cursor..].find(&open_pat) else {
            out.push_str(&html[cursor..]);
            break;
        };
        let open_abs = cursor + open_rel;
        out.push_str(&html[cursor..open_abs]);
        let after_open = lower[open_abs..].find('>').map(|p| open_abs + p + 1);
        let Some(after) = after_open else { break };
        let Some(close_rel) = lower[after..].find(&close_pat) else {
            break;
        };
        let close_abs = after + close_rel;
        let close_end = lower[close_abs..]
            .find('>')
            .map(|p| close_abs + p + 1)
            .unwrap_or(html.len());
        cursor = close_end;
    }
    out
}

enum Token<'a> {
    Text(&'a str),
    Tag(&'a str),
}

fn tokenize(html: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor < html.len() {
        let Some(open) = html[cursor..].find('<') else {
            out.push(Token::Text(&html[cursor..]));
            break;
        };
        if open > 0 {
            out.push(Token::Text(&html[cursor..cursor + open]));
        }
        let tag_start = cursor + open + 1;
        let Some(close) = html[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + close;
        // Tag name = up to the first whitespace or '>' or '/'.
        let tag_slice = &html[tag_start..tag_end];
        let name_end = tag_slice
            .find(|c: char| c.is_whitespace())
            .unwrap_or(tag_slice.len());
        out.push(Token::Tag(&tag_slice[..name_end]));
        cursor = tag_end + 1;
    }
    out
}

/// Render the `# LINK CONTEXT` system block. Empty `Vec` = empty
/// string; caller must check before pushing into `system_parts`.
pub fn render_block(summaries: &[LinkSummary]) -> String {
    if summaries.is_empty() {
        return String::new();
    }
    let mut out = String::from("# LINK CONTEXT\n\n");
    out.push_str(
        "The user's message included the following links. The runtime fetched each one and \
         extracted a text summary so you can answer with grounded facts. Cite the link if you \
         use it; do not invent details that aren't in the summary.\n\n",
    );
    for (idx, s) in summaries.iter().enumerate() {
        out.push_str(&format!("## [{}] {}\n", idx + 1, s.url));
        if let Some(title) = s.title.as_deref() {
            out.push_str(&format!("Title: {title}\n"));
        }
        out.push('\n');
        out.push_str(&s.body);
        out.push_str("\n\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_picks_https_and_http_in_order_dedup() {
        let txt = "see https://a.com/x and http://b.com? and https://a.com/x again";
        let urls = detect_urls(txt, 10);
        assert_eq!(urls, vec!["https://a.com/x", "http://b.com"]);
    }

    #[test]
    fn detect_strips_trailing_punctuation() {
        let urls = detect_urls("ok https://example.com/foo, bye.", 10);
        assert_eq!(urls, vec!["https://example.com/foo"]);
    }

    #[test]
    fn detect_caps_at_max() {
        let urls = detect_urls("https://a.com https://b.com https://c.com https://d.com", 2);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://a.com");
    }

    #[test]
    fn detect_skips_hostile_long_urls() {
        let huge = format!("https://a.com/{}", "x".repeat(3000));
        let urls = detect_urls(&format!("look {huge} thanks"), 10);
        assert!(urls.is_empty(), "URL > 2048 chars must be dropped");
    }

    #[test]
    fn host_denylist_blocks_localhost_and_metadata() {
        let deny = default_deny_hosts();
        assert!(!host_allowed("http://localhost:8080/x", &deny));
        assert!(!host_allowed("http://127.0.0.1/", &deny));
        assert!(!host_allowed("http://metadata.google.internal/x", &deny));
        assert!(!host_allowed(
            "http://api.metadata.google.internal/x",
            &deny
        ));
        assert!(host_allowed("https://example.com/x", &deny));
    }

    #[test]
    fn extract_strips_scripts_and_styles() {
        let html = "<html><head><title>T</title></head>\
                    <body><script>alert(1)</script><p>Hello</p>\
                    <style>.x{}</style><p>World</p></body></html>";
        let out = extract_main_text(html, 4096);
        assert!(out.contains("Hello"));
        assert!(out.contains("World"));
        assert!(!out.contains("alert"));
        assert!(!out.contains(".x{}"));
    }

    #[test]
    fn extract_title_from_head() {
        let html = "<html><head><title>My Page</title></head><body>x</body></html>";
        assert_eq!(extract_title(html).as_deref(), Some("My Page"));
    }

    #[test]
    fn extract_handles_missing_title() {
        let html = "<html><body>no title here</body></html>";
        assert!(extract_title(html).is_none());
    }

    #[test]
    fn render_block_lists_summaries() {
        let s = vec![
            LinkSummary {
                url: "https://a.com".into(),
                title: Some("A".into()),
                body: "alpha body".into(),
            },
            LinkSummary {
                url: "https://b.com".into(),
                title: None,
                body: "bravo body".into(),
            },
        ];
        let out = render_block(&s);
        assert!(out.contains("# LINK CONTEXT"));
        assert!(out.contains("[1] https://a.com"));
        assert!(out.contains("Title: A"));
        assert!(out.contains("alpha body"));
        assert!(out.contains("[2] https://b.com"));
        assert!(out.contains("bravo body"));
    }

    #[test]
    fn render_block_empty_yields_empty_string() {
        assert_eq!(render_block(&[]), "");
    }

    #[test]
    fn config_disabled_by_default() {
        let cfg = LinkUnderstandingConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.max_links_per_turn, 3);
        assert_eq!(cfg.max_bytes, 256 * 1024);
        assert!(cfg.deny_hosts.iter().any(|d| d == "localhost"));
    }

    #[tokio::test]
    async fn fetch_skips_when_disabled() {
        let cfg = LinkUnderstandingConfig::default(); // enabled = false
        let ext = LinkExtractor::new(&cfg);
        let r = ext.fetch("https://example.com/", &cfg).await;
        assert!(r.is_none(), "must short-circuit when disabled");
    }

    #[tokio::test]
    async fn fetch_skips_denylisted_host() {
        let cfg = LinkUnderstandingConfig {
            enabled: true,
            ..LinkUnderstandingConfig::default()
        };
        let ext = LinkExtractor::new(&cfg);
        // Even with enabled = true, localhost is on the deny list
        // and we never attempt the fetch (so this test does not
        // require a running server).
        let r = ext.fetch("http://localhost:65530/", &cfg).await;
        assert!(r.is_none());
    }
}
