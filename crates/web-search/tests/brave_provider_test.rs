#![cfg(feature = "brave")]

//! End-to-end provider test: stand up a tokio TCP listener, return a
//! canned Brave response, and assert the parsed `WebSearchHit`s.

use nexo_web_search::provider::WebSearchProvider;
use nexo_web_search::providers::brave::BraveProvider;
use nexo_web_search::WebSearchArgs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CANNED: &str = r#"{
  "web": {
    "results": [
      {
        "url": "https://example.com/a",
        "title": "First",
        "description": "Snippet of first result.",
        "page_age": "2026-04-20T00:00:00Z"
      },
      {
        "url": "https://example.org/b",
        "title": "Second",
        "description": "Snippet two."
      }
    ]
  }
}"#;

async fn spawn_brave() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                CANNED.len(),
                CANNED
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}/web/search")
}

fn args(query: &str) -> WebSearchArgs {
    WebSearchArgs {
        query: query.into(),
        count: Some(2),
        provider: None,
        freshness: None,
        country: Some("US".into()),
        language: Some("en".into()),
        expand: false,
    }
}

#[tokio::test]
async fn brave_provider_parses_canned_response() {
    let endpoint = spawn_brave().await;
    let provider = BraveProvider::with_endpoint("test-key".into(), 5000, endpoint);
    let hits = provider.search(&args("rust")).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].url, "https://example.com/a");
    assert_eq!(hits[0].title, "First");
    assert_eq!(hits[0].site_name.as_deref(), Some("example.com"));
    assert_eq!(
        hits[0].published_at.as_deref(),
        Some("2026-04-20T00:00:00Z")
    );
    assert!(hits[1].published_at.is_none());
}

#[tokio::test]
async fn brave_provider_rejects_empty_query() {
    let provider = BraveProvider::new("test-key".into(), 5000);
    let err = provider.search(&args("   ")).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("query is empty"));
}
