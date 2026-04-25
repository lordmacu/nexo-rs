//! Provider selection + cache + breaker glue.

use std::collections::HashMap;
use std::sync::Arc;

use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig};

use crate::cache::WebSearchCache;
use crate::provider::WebSearchProvider;
use crate::types::{WebSearchArgs, WebSearchError, WebSearchResult};

/// Default fallback order when `args.provider` and the agent's policy
/// `provider` are both absent. Brave first because it tends to give
/// the best signal-to-noise; DDG last because it scrapes HTML.
pub const DEFAULT_ORDER: &[&str] = &["brave", "tavily", "perplexity", "duckduckgo"];

pub struct WebSearchRouter {
    providers: HashMap<&'static str, Arc<dyn WebSearchProvider>>,
    breakers: HashMap<&'static str, Arc<CircuitBreaker>>,
    cache: Option<Arc<WebSearchCache>>,
    default_order: Vec<&'static str>,
}

impl WebSearchRouter {
    pub fn new(
        providers: Vec<Arc<dyn WebSearchProvider>>,
        cache: Option<Arc<WebSearchCache>>,
    ) -> Self {
        let mut map: HashMap<&'static str, Arc<dyn WebSearchProvider>> = HashMap::new();
        let mut breakers: HashMap<&'static str, Arc<CircuitBreaker>> = HashMap::new();
        for p in providers {
            let id = p.id();
            breakers.insert(
                id,
                Arc::new(CircuitBreaker::new(
                    format!("web_search:{id}"),
                    CircuitBreakerConfig::default(),
                )),
            );
            map.insert(id, p);
        }
        let default_order: Vec<&'static str> = DEFAULT_ORDER
            .iter()
            .copied()
            .filter(|id| map.contains_key(id))
            .collect();
        Self {
            providers: map,
            breakers,
            cache,
            default_order,
        }
    }

    /// `provider` here is the explicit ask: from `args.provider`, or
    /// from the agent's `web_search.provider` setting, or `None` for
    /// auto-detect.
    pub async fn search(
        &self,
        args: WebSearchArgs,
        provider: Option<&str>,
    ) -> Result<WebSearchResult, WebSearchError> {
        if args.query.trim().is_empty() {
            return Err(WebSearchError::InvalidArg("query is empty"));
        }
        let candidates = self.resolve_candidates(provider)?;

        if let Some(cache) = &self.cache {
            // For cache lookup we use the *first* candidate's id — the
            // resolution path (auto-detect) is deterministic from
            // `default_order`, so a cache hit on the first candidate is
            // the right answer for the same input.
            let first = candidates[0];
            let key = WebSearchCache::key(first, &args.query, &canonical_params(&args));
            if cache.ttl().as_secs() > 0 {
                if let Some(hit) = cache.get(&key).await? {
                    return Ok(hit);
                }
            }
        }

        let mut last_err: Option<WebSearchError> = None;
        for id in candidates {
            let breaker = self.breakers.get(id).cloned();
            if let Some(b) = &breaker {
                if !b.allow() {
                    last_err = Some(WebSearchError::ProviderUnavailable(id.to_string()));
                    continue;
                }
            }
            let provider = self
                .providers
                .get(id)
                .expect("candidates only contains keys from providers");
            match provider.search(&args).await {
                Ok(hits) => {
                    if let Some(b) = &breaker {
                        b.on_success();
                    }
                    let result = WebSearchResult {
                        provider: id.to_string(),
                        query: args.query.clone(),
                        results: hits,
                        from_cache: false,
                    };
                    if let Some(cache) = &self.cache {
                        if cache.ttl().as_secs() > 0 {
                            let key =
                                WebSearchCache::key(id, &args.query, &canonical_params(&args));
                            let _ = cache.put(&key, &result).await;
                        }
                    }
                    return Ok(result);
                }
                Err(e) => {
                    if let Some(b) = &breaker {
                        if matches!(
                            &e,
                            WebSearchError::Transport(_)
                                | WebSearchError::ProviderHttp { .. }
                        ) {
                            b.trip();
                        }
                    }
                    tracing::warn!(provider = id, error = %e, "web_search provider failed");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or(WebSearchError::NoProviderConfigured))
    }

    /// Returns the ordered list of provider ids to try, given an
    /// (optional) explicit pick.
    fn resolve_candidates(&self, explicit: Option<&str>) -> Result<Vec<&'static str>, WebSearchError> {
        if let Some(name) = explicit {
            for id in self.providers.keys() {
                if *id == name {
                    return Ok(vec![*id]);
                }
            }
            return Err(WebSearchError::NoProviderConfigured);
        }
        if self.default_order.is_empty() {
            return Err(WebSearchError::NoProviderConfigured);
        }
        Ok(self.default_order.clone())
    }

    /// For diagnostics / tests.
    pub fn provider_ids(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.providers.keys().copied().collect();
        v.sort();
        v
    }

    /// Optional `LinkExtractor` integration. Owners (nexo-core) call
    /// this *after* `search` to populate `body` on the top hits. Kept
    /// here as a free function so the router doesn't depend on
    /// nexo-core types.
    pub fn top_urls(result: &WebSearchResult, limit: usize) -> Vec<String> {
        result
            .results
            .iter()
            .take(limit)
            .map(|h| h.url.clone())
            .collect()
    }

    /// Replace `body` on hits whose URL matches a key in the supplied
    /// map. The caller (nexo-core) owns the actual fetch via
    /// `LinkExtractor` and hands us back a url→body map.
    pub fn merge_bodies(result: &mut WebSearchResult, bodies: HashMap<String, String>) {
        for hit in &mut result.results {
            if let Some(b) = bodies.get(&hit.url) {
                hit.body = Some(b.clone());
            }
        }
    }
}

fn canonical_params(args: &WebSearchArgs) -> String {
    // Stable JSON that excludes `provider` (router decides) and
    // `expand` (post-processing — same hits regardless).
    let v = serde_json::json!({
        "count": args.count,
        "freshness": args.freshness,
        "country": args.country,
        "language": args.language,
    });
    serde_json::to_string(&v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{WebSearchHit, WebSearchResult};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StubProvider {
        id: &'static str,
        requires_cred: bool,
        calls: AtomicUsize,
        fail_status: Option<u16>,
    }

    #[async_trait]
    impl WebSearchProvider for StubProvider {
        fn id(&self) -> &'static str {
            self.id
        }
        fn requires_credential(&self) -> bool {
            self.requires_cred
        }
        async fn search(&self, args: &WebSearchArgs) -> Result<Vec<WebSearchHit>, WebSearchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(s) = self.fail_status {
                return Err(WebSearchError::ProviderHttp {
                    provider: self.id.to_string(),
                    status: s,
                });
            }
            Ok(vec![WebSearchHit {
                url: format!("https://{}/q?{}", self.id, args.query),
                title: format!("hit on {}", self.id),
                snippet: "stub snippet".into(),
                site_name: Some(self.id.to_string()),
                published_at: None,
                body: None,
            }])
        }
    }

    fn stub(id: &'static str, requires_cred: bool, fail: Option<u16>) -> Arc<dyn WebSearchProvider> {
        Arc::new(StubProvider {
            id,
            requires_cred,
            calls: AtomicUsize::new(0),
            fail_status: fail,
        })
    }

    fn args(query: &str) -> WebSearchArgs {
        WebSearchArgs {
            query: query.into(),
            count: None,
            provider: None,
            freshness: None,
            country: None,
            language: None,
            expand: false,
        }
    }

    #[tokio::test]
    async fn auto_detect_picks_first_in_default_order() {
        let r = WebSearchRouter::new(
            vec![stub("duckduckgo", false, None), stub("brave", true, None)],
            None,
        );
        let res = r.search(args("rust"), None).await.unwrap();
        assert_eq!(res.provider, "brave");
    }

    #[tokio::test]
    async fn explicit_unknown_provider_errors() {
        let r = WebSearchRouter::new(vec![stub("brave", true, None)], None);
        let err = r.search(args("rust"), Some("tavily")).await.unwrap_err();
        assert!(matches!(err, WebSearchError::NoProviderConfigured));
    }

    #[tokio::test]
    async fn empty_provider_list_errors() {
        let r = WebSearchRouter::new(vec![], None);
        let err = r.search(args("rust"), None).await.unwrap_err();
        assert!(matches!(err, WebSearchError::NoProviderConfigured));
    }

    #[tokio::test]
    async fn explicit_failing_provider_returns_error() {
        let r = WebSearchRouter::new(vec![stub("brave", true, Some(500))], None);
        let err = r.search(args("rust"), Some("brave")).await.unwrap_err();
        assert!(matches!(err, WebSearchError::ProviderHttp { status: 500, .. }));
    }

    #[tokio::test]
    async fn auto_falls_through_failing_provider() {
        let r = WebSearchRouter::new(
            vec![
                stub("brave", true, Some(500)),
                stub("tavily", true, None),
            ],
            None,
        );
        let res = r.search(args("rust"), None).await.unwrap();
        assert_eq!(res.provider, "tavily");
    }

    #[tokio::test]
    async fn empty_query_rejected() {
        let r = WebSearchRouter::new(vec![stub("brave", true, None)], None);
        let err = r.search(args("   "), None).await.unwrap_err();
        assert!(matches!(err, WebSearchError::InvalidArg(_)));
    }

    #[tokio::test]
    async fn breaker_opens_after_repeated_failures() {
        let r = WebSearchRouter::new(vec![stub("brave", true, Some(500))], None);
        for _ in 0..10 {
            let _ = r.search(args("rust"), Some("brave")).await;
        }
        // After enough failures, the breaker rejects without calling
        // the provider — error is ProviderUnavailable, not ProviderHttp.
        let err = r.search(args("rust"), Some("brave")).await.unwrap_err();
        assert!(matches!(err, WebSearchError::ProviderUnavailable(_)));
    }

    #[test]
    fn merge_bodies_attaches_to_matching_urls() {
        let mut res = WebSearchResult {
            provider: "brave".into(),
            query: "x".into(),
            from_cache: false,
            results: vec![
                WebSearchHit {
                    url: "https://a.com".into(),
                    title: "A".into(),
                    snippet: "s".into(),
                    site_name: None,
                    published_at: None,
                    body: None,
                },
                WebSearchHit {
                    url: "https://b.com".into(),
                    title: "B".into(),
                    snippet: "s".into(),
                    site_name: None,
                    published_at: None,
                    body: None,
                },
            ],
        };
        let mut bodies = HashMap::new();
        bodies.insert("https://a.com".into(), "body of a".into());
        WebSearchRouter::merge_bodies(&mut res, bodies);
        assert_eq!(res.results[0].body.as_deref(), Some("body of a"));
        assert!(res.results[1].body.is_none());
    }
}
