//! Phase 25 — `web_search` built-in tool.
//!
//! Routes calls into the shared `WebSearchRouter`. Reads the per-binding
//! `WebSearchPolicy` from the active `AgentContext::effective` so the
//! provider hint and `expand` default come from the operator's config,
//! not from whatever the LLM decided to send.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use nexo_web_search::{WebSearchArgs, WebSearchRouter};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct WebSearchTool {
    router: Arc<WebSearchRouter>,
}

impl WebSearchTool {
    pub fn new(router: Arc<WebSearchRouter>) -> Self {
        Self { router }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "web_search".to_string(),
            description: "Search the web. Returns titles, URLs, and snippets from a configured provider (Brave, Tavily, DuckDuckGo, or Perplexity).".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query":     { "type": "string",  "description": "Search query string." },
                    "count":     { "type": "integer", "description": "Number of results (1-10). Default 5." },
                    "provider":  { "type": "string",  "description": "Override provider for this call: 'brave', 'tavily', 'duckduckgo', or 'perplexity'." },
                    "freshness": { "type": "string",  "enum": ["day","week","month","year"], "description": "Time window filter." },
                    "country":   { "type": "string",  "description": "ISO-3166 alpha-2 country code (e.g. 'US')." },
                    "language":  { "type": "string",  "description": "ISO-639-1 language code (e.g. 'en')." },
                    "expand":    { "type": "boolean", "description": "When true and link-understanding is enabled, fetch and attach the top hit bodies." }
                },
                "required": ["query"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for WebSearchTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let policy = ctx.effective_policy().web_search.clone();
        if !policy.enabled {
            return Err(anyhow::anyhow!(
                "web_search disabled by policy on this binding"
            ));
        }
        let mut search_args: WebSearchArgs = serde_json::from_value(args.clone())
            .map_err(|e| anyhow::anyhow!("web_search args: {e}"))?;
        // Default count + expand from policy when the LLM omitted them.
        if search_args.count.is_none() {
            search_args.count = Some(policy.default_count);
        }
        if !search_args.expand {
            search_args.expand = policy.expand_default;
        }
        // Provider precedence: explicit arg > policy.provider > auto.
        let policy_provider = match policy.provider.as_str() {
            "" | "auto" => None,
            other => Some(other.to_string()),
        };
        let provider_pick = search_args.provider.clone().or(policy_provider);

        let mut result = self
            .router
            .search(search_args.clone(), provider_pick.as_deref())
            .await
            .map_err(|e| anyhow::anyhow!("web_search: {e}"))?;

        if search_args.expand {
            if let Some(ext) = ctx.link_extractor.as_ref() {
                let urls = WebSearchRouter::top_urls(&result, 3);
                let mut bodies = std::collections::HashMap::new();
                let cfg = policy_link_cfg(ctx);
                for url in urls {
                    if let Some(summary) = ext.fetch(&url, &cfg).await {
                        bodies.insert(url, summary.body);
                    }
                }
                WebSearchRouter::merge_bodies(&mut result, bodies);
            }
        }

        serde_json::to_value(result).map_err(Into::into)
    }
}

/// Build a `LinkUnderstandingConfig` for the on-demand fetches `expand`
/// fires. We piggy-back on the agent's resolved policy when link
/// understanding is enabled; otherwise fall through with a permissive
/// default that still respects host denylist + size caps.
fn policy_link_cfg(ctx: &AgentContext) -> crate::link_understanding::LinkUnderstandingConfig {
    let mut cfg = ctx.effective_policy().link_understanding.clone();
    cfg.enabled = true; // local override: we asked for the fetch.
    cfg
}
