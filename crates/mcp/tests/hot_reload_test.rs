//! Phase 12.8 — integration: a connected `StdioMcpClient` surfaces
//! `notifications/tools/list_changed` on its event channel, and the
//! session runtime fires the `on_tools_changed` callback after the
//! debounce window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_mcp::{
    ClientEvent, McpClient, McpServerConfig, SessionMcpRuntime, StdioMcpClient,
};
use uuid::Uuid;

fn mock_server_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock_mcp_server"))
}

fn mock_cfg(name: &str, mode: &str) -> McpServerConfig {
    let mut env = HashMap::new();
    env.insert("MOCK_MODE".to_string(), mode.to_string());
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
async fn stdio_client_emits_tools_list_changed_event() {
    let client = StdioMcpClient::connect(mock_cfg("notif", "notify_burst"))
        .await
        .expect("connect");
    let mut rx = <StdioMcpClient as McpClient>::subscribe_events(&client);

    // The `notify_burst` mode only emits on `tools/call`.
    let _ = client
        .call_tool("anything", serde_json::json!({}))
        .await
        .expect("call");

    let event = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("event recv timed out")
        .expect("event channel closed");
    assert_eq!(event, ClientEvent::ToolsListChanged);

    client.shutdown().await;
}

#[tokio::test]
async fn session_on_tools_changed_fires_on_real_notification() {
    let client = StdioMcpClient::connect(mock_cfg("notif-session", "notify_burst"))
        .await
        .expect("connect");
    let client_arc: Arc<dyn McpClient> = Arc::new(client);

    let mut map: HashMap<String, Arc<dyn McpClient>> = HashMap::new();
    map.insert("notif-session".into(), client_arc.clone());
    let rt = Arc::new(SessionMcpRuntime::new(Uuid::new_v4(), "fp".into(), map));

    let fires = Arc::new(AtomicUsize::new(0));
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let fires = fires.clone();
        let seen = seen.clone();
        rt.on_tools_changed(move |sid| {
            fires.fetch_add(1, Ordering::SeqCst);
            seen.lock().unwrap().push(sid);
        });
    }
    // Let subscribe task start.
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Trigger the server-pushed notification.
    let _ = client_arc
        .call_tool("foo", serde_json::json!({}))
        .await
        .expect("call");

    // Wait past the 200 ms debounce window.
    tokio::time::sleep(Duration::from_millis(400)).await;

    assert_eq!(fires.load(Ordering::SeqCst), 1);
    assert_eq!(seen.lock().unwrap().as_slice(), &["notif-session".to_string()]);

    rt.dispose().await;
}

#[tokio::test]
async fn session_on_resources_changed_fires_on_real_notification() {
    let client = StdioMcpClient::connect(mock_cfg("resources-session", "notify_resources"))
        .await
        .expect("connect");
    let client_arc: Arc<dyn McpClient> = Arc::new(client);

    let mut map: HashMap<String, Arc<dyn McpClient>> = HashMap::new();
    map.insert("resources-session".into(), client_arc.clone());
    let rt = Arc::new(SessionMcpRuntime::new(Uuid::new_v4(), "fp".into(), map));

    let fires = Arc::new(AtomicUsize::new(0));
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let fires = fires.clone();
        let seen = seen.clone();
        rt.on_resources_changed(move |sid| {
            fires.fetch_add(1, Ordering::SeqCst);
            seen.lock().unwrap().push(sid);
        });
    }
    tokio::time::sleep(Duration::from_millis(30)).await;

    let _ = client_arc
        .call_tool("foo", serde_json::json!({}))
        .await
        .expect("call");

    tokio::time::sleep(Duration::from_millis(400)).await;

    assert_eq!(fires.load(Ordering::SeqCst), 1);
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &["resources-session".to_string()]
    );

    rt.dispose().await;
}
