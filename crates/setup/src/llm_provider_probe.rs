//! Phase 82.10.l production adapter — daemon-side LLM provider
//! probe. Reads `llm.yaml.providers.<id>` (or
//! `tenants.<tid>.providers.<id>` when tenant-scoped) via the
//! existing [`LlmYamlPatcher`], resolves
//! `std::env::var(api_key_env)`, and issues `GET {base_url}/models`
//! with bearer auth.
//!
//! Mirrors the microapp's own `llm_probe.rs` shape (5s timeout,
//! key redaction in error strings) but runs from the daemon's
//! network position so post-`secrets/write` propagation +
//! firewall reachability are validated end-to-end.
//!
//! Tenant scope (the `tenant_id` parameter) is ignored in v1 —
//! the adapter always reads the global table. Full tenant
//! support lands as `82.10.l.tenant`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nexo_core::agent::admin_rpc::dispatcher::AdminRpcError;
use nexo_core::agent::admin_rpc::domains::llm_providers::{
    LlmProvidersProbe, LlmYamlPatcher,
};
use nexo_tool_meta::admin::llm_providers::LlmProviderProbeResponse;

const PROBE_TIMEOUT_ENV: &str = "NEXO_LLM_PROBE_TIMEOUT_SECS";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct HttpLlmProviderProbe {
    yaml: Arc<dyn LlmYamlPatcher>,
    http: reqwest::Client,
}

impl HttpLlmProviderProbe {
    pub fn new(yaml: Arc<dyn LlmYamlPatcher>) -> Arc<Self> {
        let timeout = parse_timeout_env();
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client builds with default + timeout");
        Arc::new(Self { yaml, http })
    }
}

#[async_trait]
impl LlmProvidersProbe for HttpLlmProviderProbe {
    async fn probe(
        &self,
        provider_id: &str,
        _tenant_id: Option<&str>,
    ) -> Result<LlmProviderProbeResponse, AdminRpcError> {
        // 1. Resolve `(base_url, api_key_env)` from llm.yaml.
        //    `read_provider_field` returns `None` when missing.
        //    Tenant scope reserved for 82.10.l.tenant.
        let base_url = self
            .yaml
            .read_provider_field(provider_id, "base_url")
            .map_err(|e| AdminRpcError::Internal(e.to_string()))?
            .and_then(|v| v.as_str().map(String::from))
            .ok_or_else(|| {
                AdminRpcError::InvalidParams(format!(
                    "provider {provider_id:?} not in llm.yaml or has no base_url"
                ))
            })?;
        let api_key_env = self
            .yaml
            .read_provider_field(provider_id, "api_key_env")
            .map_err(|e| AdminRpcError::Internal(e.to_string()))?
            .and_then(|v| v.as_str().map(String::from))
            .ok_or_else(|| {
                AdminRpcError::InvalidParams(format!(
                    "provider {provider_id:?} missing api_key_env in llm.yaml"
                ))
            })?;

        // 2. Resolve env var. Empty / unset → actionable error
        //    so the wizard surfaces a clear "secrets/write
        //    didn't propagate" signal.
        let api_key = std::env::var(&api_key_env).map_err(|_| {
            AdminRpcError::InvalidParams(format!(
                "env var {api_key_env:?} not set in daemon process"
            ))
        })?;
        if api_key.is_empty() {
            return Err(AdminRpcError::InvalidParams(format!(
                "env var {api_key_env:?} is empty in daemon process"
            )));
        }

        // 3. Issue GET {base_url}/models. sanitise errors so
        //    the cleartext key never surfaces in the response.
        let url = build_models_url(&base_url);
        let started = Instant::now();
        let response = self.http.get(&url).bearer_auth(&api_key).send().await;

        let latency_ms = started.elapsed().as_millis() as u64;
        match response {
            Ok(r) => {
                let status = r.status().as_u16();
                let body = r.bytes().await.unwrap_or_default();
                let ok = (200..300).contains(&status);
                if ok {
                    Ok(LlmProviderProbeResponse {
                        ok: true,
                        status,
                        latency_ms,
                        model_count: parse_model_count(&body),
                        error: None,
                    })
                } else {
                    let raw_text = String::from_utf8_lossy(&body)
                        .chars()
                        .take(400)
                        .collect::<String>();
                    let safe = redact_key(&raw_text, &api_key);
                    Ok(LlmProviderProbeResponse {
                        ok: false,
                        status,
                        latency_ms,
                        model_count: None,
                        error: Some(format!("HTTP {status}: {safe}")),
                    })
                }
            }
            Err(e) => {
                let raw = e.to_string();
                let safe = redact_key(&raw, &api_key);
                Ok(LlmProviderProbeResponse {
                    ok: false,
                    status: 0,
                    latency_ms,
                    model_count: None,
                    error: Some(safe),
                })
            }
        }
    }
}

fn parse_timeout_env() -> Duration {
    match std::env::var(PROBE_TIMEOUT_ENV) {
        Ok(s) => s
            .parse::<u64>()
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_TIMEOUT),
        Err(_) => DEFAULT_TIMEOUT,
    }
}

fn build_models_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    format!("{trimmed}/models")
}

fn parse_model_count(body: &[u8]) -> Option<usize> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("data")?.as_array().map(|a| a.len())
}

/// Replace every occurrence of `key` (and its first 8 chars
/// as a fingerprint defence) with `<redacted>`. Cheap: most
/// error strings don't contain the key at all.
fn redact_key(haystack: &str, key: &str) -> String {
    if key.is_empty() {
        return haystack.to_string();
    }
    let mut out = haystack.replace(key, "<redacted>");
    if key.len() > 8 {
        out = out.replace(&key[..8], "<redacted>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::Mutex;

    /// `std::env::set_var` is process-global; tests serialise
    /// via this lock.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Mock that returns canned `(base_url, api_key_env)` for
    /// the only fields the probe reads.
    struct FakeYaml {
        base_url: Option<String>,
        api_key_env: Option<String>,
    }

    impl LlmYamlPatcher for FakeYaml {
        fn list_provider_ids(&self) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        fn read_provider_field(
            &self,
            _provider_id: &str,
            dotted: &str,
        ) -> anyhow::Result<Option<Value>> {
            match dotted {
                "base_url" => Ok(self.base_url.clone().map(Value::String)),
                "api_key_env" => Ok(self.api_key_env.clone().map(Value::String)),
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

    fn unique_env_name(suffix: &str) -> String {
        format!("NEXO_TEST_PROBE_{}_{}_KEY", std::process::id(), suffix)
    }

    #[tokio::test]
    async fn probe_invalid_params_when_provider_missing() {
        let probe = HttpLlmProviderProbe::new(Arc::new(FakeYaml {
            base_url: None,
            api_key_env: None,
        }));
        let err = probe.probe("nope", None).await.unwrap_err();
        match err {
            AdminRpcError::InvalidParams(msg) => assert!(msg.contains("not in llm.yaml")),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_invalid_params_when_env_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        let env_name = unique_env_name("UNSET");
        std::env::remove_var(&env_name);
        let probe = HttpLlmProviderProbe::new(Arc::new(FakeYaml {
            base_url: Some("https://example.test".into()),
            api_key_env: Some(env_name.clone()),
        }));
        let err = probe.probe("minimax", None).await.unwrap_err();
        match err {
            AdminRpcError::InvalidParams(msg) => {
                assert!(msg.contains(&env_name));
                assert!(msg.contains("not set"));
            }
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[test]
    fn redact_key_replaces_value_and_prefix() {
        let key = "sk-supersecretkey-1234567890abcdef";
        let body = format!("error: invalid token {key} (origin: foo)");
        let redacted = redact_key(&body, key);
        assert!(!redacted.contains(key));
        assert!(redacted.contains("<redacted>"));

        let prefix_only = format!("token starts with {} which is wrong", &key[..8]);
        let redacted2 = redact_key(&prefix_only, key);
        assert!(!redacted2.contains(&key[..8]));
    }

    #[test]
    fn build_models_url_handles_trailing_slash() {
        assert_eq!(
            build_models_url("https://api.minimax.chat/v1"),
            "https://api.minimax.chat/v1/models"
        );
        assert_eq!(
            build_models_url("https://api.minimax.chat/v1/"),
            "https://api.minimax.chat/v1/models"
        );
    }

    #[test]
    fn parse_model_count_returns_none_on_unexpected_shape() {
        assert_eq!(parse_model_count(b"not json"), None);
        assert_eq!(parse_model_count(br#"{"models":[]}"#), None);
        assert_eq!(parse_model_count(br#"{"data":"oops"}"#), None);
        assert_eq!(parse_model_count(br#"{"data":[{},{}]}"#), Some(2));
    }

    #[tokio::test]
    async fn probe_timeout_returns_error_under_seven_seconds() {
        let _g = ENV_LOCK.lock().unwrap();
        let env_name = unique_env_name("TIMEOUT");
        std::env::set_var(&env_name, "sk-test");

        // Bind a TCP listener that accepts but never responds —
        // the probe's 5s timeout should fire.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _accept_task = tokio::spawn(async move {
            let _ = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(15)).await;
        });

        let probe = HttpLlmProviderProbe::new(Arc::new(FakeYaml {
            base_url: Some(format!("http://{addr}/v1")),
            api_key_env: Some(env_name.clone()),
        }));
        let started = Instant::now();
        let response = probe.probe("minimax", None).await.unwrap();
        let elapsed = started.elapsed();
        assert!(!response.ok);
        assert_eq!(response.status, 0);
        assert!(response.error.is_some());
        assert!(
            elapsed < Duration::from_secs(7),
            "probe waited too long: {elapsed:?}"
        );

        std::env::remove_var(&env_name);
    }
}
