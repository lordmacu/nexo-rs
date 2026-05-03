//! Phase 82.10.l integration test — wire the production
//! `HttpLlmProviderProbe` against a stub HTTP server and an
//! in-memory `LlmYamlPatcher` mock to verify a
//! `nexo/admin/llm_providers/probe` dispatch:
//! - reads `(base_url, api_key_env)` from `llm.yaml`,
//! - resolves `std::env::var(api_key_env)`,
//! - issues `GET {base_url}/models` with bearer auth,
//! - returns a typed `{ ok, status, latency_ms, model_count? }`
//!   response.
//!
//! Captures both the happy path (200 + `data: [...]`) and the
//! 401 unauthorized path with sanitised error.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nexo_core::agent::admin_rpc::dispatcher::AdminRpcError;
use nexo_core::agent::admin_rpc::domains::llm_providers::LlmYamlPatcher;
use nexo_core::agent::admin_rpc::{AdminRpcDispatcher, CapabilitySet};
use nexo_setup::llm_provider_probe::HttpLlmProviderProbe;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn unique_env_name(suffix: &str) -> String {
    format!("NEXO_INTG_PROBE_{}_{}_KEY", std::process::id(), suffix)
}

struct FakeYaml {
    base_url: String,
    api_key_env: String,
}

impl LlmYamlPatcher for FakeYaml {
    fn list_provider_ids(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec!["minimax".into()])
    }
    fn read_provider_field(
        &self,
        _provider_id: &str,
        dotted: &str,
    ) -> anyhow::Result<Option<Value>> {
        match dotted {
            "base_url" => Ok(Some(Value::String(self.base_url.clone()))),
            "api_key_env" => Ok(Some(Value::String(self.api_key_env.clone()))),
            _ => Ok(None),
        }
    }
    fn upsert_provider_field(
        &self,
        _provider_id: &str,
        _dotted: &str,
        _value: Value,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    fn remove_provider(&self, _provider_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Minimal HTTP server that responds with `status` + `body`
/// to any request. Returns the bound port.
async fn spawn_stub(status: u16, body: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            // Drain the request line + headers so the client
            // doesn't get a connection-reset error before
            // reading the response.
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let reason = match status {
                200 => "OK",
                401 => "Unauthorized",
                _ => "Status",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
                len = body.len(),
                body = body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    port
}

#[tokio::test]
async fn end_to_end_llm_providers_probe_returns_ok_on_200() {
    let _g = ENV_LOCK.lock().unwrap();
    let env_name = unique_env_name("OK");
    std::env::set_var(&env_name, "sk-integration-test");

    let port = spawn_stub(200, r#"{"data":[{"id":"m1"},{"id":"m2"},{"id":"m3"}]}"#).await;
    // Tiny wait so the listener is ready.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let yaml = Arc::new(FakeYaml {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        api_key_env: env_name.clone(),
    });
    let probe = HttpLlmProviderProbe::new(yaml);

    let mut grants: HashMap<String, HashSet<String>> = HashMap::new();
    let mut caps_set = HashSet::new();
    caps_set.insert("llm_keys_crud".to_string());
    grants.insert("agent-creator".to_string(), caps_set);
    let capabilities = CapabilitySet::from_grants(grants);

    let dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(capabilities)
        .with_llm_provider_probe(probe);

    let result = dispatcher
        .dispatch(
            "agent-creator",
            "nexo/admin/llm_providers/probe",
            json!({"provider_id": "minimax"}),
        )
        .await;

    let value = result.result.expect("ok dispatch");
    assert_eq!(value["ok"], true, "expected ok=true, got {value}");
    assert_eq!(value["status"], 200);
    assert_eq!(value["model_count"], 3);

    std::env::remove_var(&env_name);
}

#[tokio::test]
async fn end_to_end_llm_providers_probe_returns_not_ok_on_401() {
    let _g = ENV_LOCK.lock().unwrap();
    let env_name = unique_env_name("UNAUTH");
    std::env::set_var(&env_name, "sk-integration-bad");

    let port = spawn_stub(401, r#"{"error":"invalid api key"}"#).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let yaml = Arc::new(FakeYaml {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        api_key_env: env_name.clone(),
    });
    let probe = HttpLlmProviderProbe::new(yaml);

    let mut grants: HashMap<String, HashSet<String>> = HashMap::new();
    let mut caps_set = HashSet::new();
    caps_set.insert("llm_keys_crud".to_string());
    grants.insert("agent-creator".to_string(), caps_set);
    let capabilities = CapabilitySet::from_grants(grants);

    let dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(capabilities)
        .with_llm_provider_probe(probe);

    let result = dispatcher
        .dispatch(
            "agent-creator",
            "nexo/admin/llm_providers/probe",
            json!({"provider_id": "minimax"}),
        )
        .await;

    let value = result.result.expect("ok dispatch");
    assert_eq!(value["ok"], false);
    assert_eq!(value["status"], 401);
    let err = value["error"].as_str().unwrap_or("");
    assert!(err.contains("401"), "expected error to mention 401: {err}");
    // Sanity: the cleartext API key should NOT leak.
    assert!(!err.contains("sk-integration-bad"));

    std::env::remove_var(&env_name);
}

// Silence unused import warning for AdminRpcError in case the
// module shape changes — keeps the file resilient.
#[allow(dead_code)]
fn _silence(_: AdminRpcError) {}
