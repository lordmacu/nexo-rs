use std::time::Duration;

use serde_json::json;
use tokio::net::TcpListener;

/// Spawn a minimal mock CDP WebSocket server that responds to one method call.
/// `handler` receives the parsed request JSON and returns the result JSON.
async fn mock_cdp_server(
    handler: impl Fn(serde_json::Value) -> serde_json::Value + Send + 'static,
) -> String {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let ws = accept_async(stream).await.unwrap();
            let (mut sink, mut source) = ws.split();
            while let Some(Ok(msg)) = source.next().await {
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    _ => continue,
                };
                let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                let id = req["id"].as_u64().unwrap();
                let result = handler(req);
                let resp = json!({ "id": id, "result": result });
                let _ = sink.send(Message::Text(resp.to_string())).await;
            }
        }
    });

    format!("ws://127.0.0.1:{}", addr.port())
}

#[tokio::test]
async fn send_receives_correct_response() {
    use nexo_plugin_browser::CdpClient;

    let ws_url = mock_cdp_server(|_req| json!({ "title": "Test Page" })).await;
    let client = CdpClient::connect(&ws_url).await.unwrap();

    let result = client
        .send("Target.getTargetInfo", json!({}))
        .await
        .unwrap();
    assert_eq!(result["title"], "Test Page");
}

#[tokio::test]
async fn multiple_concurrent_sends_correlate_correctly() {
    use nexo_plugin_browser::CdpClient;

    let ws_url = mock_cdp_server(|req| {
        // Echo back the method name as the result
        json!({ "method": req["method"] })
    })
    .await;

    let client = CdpClient::connect(&ws_url).await.unwrap();

    let (r1, r2, r3) = tokio::join!(
        client.send("Page.enable", json!({})),
        client.send("Runtime.enable", json!({})),
        client.send("Network.enable", json!({})),
    );

    assert_eq!(r1.unwrap()["method"], "Page.enable");
    assert_eq!(r2.unwrap()["method"], "Runtime.enable");
    assert_eq!(r3.unwrap()["method"], "Network.enable");
}

#[tokio::test]
async fn error_response_propagates_as_err() {
    use nexo_plugin_browser::CdpClient;
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ws_url = format!("ws://127.0.0.1:{}", addr.port());

    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let ws = accept_async(stream).await.unwrap();
            let (mut sink, mut source) = ws.split();
            if let Some(Ok(Message::Text(t))) = source.next().await {
                let req: serde_json::Value = serde_json::from_str(&t.to_string()).unwrap();
                let id = req["id"].as_u64().unwrap();
                let resp = json!({
                    "id": id,
                    "error": { "code": -32601, "message": "Method not found" }
                });
                let _ = sink.send(Message::Text(resp.to_string())).await;
            }
        }
    });

    let client = CdpClient::connect(&ws_url).await.unwrap();
    let result = client.send("Unknown.method", json!({})).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Method not found"));
}

#[tokio::test]
async fn connect_timeout_on_unreachable_host() {
    // Port that has nothing listening — should fail fast
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        nexo_plugin_browser::CdpClient::connect("ws://127.0.0.1:19999"),
    )
    .await;

    // Either times out our test wrapper or returns a connection error
    match result {
        Ok(Err(_)) => {} // connection refused — expected
        Err(_) => {}     // test timeout — acceptable
        Ok(Ok(_)) => panic!("should not connect to nothing"),
    }
}
