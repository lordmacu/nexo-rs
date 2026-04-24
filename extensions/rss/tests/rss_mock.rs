use rss_ext::tools;
use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn dispatch(
    name: &'static str,
    args: serde_json::Value,
) -> Result<serde_json::Value, tools::ToolError> {
    tokio::task::spawn_blocking(move || tools::dispatch(name, &args))
        .await
        .expect("join")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn fetch_feed_parses_rss() {
    let server = MockServer::start().await;
    let feed = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Example Feed</title>
    <link>https://example.test/</link>
    <description>Sample RSS</description>
    <item>
      <title>First item</title>
      <link>https://example.test/first</link>
      <guid>item-1</guid>
      <pubDate>Thu, 24 Apr 2026 12:00:00 GMT</pubDate>
      <description>Hello world</description>
    </item>
  </channel>
</rss>"#;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/rss+xml")
                .set_body_string(feed),
        )
        .mount(&server)
        .await;

    let out = dispatch(
        "fetch_feed",
        json!({"url": format!("{}/feed.xml", server.uri()), "limit": 1}),
    )
    .await
    .expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["count"], 1);
    assert_eq!(out["feed"]["title"], "Example Feed");
    assert_eq!(out["entries"][0]["title"], "First item");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn fetch_feed_rejects_non_http_urls() {
    let err = dispatch("fetch_feed", json!({"url":"ftp://example.com/feed.xml"}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("http(s)"));
}
