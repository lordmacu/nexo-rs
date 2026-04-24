//! Integration tests for Phase 12.1 — `StdioMcpClient` against the
//! `mock_mcp_server` example binary.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use agent_mcp::{McpError, McpServerConfig, StdioMcpClient};

fn mock_server_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock_mcp_server"))
}

fn base_config(name: &str, mode: &str) -> McpServerConfig {
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
async fn happy_path_connect_list_call() {
    let client = StdioMcpClient::connect(base_config("happy", "happy"))
        .await
        .expect("connect");

    assert_eq!(client.server_info().name, "mock");
    assert!(client.capabilities().tools);

    let tools = client.list_tools().await.expect("list");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"ping"));

    let result = client
        .call_tool("echo", serde_json::json!({"text": "hi"}))
        .await
        .expect("call");
    assert!(!result.is_error);
    assert_eq!(result.content.len(), 1);

    client.shutdown().await;
}

#[tokio::test]
async fn initialize_timeout_surfaces() {
    match StdioMcpClient::connect(base_config("silent", "silent_initialize")).await {
        Ok(_) => panic!("expected initialize timeout, got ready client"),
        Err(McpError::InitializeTimeout(_)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn list_tools_consolidates_pages() {
    let client = StdioMcpClient::connect(base_config("paginate", "paginate"))
        .await
        .expect("connect");
    let tools = client.list_tools().await.expect("list");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "beta"]);
    client.shutdown().await;
}

#[tokio::test]
async fn tool_error_surfaces_as_ok_with_is_error() {
    let client = StdioMcpClient::connect(base_config("tool_error", "tool_error"))
        .await
        .expect("connect");
    let r = client
        .call_tool("anything", serde_json::json!({}))
        .await
        .expect("call");
    assert!(r.is_error);
    client.shutdown().await;
}

#[tokio::test]
async fn jsonrpc_error_maps_to_server_error() {
    let client = StdioMcpClient::connect(base_config("server_error", "server_error"))
        .await
        .expect("connect");
    let err = client
        .call_tool("anything", serde_json::json!({}))
        .await
        .expect_err("call should fail");
    match err {
        McpError::ServerError { code, ref message } => {
            assert_eq!(code, -32001);
            assert!(message.contains("forbidden"));
        }
        other => panic!("unexpected: {other:?}"),
    }
    client.shutdown().await;
}

#[tokio::test]
async fn notification_during_call_does_not_break() {
    let client = StdioMcpClient::connect(base_config("notify_burst", "notify_burst"))
        .await
        .expect("connect");
    let r = client
        .call_tool("anything", serde_json::json!({}))
        .await
        .expect("call");
    assert!(!r.is_error);
    assert_eq!(r.content.len(), 1);
    client.shutdown().await;
}

#[tokio::test]
async fn shutdown_sends_cancelled_for_pending() {
    use std::sync::Arc;
    let log = tempfile::NamedTempFile::new().expect("tmp");
    let log_path = log.path().to_path_buf();

    let mut cfg = base_config("slow", "slow_call");
    cfg.env.insert(
        "MOCK_CANCELLED_LOG".into(),
        log_path.display().to_string(),
    );
    cfg.call_timeout = Duration::from_secs(2);
    cfg.shutdown_grace = Duration::from_millis(200);

    let client = Arc::new(StdioMcpClient::connect(cfg).await.expect("connect"));

    let call = {
        let client = client.clone();
        tokio::spawn(async move {
            // Best-effort: may return ok (mock finishes) or an error
            // depending on timing. We only care that the notif was sent.
            let _ = client.call_tool("anything", serde_json::json!({})).await;
        })
    };

    // Give the child time to read the request before we shut down,
    // otherwise `pending` could be empty (we shut down before send).
    tokio::time::sleep(Duration::from_millis(100)).await;
    client.shutdown().await;
    let _ = call.await;

    // Allow any pending flushing.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let contents = std::fs::read_to_string(&log_path).expect("read log");
    let lines: Vec<&str> = contents.lines().filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty(), "no cancelled notif written: {contents:?}");
    let parsed: serde_json::Value =
        serde_json::from_str(lines[0]).expect("json parse");
    assert_eq!(parsed["method"], "notifications/cancelled");
    assert!(parsed["params"]["requestId"].is_u64(), "{parsed}");
    assert_eq!(parsed["params"]["reason"], "client shutdown");
}

#[tokio::test]
async fn shutdown_with_reason_sends_custom_reason() {
    use std::sync::Arc;
    let log = tempfile::NamedTempFile::new().expect("tmp");
    let log_path = log.path().to_path_buf();

    let mut cfg = base_config("slow-reason", "slow_call");
    cfg.env.insert(
        "MOCK_CANCELLED_LOG".into(),
        log_path.display().to_string(),
    );
    cfg.call_timeout = Duration::from_secs(2);
    cfg.shutdown_grace = Duration::from_millis(200);

    let client = Arc::new(StdioMcpClient::connect(cfg).await.expect("connect"));

    let call = {
        let client = client.clone();
        tokio::spawn(async move {
            let _ = client.call_tool("anything", serde_json::json!({})).await;
        })
    };

    tokio::time::sleep(Duration::from_millis(100)).await;
    client.shutdown_with_reason("sigterm").await;
    let _ = call.await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let contents = std::fs::read_to_string(&log_path).expect("read log");
    let lines: Vec<&str> = contents.lines().filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty(), "no cancelled notif written");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("json");
    assert_eq!(parsed["params"]["reason"], "sigterm");
}

#[tokio::test(flavor = "current_thread")]
async fn log_notification_from_mock_is_observable() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::{fmt, EnvFilter};

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    impl<'a> MakeWriter<'a> for SharedBuf {
        type Writer = SharedBuf;
        fn make_writer(&'a self) -> Self::Writer { self.clone() }
    }

    let buf = SharedBuf::default();
    let subscriber = fmt::Subscriber::builder()
        .with_env_filter(EnvFilter::new("info"))
        .with_writer(buf.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let client = StdioMcpClient::connect(base_config("log-mock", "emit_log"))
        .await
        .expect("connect");
    let _ = client
        .call_tool("anything", serde_json::json!({}))
        .await
        .expect("call");
    tokio::time::sleep(Duration::from_millis(100)).await;
    client.shutdown().await;

    let captured = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(
        captured.contains("mcp log"),
        "expected `mcp log` event in captured output. Got:\n{captured}"
    );
    assert!(
        captured.contains("tool invoked"),
        "expected `tool invoked` data in captured output. Got:\n{captured}"
    );
    assert!(
        captured.contains("log-mock"),
        "expected mcp field 'log-mock' in captured output. Got:\n{captured}"
    );
}

#[tokio::test]
async fn tools_call_with_meta_is_forwarded_to_server() {
    let client = StdioMcpClient::connect(base_config("meta-echo", "echo_meta"))
        .await
        .expect("connect");
    let result = client
        .call_tool_with_meta(
            "echo",
            serde_json::json!({"x": 1}),
            Some(serde_json::json!({"agent_id": "kate", "session_id": "abc"})),
        )
        .await
        .expect("call");
    assert!(!result.is_error);
    let text = match &result.content[0] {
        agent_mcp::McpContent::Text { text } => text.clone(),
        other => panic!("unexpected: {other:?}"),
    };
    let echoed: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(echoed["agent_id"], "kate");
    assert_eq!(echoed["session_id"], "abc");
    client.shutdown().await;
}

#[tokio::test]
async fn tools_call_without_meta_omits_the_field() {
    let client = StdioMcpClient::connect(base_config("meta-echo-none", "echo_meta"))
        .await
        .expect("connect");
    let result = client
        .call_tool("echo", serde_json::json!({"x": 1}))
        .await
        .expect("call");
    let text = match &result.content[0] {
        agent_mcp::McpContent::Text { text } => text.clone(),
        other => panic!("unexpected: {other:?}"),
    };
    // Server echoes "null" when no _meta was sent.
    assert_eq!(text, "null");
    client.shutdown().await;
}

#[tokio::test]
async fn set_log_level_applies_and_server_acks() {
    let log = tempfile::NamedTempFile::new().expect("tmp");
    let log_path = log.path().to_path_buf();

    let mut cfg = base_config("setlevel", "logging_capable");
    cfg.env.insert(
        "MOCK_SETLEVEL_LOG".into(),
        log_path.display().to_string(),
    );
    cfg.log_level = Some("warning".into());

    let client = StdioMcpClient::connect(cfg).await.expect("connect");
    // Auto-applied in connect(). Also verify manual call works.
    client
        .set_log_level("error")
        .await
        .expect("set level manually");
    client.shutdown().await;

    let contents = std::fs::read_to_string(&log_path).expect("read log");
    let lines: Vec<&str> = contents.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines.contains(&"warning"), "missing 'warning' in {lines:?}");
    assert!(lines.contains(&"error"), "missing 'error' in {lines:?}");
}

#[tokio::test]
async fn set_log_level_fails_without_capability() {
    let client = StdioMcpClient::connect(base_config("no-logging", "happy"))
        .await
        .expect("connect");
    match client.set_log_level("warning").await {
        Err(McpError::Protocol(msg)) => {
            assert!(msg.contains("logging capability"), "unexpected msg: {msg}");
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }
    client.shutdown().await;
}

#[tokio::test]
async fn set_log_level_rejects_invalid_level() {
    let client = StdioMcpClient::connect(base_config("no-logging-2", "logging_capable"))
        .await
        .expect("connect");
    match client.set_log_level("warn").await {
        Err(McpError::Protocol(msg)) => assert!(msg.contains("invalid log level")),
        other => panic!("expected Protocol error, got {other:?}"),
    }
    client.shutdown().await;
}

#[tokio::test]
async fn update_config_hot_reloads_log_level_on_live_client() {
    use agent_mcp::runtime_config::{McpRuntimeConfig, McpServerRuntimeConfig};
    use agent_mcp::McpRuntimeManager;
    use uuid::Uuid;

    let log = tempfile::NamedTempFile::new().expect("tmp");
    let log_path = log.path().to_path_buf();

    let mut initial = base_config("setlevel-hot", "logging_capable");
    initial.env.insert(
        "MOCK_SETLEVEL_LOG".into(),
        log_path.display().to_string(),
    );
    initial.log_level = Some("warning".into());

    let mgr = McpRuntimeManager::new(McpRuntimeConfig {
        servers: vec![McpServerRuntimeConfig::Stdio(initial.clone())],
        session_ttl: Duration::from_secs(60),
        idle_reap_interval: Duration::from_secs(1),
        reset_level_on_unset: false,
        default_reset_level: "info".into(),
        resource_cache: Default::default(),
        resource_uri_allowlist: Vec::new(),
    });
    let _ = mgr.get_or_create(Uuid::nil()).await;

    // Let the initial setLevel land.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Swap config with a new level.
    let mut bumped = initial.clone();
    bumped.log_level = Some("error".into());
    mgr.update_config(McpRuntimeConfig {
        servers: vec![McpServerRuntimeConfig::Stdio(bumped)],
        session_ttl: Duration::from_secs(60),
        idle_reap_interval: Duration::from_secs(1),
        reset_level_on_unset: false,
        default_reset_level: "info".into(),
        resource_cache: Default::default(),
        resource_uri_allowlist: Vec::new(),
    })
    .await;

    // Give the spawned setLevel task time to complete.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let contents = std::fs::read_to_string(&log_path).expect("read log");
    let lines: Vec<&str> = contents.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines.contains(&"warning"), "missing initial level: {lines:?}");
    assert!(lines.contains(&"error"), "missing hot-reloaded level: {lines:?}");

    mgr.shutdown_all().await;
}

#[tokio::test]
async fn list_resources_with_meta_forwards_to_server() {
    let client = StdioMcpClient::connect(base_config("res-meta-list", "echo_meta"))
        .await
        .expect("connect");
    let resources = client
        .list_resources_with_meta(Some(serde_json::json!({"agent_id": "kate"})))
        .await
        .expect("list");
    // Mock echoes _meta into the first resource uri.
    assert!(!resources.is_empty());
    assert!(resources[0].uri.contains("agent_id"));
    client.shutdown().await;
}

#[tokio::test]
async fn read_resource_with_meta_forwards_to_server() {
    let client = StdioMcpClient::connect(base_config("res-meta-read", "echo_meta"))
        .await
        .expect("connect");
    let contents = client
        .read_resource_with_meta(
            "any://uri",
            Some(serde_json::json!({"agent_id": "kate", "session_id": "abc"})),
        )
        .await
        .expect("read");
    let text = contents[0].text.clone().unwrap_or_default();
    assert!(text.contains("kate"), "missing agent in echo: {text}");
    assert!(text.contains("abc"), "missing session in echo: {text}");
    client.shutdown().await;
}

#[tokio::test]
async fn update_config_resets_level_to_info_when_flag_on_and_unset() {
    use agent_mcp::runtime_config::{McpRuntimeConfig, McpServerRuntimeConfig};
    use agent_mcp::McpRuntimeManager;
    use uuid::Uuid;

    let log = tempfile::NamedTempFile::new().expect("tmp");
    let log_path = log.path().to_path_buf();

    let mut initial = base_config("reset-level", "logging_capable");
    initial.env.insert(
        "MOCK_SETLEVEL_LOG".into(),
        log_path.display().to_string(),
    );
    initial.log_level = Some("warning".into());

    let mgr = McpRuntimeManager::new(McpRuntimeConfig {
        servers: vec![McpServerRuntimeConfig::Stdio(initial.clone())],
        session_ttl: Duration::from_secs(60),
        idle_reap_interval: Duration::from_secs(1),
        reset_level_on_unset: true,
        default_reset_level: "info".into(),
        resource_cache: Default::default(),
        resource_uri_allowlist: Vec::new(),
    });
    let _ = mgr.get_or_create(Uuid::nil()).await;
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Hot reload removing log_level; flag reset_level_on_unset=true should
    // push "info" to the live client.
    let mut bumped = initial.clone();
    bumped.log_level = None;
    mgr.update_config(McpRuntimeConfig {
        servers: vec![McpServerRuntimeConfig::Stdio(bumped)],
        session_ttl: Duration::from_secs(60),
        idle_reap_interval: Duration::from_secs(1),
        reset_level_on_unset: true,
        default_reset_level: "info".into(),
        resource_cache: Default::default(),
        resource_uri_allowlist: Vec::new(),
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let contents = std::fs::read_to_string(&log_path).expect("read log");
    let lines: Vec<&str> = contents.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines.contains(&"warning"), "missing initial level: {lines:?}");
    assert!(lines.contains(&"info"), "missing reset-to-info: {lines:?}");

    mgr.shutdown_all().await;
}

#[tokio::test]
async fn empty_name_rejected() {
    let mut cfg = base_config("", "happy");
    cfg.name = String::new();
    match StdioMcpClient::connect(cfg).await {
        Ok(_) => panic!("expected Protocol error"),
        Err(McpError::Protocol(_)) => {}
        Err(other) => panic!("unexpected: {other:?}"),
    }
}
