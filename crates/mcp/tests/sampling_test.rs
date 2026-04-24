//! Integration test for MCP sampling.
//!
//! The mock server is spawned in `sampling_trigger` mode; it sends a
//! `sampling/createMessage` request to the client as soon as it receives
//! `notifications/initialized`. A `FakeSamplingProvider` supplies the
//! response. The mock logs the raw response line to `MOCK_SAMPLING_LOG`
//! so the test can assert end-to-end correlation and payload shape.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use agent_mcp::sampling::{
    SamplingError, SamplingProvider, SamplingRequest, SamplingResponse, SamplingRole, StopReason,
};
use agent_mcp::{McpServerConfig, StdioMcpClient};

fn mock_server_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock_mcp_server"))
}

fn base_config(name: &str, sampling_log: &std::path::Path) -> McpServerConfig {
    let mut env = HashMap::new();
    env.insert("MOCK_MODE".to_string(), "sampling_trigger".to_string());
    env.insert(
        "MOCK_SAMPLING_LOG".to_string(),
        sampling_log.display().to_string(),
    );
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

struct FakeProvider {
    reply: String,
}

#[async_trait]
impl SamplingProvider for FakeProvider {
    async fn sample(&self, req: SamplingRequest) -> Result<SamplingResponse, SamplingError> {
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].text, "ping from mock");
        Ok(SamplingResponse {
            role: SamplingRole::Assistant,
            text: self.reply.clone(),
            model: "fake-model".into(),
            stop_reason: StopReason::EndTurn,
        })
    }
}

async fn wait_for_log(path: &std::path::Path, max_wait: Duration) -> Option<String> {
    let start = std::time::Instant::now();
    while start.elapsed() < max_wait {
        if let Ok(s) = std::fs::read_to_string(path) {
            if !s.trim().is_empty() {
                return Some(s);
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

#[tokio::test]
async fn sampling_create_message_happy_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let log = tmp.path().join("sampling.log");
    let provider: Arc<dyn SamplingProvider> = Arc::new(FakeProvider {
        reply: "pong".into(),
    });

    let client =
        StdioMcpClient::connect_with_sampling(base_config("sampling-happy", &log), Some(provider))
            .await
            .expect("connect");

    let raw = wait_for_log(&log, Duration::from_secs(3))
        .await
        .expect("mock server should have logged the sampling response");
    // The log is a JSON-RPC response: {jsonrpc, id: 9001, result:{...}}.
    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(v["id"], 9001);
    assert_eq!(v["result"]["role"], "assistant");
    assert_eq!(v["result"]["content"]["type"], "text");
    assert_eq!(v["result"]["content"]["text"], "pong");
    assert_eq!(v["result"]["model"], "fake-model");
    assert_eq!(v["result"]["stopReason"], "endTurn");

    client.shutdown().await;
}

#[tokio::test]
async fn sampling_disabled_returns_method_not_found() {
    let tmp = tempfile::TempDir::new().unwrap();
    let log = tmp.path().join("sampling.log");

    // No provider — client should reply with -32601.
    let client = StdioMcpClient::connect(base_config("sampling-off", &log))
        .await
        .expect("connect");

    let raw = wait_for_log(&log, Duration::from_secs(3))
        .await
        .expect("mock server should have logged the error response");
    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(v["id"], 9001);
    assert_eq!(v["error"]["code"], -32601);

    client.shutdown().await;
}

struct FailingProvider;

#[async_trait]
impl SamplingProvider for FailingProvider {
    async fn sample(&self, _req: SamplingRequest) -> Result<SamplingResponse, SamplingError> {
        Err(SamplingError::LlmError("fake llm failure".into()))
    }
}

#[tokio::test]
async fn sampling_llm_failure_maps_to_jsonrpc_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let log = tmp.path().join("sampling.log");

    let client = StdioMcpClient::connect_with_sampling(
        base_config("sampling-fail", &log),
        Some(Arc::new(FailingProvider) as Arc<dyn SamplingProvider>),
    )
    .await
    .expect("connect");

    let raw = wait_for_log(&log, Duration::from_secs(3))
        .await
        .expect("mock server should have logged the error response");
    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(v["id"], 9001);
    assert_eq!(v["error"]["code"], -32603);
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("fake llm failure"));

    client.shutdown().await;
}
