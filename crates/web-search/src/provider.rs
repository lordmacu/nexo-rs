//! `WebSearchProvider` trait — every backend implements this.
//!
//! Providers stay thin: they translate `WebSearchArgs` into a single
//! HTTP call, parse the response into `WebSearchHit`s, and return
//! errors typed enough that the router can decide whether to retry on
//! another provider or surface to the LLM.

use async_trait::async_trait;

use crate::types::{WebSearchArgs, WebSearchError, WebSearchHit};

#[async_trait]
pub trait WebSearchProvider: Send + Sync {
    /// Stable string identifier (`"brave"`, `"tavily"`, …). Used as a
    /// breaker key, cache key prefix, and metric label.
    fn id(&self) -> &'static str;

    /// `true` when the provider needs an API key. The router uses this
    /// to skip credentialed providers in auto-detect when no key is
    /// present, and to fall through to free providers (DuckDuckGo).
    fn requires_credential(&self) -> bool;

    async fn search(&self, args: &WebSearchArgs) -> Result<Vec<WebSearchHit>, WebSearchError>;
}
