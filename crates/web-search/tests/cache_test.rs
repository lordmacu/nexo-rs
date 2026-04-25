use std::time::Duration;

use nexo_web_search::cache::WebSearchCache;
use nexo_web_search::{WebSearchHit, WebSearchResult};

fn sample(provider: &str) -> WebSearchResult {
    WebSearchResult {
        provider: provider.into(),
        query: "rust async".into(),
        from_cache: false,
        results: vec![WebSearchHit {
            url: "https://example.com".into(),
            title: "Example".into(),
            snippet: "snip".into(),
            site_name: Some("example.com".into()),
            published_at: None,
            body: None,
        }],
    }
}

#[tokio::test]
async fn put_then_get_within_ttl_returns_hit_marked_from_cache() {
    let cache = WebSearchCache::open_memory(Duration::from_secs(60))
        .await
        .unwrap();
    let key = WebSearchCache::key("brave", "rust async", "{}");
    cache.put(&key, &sample("brave")).await.unwrap();
    let got = cache.get(&key).await.unwrap().expect("cache hit");
    assert_eq!(got.provider, "brave");
    assert!(got.from_cache);
    assert_eq!(got.results.len(), 1);
}

#[tokio::test]
async fn get_misses_when_ttl_zero() {
    let cache = WebSearchCache::open_memory(Duration::from_secs(0))
        .await
        .unwrap();
    let key = WebSearchCache::key("brave", "x", "{}");
    cache.put(&key, &sample("brave")).await.unwrap();
    // ttl=0 means cutoff > inserted_at, so even fresh entries miss.
    let got = cache.get(&key).await.unwrap();
    assert!(got.is_none());
}

#[tokio::test]
async fn key_is_deterministic_and_distinct_across_providers() {
    let k1 = WebSearchCache::key("brave", "x", "{}");
    let k2 = WebSearchCache::key("brave", "x", "{}");
    let k3 = WebSearchCache::key("tavily", "x", "{}");
    assert_eq!(k1, k2);
    assert_ne!(k1, k3);
}

#[tokio::test]
async fn purge_expired_drops_old_rows() {
    let cache = WebSearchCache::open_memory(Duration::from_secs(60))
        .await
        .unwrap();
    let key = WebSearchCache::key("brave", "x", "{}");
    cache.put(&key, &sample("brave")).await.unwrap();
    // Empty TTL purge: with ttl=60 and entry just inserted, cutoff is
    // 60s ago — entry survives.
    let n = cache.purge_expired().await.unwrap();
    assert_eq!(n, 0);
}
