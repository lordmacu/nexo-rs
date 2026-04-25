//! Phase 25 — multi-provider web search.
//!
//! Native built-in tool that an agent can call as `web_search(query, ...)`.
//! The runtime selects a provider (Brave / Tavily / DuckDuckGo / Perplexity),
//! consults a shared SQLite cache, runs the call through a circuit
//! breaker, sanitises the output, and (optionally) expands top hits via
//! the Phase 21 [`LinkExtractor`].
//!
//! Boundary:
//! - This crate owns the trait, the router, the cache, the providers,
//!   and the result types.
//! - It does **not** know about [`agent_core::AgentContext`], the LLM
//!   tool registry, or YAML loading. Wiring lives in `agent-core` and
//!   `src/main.rs`.

pub mod cache;
pub mod provider;
pub mod providers;
pub mod router;
pub mod sanitise;
pub mod types;

pub use cache::WebSearchCache;
pub use provider::WebSearchProvider;
pub use router::WebSearchRouter;
pub use types::{Freshness, WebSearchArgs, WebSearchError, WebSearchHit, WebSearchResult};
