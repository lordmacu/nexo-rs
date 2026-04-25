//! Phase 21 — end-to-end fetch test.
//!
//! Spins a tiny tokio TCP listener that speaks just enough HTTP/1.1
//! to satisfy reqwest, then drives `LinkExtractor::fetch` against it
//! and asserts the extracted summary.

use nexo_core::link_understanding::{LinkExtractor, LinkUnderstandingConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn spawn_html_server(html: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let body = html.as_bytes();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.write_all(body).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{}/", addr)
}

fn permissive_cfg() -> LinkUnderstandingConfig {
    LinkUnderstandingConfig {
        enabled: true,
        deny_hosts: vec![],
        ..Default::default()
    }
}

#[tokio::test]
async fn fetch_returns_summary_with_title_and_body() {
    let url = spawn_html_server(
        "<html><head><title>Hello World</title></head>\
         <body><script>alert(1)</script><p>First paragraph of content.</p>\
         <p>Second paragraph.</p></body></html>",
    )
    .await;

    let extractor = LinkExtractor::new(&permissive_cfg());
    let summary = extractor
        .fetch(&url, &permissive_cfg())
        .await
        .expect("fetch should succeed");

    assert_eq!(summary.url, url);
    assert_eq!(summary.title.as_deref(), Some("Hello World"));
    assert!(summary.body.contains("First paragraph"));
    assert!(summary.body.contains("Second paragraph"));
    assert!(
        !summary.body.contains("alert(1)"),
        "script content must be stripped"
    );
}

#[tokio::test]
async fn fetch_disabled_returns_none() {
    let url = spawn_html_server("<html><body>hi</body></html>").await;
    let extractor = LinkExtractor::new(&LinkUnderstandingConfig::default());
    let cfg = LinkUnderstandingConfig::default(); // enabled = false
    assert!(extractor.fetch(&url, &cfg).await.is_none());
}

#[tokio::test]
async fn fetch_blocked_host_returns_none() {
    // Default denylist includes 127.0.0.1 — even with enabled=true,
    // the fetcher must refuse before opening the socket.
    let url = spawn_html_server("<html><body>secret</body></html>").await;
    let extractor = LinkExtractor::new(&LinkUnderstandingConfig::default());
    let cfg = LinkUnderstandingConfig {
        enabled: true,
        ..Default::default()
    };
    assert!(extractor.fetch(&url, &cfg).await.is_none());
}

#[tokio::test]
async fn second_fetch_hits_cache() {
    let url = spawn_html_server(
        "<html><head><title>Cached</title></head><body><p>once</p></body></html>",
    )
    .await;
    let extractor = LinkExtractor::new(&permissive_cfg());
    let cfg = permissive_cfg();
    let first = extractor.fetch(&url, &cfg).await.expect("first ok");
    let second = extractor.fetch(&url, &cfg).await.expect("second ok");
    assert_eq!(first.body, second.body);
}
