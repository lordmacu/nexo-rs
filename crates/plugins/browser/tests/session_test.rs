use std::sync::Arc;

use serde_json::json;
use tokio::net::TcpListener;

async fn mock_cdp_server_stateful() -> String {
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;
    use futures::{SinkExt, StreamExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let ws = accept_async(stream).await.unwrap();
            let (mut sink, mut source) = ws.split();
            while let Some(Ok(msg)) = source.next().await {
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    _ => continue,
                };
                let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                let id = req["id"].as_u64().unwrap();
                let method = req["method"].as_str().unwrap_or("");

                let result = match method {
                    "Target.attachToTarget" => json!({ "sessionId": "test-session-123" }),
                    "Runtime.evaluate" => json!({
                        "result": {
                            "type": "string",
                            "value": r#"[{"ref_id":"@e1","tag":"button","text":"Click me","type":""},{"ref_id":"@e2","tag":"input","text":"","type":"text"}]"#
                        }
                    }),
                    _ => json!({}),
                };

                let resp = json!({ "id": id, "result": result });
                let _ = sink.send(Message::Text(resp.to_string().into())).await;
            }
        }
    });

    format!("ws://127.0.0.1:{}", addr.port())
}

#[tokio::test]
async fn snapshot_generates_element_refs() {
    use agent_plugin_browser::{CdpClient, CdpSession};

    let ws_url = mock_cdp_server_stateful().await;
    let client = Arc::new(CdpClient::connect(&ws_url).await.unwrap());
    let mut session = CdpSession::new(client, "fake-target", 5000).await.unwrap();

    let snapshot = session.snapshot().await.unwrap();

    assert!(snapshot.contains("@e1"), "snapshot should contain @e1: {snapshot}");
    assert!(snapshot.contains("@e2"), "snapshot should contain @e2: {snapshot}");
    assert!(snapshot.contains("button"), "snapshot should contain button tag");
}

#[tokio::test]
async fn session_attaches_with_session_id() {
    use agent_plugin_browser::{CdpClient, CdpSession};

    let ws_url = mock_cdp_server_stateful().await;
    let client = Arc::new(CdpClient::connect(&ws_url).await.unwrap());
    // If attachToTarget succeeds, session_id is stored — just verify no panic
    let _session = CdpSession::new(client, "fake-target", 5000).await.unwrap();
}
