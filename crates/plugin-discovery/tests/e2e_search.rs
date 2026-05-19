//! Phase 98.9 — end-to-end DiscoveryClient orchestration.
//!
//! Spins up 3 wiremock servers (crates.io / GitHub / raw.github)
//! and drives `DefaultDiscoveryClient::search` against them.
//! Asserts that the cold path:
//!   1. Hits every source.
//!   2. Merges contributions by `name` into one row per crate.
//!   3. Pulls the manifest from raw.github + derives compat +
//!      category from the manifest sections.
//!   4. Promotes trust tier when the owner is in the allowlist.
//!   5. Persists the result to disk so the next call short-
//!      circuits via cache.
//! And that `refresh()` invalidates the cache + forces a re-fetch.

use std::time::Duration;

use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use nexo_plugin_discovery::client::{DefaultDiscoveryClient, DiscoveryClient};
use nexo_plugin_discovery::config::DiscoveryConfig;
use nexo_plugin_discovery::types::{CompatStatus, PluginCategory, PluginSource, TrustTier};

const MANIFEST_TOML: &str = r#"manifest_version = 2

[plugin]
id = "telegram"
version = "0.3.0"
name = "Telegram"
description = "Telegram channel"
min_nexo_version = ">=0.1"

[plugin.pairing]
kind = "qr"
"#;

fn crates_io_body() -> serde_json::Value {
    serde_json::json!({
        "crates": [
            {
                "name": "nexo-plugin-telegram",
                "description": "Telegram bot channel",
                "repository": "https://github.com/lordmacu/nexo-rs-plugin-telegram",
                "homepage": null,
                "max_stable_version": "0.3.0",
                "keywords": ["telegram", "messaging"]
            }
        ]
    })
}

fn github_body() -> serde_json::Value {
    serde_json::json!({
        "items": [
            {
                "full_name": "lordmacu/nexo-rs-plugin-telegram",
                "name": "nexo-rs-plugin-telegram",
                "description": "Telegram bot channel plugin",
                "html_url": "https://github.com/lordmacu/nexo-rs-plugin-telegram",
                "homepage": null,
                "default_branch": "main",
                "topics": ["nexo-plugin", "telegram"],
                "owner": { "login": "lordmacu" }
            }
        ]
    })
}

fn index_body(manifest_base: &str) -> serde_json::Value {
    // Point manifest_url at the mock server so wiremock can serve
    // the TOML body. Production index.json uses the real
    // raw.githubusercontent.com URL.
    serde_json::json!({
        "schema_version": 1,
        "updated_at": "2026-05-19T00:00:00Z",
        "plugins": [
            {
                "name": "nexo-plugin-telegram",
                "owner": "lordmacu",
                "repo": "lordmacu/nexo-rs-plugin-telegram",
                "manifest_url": format!("{manifest_base}/nexo-plugin.toml"),
                "category": "channel",
                "tags": ["messaging"],
                "description": "Telegram"
            }
        ]
    })
}

async fn mount_crates_io(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v1/crates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(crates_io_body()))
        .mount(server)
        .await;
}

async fn mount_github(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/search/repositories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(github_body()))
        .mount(server)
        .await;
}

async fn mount_index(server: &MockServer, manifest_base: &str) {
    Mock::given(method("GET"))
        .and(path("/index.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(index_body(manifest_base)))
        .mount(server)
        .await;
}

async fn mount_manifest(server: &MockServer) {
    // The discovery client tries the GithubTopic-derived manifest
    // URL first; here we wire BOTH the curated `manifest_url` from
    // index.json and the raw-github fallback that github_topic
    // constructs, by serving every GET that ends in
    // `/nexo-plugin.toml`.
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(r".*/nexo-plugin\.toml$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(MANIFEST_TOML)
                .insert_header("content-type", "text/plain"),
        )
        .mount(server)
        .await;
}

fn config_for(
    state_dir: &TempDir,
    crates_io: &MockServer,
    github: &MockServer,
    index_url: String,
) -> DiscoveryConfig {
    DiscoveryConfig {
        state_dir: state_dir.path().to_path_buf(),
        cache_ttl: Duration::from_secs(60),
        crates_io_endpoint: crates_io.uri(),
        github_endpoint: github.uri(),
        index_url,
        http_timeout: Duration::from_secs(5),
        official_owners: vec!["lordmacu".into()],
        daemon_version: semver::Version::parse("0.2.0").unwrap(),
        github_token: None,
    }
}

#[tokio::test]
async fn cold_fetch_merges_three_sources_into_one_official_compatible_row() {
    let crates_io = MockServer::start().await;
    let github = MockServer::start().await;
    let index_server = MockServer::start().await;
    mount_crates_io(&crates_io).await;
    mount_github(&github).await;
    mount_index(&index_server, &index_server.uri()).await;
    // Use the SAME mock host for the manifest fetch so the URL the
    // curated index points to (raw.githubusercontent.com) is
    // intercepted by wiremock; we replace the host in the discovery
    // client's fetch loop by routing through a single test server
    // for `nexo-plugin.toml` paths. The github mock server hosts
    // both the search endpoint AND the manifest endpoint.
    mount_manifest(&github).await;
    mount_manifest(&index_server).await;

    let state_dir = TempDir::new().unwrap();
    // Discovery client tries to GET the index URL VERBATIM, so the
    // mocked path is `/index.json` under index_server.
    let config = config_for(
        &state_dir,
        &crates_io,
        &github,
        format!("{}/index.json", index_server.uri()),
    );
    let client = DefaultDiscoveryClient::new(config);

    let outcome = client.search(None).await.expect("search ok");

    // Exactly 1 merged row.
    assert_eq!(
        outcome.items.len(),
        1,
        "expected 3 sources merged into 1 row, got: {:?}",
        outcome.items.iter().map(|p| &p.name).collect::<Vec<_>>()
    );
    let row = &outcome.items[0];
    assert_eq!(row.name, "nexo-plugin-telegram");

    // All 3 source badges aggregated.
    assert!(row
        .sources
        .iter()
        .any(|s| matches!(s, PluginSource::CratesIo)));
    assert!(row
        .sources
        .iter()
        .any(|s| matches!(s, PluginSource::GithubTopic { .. })));
    assert!(row
        .sources
        .iter()
        .any(|s| matches!(s, PluginSource::CuratedIndex)));

    // Owner promotion: `lordmacu` in allowlist → Official.
    assert_eq!(row.trust_tier, TrustTier::Official);

    // Compat: manifest declares `>=0.1`, daemon at 0.2.0 → Compatible.
    assert_eq!(row.compat, CompatStatus::Compatible);

    // Category derived from manifest's `[plugin.pairing]` section.
    assert_eq!(row.category, PluginCategory::Channel);

    // Version comes from crates.io's max_stable_version.
    assert_eq!(row.version.as_deref(), Some("0.3.0"));
}

#[tokio::test]
async fn second_search_serves_from_cache_without_hitting_network() {
    let crates_io = MockServer::start().await;
    let github = MockServer::start().await;
    let index_server = MockServer::start().await;
    mount_crates_io(&crates_io).await;
    mount_github(&github).await;
    mount_index(&index_server, &index_server.uri()).await;
    mount_manifest(&github).await;
    mount_manifest(&index_server).await;

    let state_dir = TempDir::new().unwrap();
    let config = config_for(
        &state_dir,
        &crates_io,
        &github,
        format!("{}/index.json", index_server.uri()),
    );
    let client = DefaultDiscoveryClient::new(config);

    let first = client.search(None).await.expect("first search ok");

    // Tear down the network — second call must read from disk
    // cache, not hit any HTTP server.
    drop(crates_io);
    drop(github);
    drop(index_server);

    let second = client.search(None).await.expect("cached search ok");
    assert_eq!(first.items.len(), second.items.len());
    assert_eq!(first.fetched_at_ms, second.fetched_at_ms);
    assert!(second.partial_failures.is_empty());
}

#[tokio::test]
async fn refresh_invalidates_cache() {
    let crates_io = MockServer::start().await;
    let github = MockServer::start().await;
    let index_server = MockServer::start().await;
    mount_crates_io(&crates_io).await;
    mount_github(&github).await;
    mount_index(&index_server, &index_server.uri()).await;
    mount_manifest(&github).await;
    mount_manifest(&index_server).await;

    let state_dir = TempDir::new().unwrap();
    let config = config_for(
        &state_dir,
        &crates_io,
        &github,
        format!("{}/index.json", index_server.uri()),
    );
    let client = DefaultDiscoveryClient::new(config);
    let first = client.search(None).await.expect("first search ok");
    client.refresh().await.expect("refresh ok");

    // Cache file must be gone after refresh.
    let cache_path = state_dir
        .path()
        .join("plugin-discovery")
        .join("catalogue.json");
    assert!(
        !cache_path.exists(),
        "cache file must be removed after refresh"
    );

    // Re-running search forces a cold re-fetch (timestamps advance).
    let second = client.search(None).await.expect("post-refresh ok");
    assert!(
        second.fetched_at_ms >= first.fetched_at_ms,
        "post-refresh search must produce a >= timestamp"
    );
    assert_eq!(second.items.len(), 1);
}

#[tokio::test]
async fn query_filters_client_side() {
    let crates_io = MockServer::start().await;
    let github = MockServer::start().await;
    let index_server = MockServer::start().await;
    mount_crates_io(&crates_io).await;
    mount_github(&github).await;
    mount_index(&index_server, &index_server.uri()).await;
    mount_manifest(&github).await;
    mount_manifest(&index_server).await;

    let state_dir = TempDir::new().unwrap();
    let config = config_for(
        &state_dir,
        &crates_io,
        &github,
        format!("{}/index.json", index_server.uri()),
    );
    let client = DefaultDiscoveryClient::new(config);

    // Hits the substring against `name` ("telegram").
    let hits = client
        .search(Some("telegram"))
        .await
        .expect("hit search ok");
    assert_eq!(hits.items.len(), 1);

    // Misses with no match.
    let misses = client
        .search(Some("does-not-exist-anywhere"))
        .await
        .expect("miss search ok");
    assert!(misses.items.is_empty());
}
