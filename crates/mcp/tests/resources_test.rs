//! Phase 12.5 — integration tests for `resources/list` + `resources/read`
//! against the `mock_mcp_server` binary in mode=`resources`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use agent_mcp::{McpError, McpServerConfig, StdioMcpClient};

fn mock_server_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock_mcp_server"))
}

fn config(name: &str, mode: &str) -> McpServerConfig {
    let mut env = HashMap::new();
    env.insert("MOCK_MODE".into(), mode.into());
    McpServerConfig {
        name: name.into(),
        command: mock_server_path().display().to_string(),
        env,
        connect_timeout: Duration::from_secs(5),
        initialize_timeout: Duration::from_millis(500),
        call_timeout: Duration::from_millis(500),
        shutdown_grace: Duration::from_millis(100),
        ..Default::default()
    }
}

#[tokio::test]
async fn list_resources_happy() {
    let client = StdioMcpClient::connect(config("r1", "resources"))
        .await
        .expect("connect");
    assert!(client.capabilities().resources);

    let resources = client.list_resources().await.expect("list");
    let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).collect();
    assert!(uris.contains(&"file:///readme"));
    assert!(uris.contains(&"file:///config"));
    client.shutdown().await;
}

#[tokio::test]
async fn read_resource_text() {
    let client = StdioMcpClient::connect(config("r2", "resources"))
        .await
        .expect("connect");
    let contents = client
        .read_resource("file:///readme")
        .await
        .expect("read");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].text.as_deref(), Some("hello"));
    client.shutdown().await;
}

#[tokio::test]
async fn read_resource_blob() {
    let client = StdioMcpClient::connect(config("r3", "resources"))
        .await
        .expect("connect");
    let contents = client
        .read_resource("file:///blob")
        .await
        .expect("read");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].blob.as_deref(), Some("aGVsbG8="));
    assert_eq!(contents[0].mime_type.as_deref(), Some("image/png"));
    client.shutdown().await;
}

#[tokio::test]
async fn read_resource_not_found_is_server_error() {
    let client = StdioMcpClient::connect(config("r4", "resources"))
        .await
        .expect("connect");
    let err = client
        .read_resource("file:///ghost")
        .await
        .unwrap_err();
    match err {
        McpError::ServerError { code, .. } => assert_eq!(code, -32002),
        other => panic!("unexpected: {other:?}"),
    }
    client.shutdown().await;
}

#[tokio::test]
async fn list_resources_on_server_without_capability_errors_protocol() {
    let client = StdioMcpClient::connect(config("no_res", "happy"))
        .await
        .expect("connect");
    assert!(!client.capabilities().resources);
    let err = client.list_resources().await.unwrap_err();
    match err {
        McpError::Protocol(msg) => assert!(msg.contains("resources capability")),
        other => panic!("unexpected: {other:?}"),
    }
    client.shutdown().await;
}
