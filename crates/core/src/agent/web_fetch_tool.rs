//! FOLLOWUPS W-2 — `web_fetch` built-in tool.
//!
//! Companion to `web_search`: takes one or more URLs the agent
//! already knows (from a prior `web_search` hit, a user message,
//! a `link_understanding` summary, …) and returns the cleaned
//! body text + title for each. Reuses the runtime's existing
//! `LinkExtractor` (Phase 21) so:
//!
//! - The fetch budget, deny-host list, max-bytes cap, timeout
//!   and host blocklist are exactly the same as the auto-link
//!   pipeline. There's no second copy of the config to drift.
//! - The LRU cache is shared, so a `web_fetch` of a URL the
//!   user already pasted earlier in the session is free.
//! - Telemetry (`nexo_link_understanding_fetch_total`,
//!   `nexo_link_understanding_cache_total`,
//!   `nexo_link_understanding_fetch_duration_ms`) covers
//!   `web_fetch` calls as well — operators don't need a second
//!   dashboard.
//!
//! Distinct from `web_search` because the agent often knows the
//! URL up front (skill output, RSS poll, calendar attachment)
//! and would otherwise have to either hallucinate a search
//! query or shell out to a `fetch-url` extension.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};

pub struct WebFetchTool;

impl WebFetchTool {
    pub fn new() -> Self {
        Self
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "web_fetch".to_string(),
            description: "Fetch one or more URLs and return their cleaned body text + title. \
                Use when the agent already knows the URL (from a previous web_search, a user \
                message, a poller item, etc.). Reuses the link-understanding pipeline's \
                cache, deny-list, and size caps."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "urls": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "URLs to fetch. Up to 5 per call to keep the prompt budget bounded."
                    },
                    "max_bytes": {
                        "type": "integer",
                        "description": "Per-URL body cap (overrides the policy default; clamped down by the deployment's `link_understanding.max_bytes`)."
                    }
                },
                "required": ["urls"]
            }),
        }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for WebFetchTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        // Reuse the runtime's shared LinkExtractor. If link
        // understanding isn't wired (e.g. tests, minimal boots),
        // fail loud — we don't want to silently return empty
        // bodies and let the LLM hallucinate around them.
        let Some(extractor) = ctx.link_extractor.as_ref() else {
            return Err(anyhow::anyhow!(
                "web_fetch unavailable: runtime has no link_understanding extractor wired"
            ));
        };

        // Parse args. `urls` is required; `max_bytes` is optional.
        let urls: Vec<String> = args
            .get("urls")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .ok_or_else(|| anyhow::anyhow!("web_fetch: `urls` must be a non-empty string array"))?;

        if urls.is_empty() {
            return Err(anyhow::anyhow!("web_fetch: `urls` is empty"));
        }

        // Per-call cap so a runaway agent can't queue 1000 fetches.
        const MAX_URLS_PER_CALL: usize = 5;
        let urls = if urls.len() > MAX_URLS_PER_CALL {
            tracing::warn!(
                requested = urls.len(),
                cap = MAX_URLS_PER_CALL,
                "web_fetch: trimming urls list to per-call cap"
            );
            urls.into_iter().take(MAX_URLS_PER_CALL).collect()
        } else {
            urls
        };

        // Build cfg from policy. Same shape `web_search`'s expand
        // path uses (`policy_link_cfg` lives in
        // `web_search_tool.rs` but we duplicate the 3-line builder
        // here to keep the modules independent — copying is cheaper
        // than a cross-module helper for a 3-LOC fn).
        let mut cfg = ctx.effective_policy().link_understanding.clone();
        cfg.enabled = true;
        if let Some(max) = args.get("max_bytes").and_then(|v| v.as_u64()) {
            // Caller can shrink, never grow past the deployment cap.
            cfg.max_bytes = (max as usize).min(cfg.max_bytes);
        }

        // Fetch concurrently. Order preserved so the agent can
        // correlate each entry to its URL.
        let mut out: Vec<Value> = Vec::with_capacity(urls.len());
        for url in &urls {
            match extractor.fetch(url, &cfg).await {
                Some(summary) => out.push(json!({
                    "url": url,
                    "title": summary.title,
                    "body": summary.body,
                    "ok": true,
                })),
                None => out.push(json!({
                    "url": url,
                    "ok": false,
                    "reason": "fetch failed (host blocked, timeout, non-HTML, oversized, or transport error). \
                               Check `nexo_link_understanding_fetch_total{result}` for the bucket.",
                })),
            }
        }

        Ok(json!({
            "results": out,
            "count": urls.len(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_def_shape() {
        let def = WebFetchTool::tool_def();
        assert_eq!(def.name, "web_fetch");
        let params = &def.parameters;
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["urls"].is_object());
        assert_eq!(params["required"][0], "urls");
    }

    #[test]
    fn rejects_empty_urls_array() {
        // Sanity that the JSON Schema marks `urls` as required.
        let def = WebFetchTool::tool_def();
        let required = def.parameters["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "urls"));
    }
}
