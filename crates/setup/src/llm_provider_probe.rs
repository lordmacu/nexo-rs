//! Daemon-side LLM provider probe adapter.
//! Reads `llm.yaml.providers.<id>` (or
//! `tenants.<tid>.providers.<id>` when tenant-scoped) via the
//! existing [`LlmYamlPatcher`], resolves
//! `std::env::var(api_key_env)`, and issues `GET {base_url}/models`.
//!
//! Per-factory auth shape:
//! * **OpenAI-compat** (minimax, openai, deepseek, gemini): bearer
//!   auth, `data[].id` parser. Default path.
//! * **Anthropic**: builds an [`AnthropicAuth`] from the yaml
//!   (api_key / setup_token / oauth_bundle), calls
//!   `auth.resolve_headers(http)` for the right header set
//!   (`x-api-key` for legacy keys, `Authorization: Bearer` +
//!   `anthropic-beta: oauth-2025-04-20` for OAuth subscription),
//!   adds `anthropic-version: 2023-06-01`, then `GET /v1/models`
//!   parses the same `data[].id` shape.
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
use nexo_config::types::llm::{LlmAuthConfig, LlmProviderConfig, RateLimitConfig};
use nexo_core::agent::admin_rpc::dispatcher::AdminRpcError;
use nexo_core::agent::admin_rpc::domains::llm_providers::{LlmProvidersProbe, LlmYamlPatcher};
use nexo_llm::anthropic::resolve_auth as resolve_anthropic_auth;
use nexo_tool_meta::admin::llm_providers::{
    AuthMode, LlmProviderProbeDraftInput, LlmProviderProbeResponse,
};

const ANTHROPIC_FACTORY_ID: &str = "anthropic";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";

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
        // Resolve `base_url` (always required). `factory_type`
        // determines auth shape — falls back to `provider_id` when
        // absent (legacy single-instance-per-factory yamls).
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
        let factory_id = self
            .yaml
            .read_provider_field(provider_id, "factory_type")
            .map_err(|e| AdminRpcError::Internal(e.to_string()))?
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| provider_id.to_string());

        if factory_id == ANTHROPIC_FACTORY_ID {
            let cfg = self.read_anthropic_cfg(provider_id, &base_url)?;
            return self.probe_anthropic(&base_url, &cfg).await;
        }

        // OpenAI-compat (minimax / openai / deepseek / gemini).
        // Resolve env var. Empty / unset → actionable error so
        // the wizard surfaces a clear "secrets/write didn't
        // propagate" signal.
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
        self.probe_bearer(&base_url, &api_key, None).await
    }

    /// Draft probe. Operator-supplied
    /// `(factory_type, base_url, auth_mode, fields)` payload; no
    /// yaml, no env var lookup. Defensive: every error path
    /// returns `Ok(_)` with `ok: false` + sanitised hint so the
    /// wizard renders a usable diagnostic. Network errors only
    /// become `Err(_)` when the input itself is malformed —
    /// those bail before any HTTP call.
    async fn probe_draft(
        &self,
        draft: LlmProviderProbeDraftInput,
    ) -> Result<LlmProviderProbeResponse, AdminRpcError> {
        let base_url = draft.base_url.trim();
        if base_url.is_empty() {
            return Err(AdminRpcError::InvalidParams(
                "draft.base_url is required".into(),
            ));
        }
        let factory_id = draft.factory_type.trim();

        if factory_id == ANTHROPIC_FACTORY_ID {
            // OAuth flows don't carry the bundle in `fields` (it
            // gets persisted by `oauth_finish` to disk + secrets
            // store). Pre-upsert there's no instance entry yet,
            // so the probe can't reach the bundle. Return ok=true
            // with empty `model_names` → frontend falls back to
            // the static `known_models` catalog at line 222-226 of
            // LlmInstanceCreateModal.tsx.
            let auth_mode = draft.auth_mode.unwrap_or(AuthMode::ApiKey);
            match auth_mode {
                AuthMode::OAuthAuthCode
                | AuthMode::OAuthDeviceCode
                | AuthMode::OAuthBundleImport => {
                    return Ok(LlmProviderProbeResponse {
                        ok: true,
                        status: 0,
                        latency_ms: 0,
                        model_count: None,
                        model_names: None,
                        error: None,
                    });
                }
                _ => {}
            }
            let cfg = build_anthropic_draft_cfg(base_url, &auth_mode, &draft.fields)?;
            return self.probe_anthropic(base_url, &cfg).await;
        }

        // OpenAI-compat — bearer + optional MiniMax group header.
        let api_key = draft
            .fields
            .get("api_key")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AdminRpcError::InvalidParams(
                    "draft.fields.api_key is required (empty / missing)".into(),
                )
            })?;
        let group_id = draft
            .fields
            .get("group_id")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.probe_bearer(base_url, &api_key, group_id.as_deref())
            .await
    }
}

impl HttpLlmProviderProbe {
    async fn probe_bearer(
        &self,
        base_url: &str,
        api_key: &str,
        minimax_group_id: Option<&str>,
    ) -> Result<LlmProviderProbeResponse, AdminRpcError> {
        let mut request = self
            .http
            .get(build_models_url(base_url))
            .bearer_auth(api_key);
        if let Some(g) = minimax_group_id {
            request = request.header("X-MiniMax-Group-Id", g);
        }
        let started = Instant::now();
        let response = request.send().await;
        let latency_ms = started.elapsed().as_millis() as u64;
        Ok(into_probe_response(response, latency_ms, api_key).await)
    }

    /// Anthropic probe. Builds an [`AnthropicAuth`] from the cfg
    /// (api_key / setup_token / oauth_bundle / cli_import / auto)
    /// via `nexo_llm::anthropic::resolve_auth`, asks
    /// `resolve_headers(http)` for the right header set (refreshes
    /// OAuth access token if near expiry), adds
    /// `anthropic-version: 2023-06-01`, then `GET /v1/models`.
    async fn probe_anthropic(
        &self,
        base_url: &str,
        cfg: &LlmProviderConfig,
    ) -> Result<LlmProviderProbeResponse, AdminRpcError> {
        // Auth resolution may fail (bundle file missing / invalid
        // setup-token / etc). Wrap as `ok: false` so the wizard
        // surfaces a clear hint instead of bubbling up as 500.
        let auth = match resolve_anthropic_auth(cfg) {
            Ok(a) => a,
            Err(e) => {
                return Ok(LlmProviderProbeResponse {
                    ok: false,
                    status: 0,
                    latency_ms: 0,
                    model_count: None,
                    model_names: None,
                    error: Some(format!("anthropic auth: {e}")),
                });
            }
        };
        let headers = match auth.resolve_headers(&self.http).await {
            Ok(h) => h,
            Err(e) => {
                return Ok(LlmProviderProbeResponse {
                    ok: false,
                    status: 0,
                    latency_ms: 0,
                    model_count: None,
                    model_names: None,
                    error: Some(format!("anthropic header resolution: {e}")),
                });
            }
        };

        let mut request = self
            .http
            .get(build_models_url(base_url))
            .header(headers.auth.0, &headers.auth.1)
            .header("anthropic-version", ANTHROPIC_API_VERSION);
        if let Some(beta) = headers.beta {
            request = request.header("anthropic-beta", beta);
        }
        for (k, v) in &headers.extra {
            request = request.header(*k, v);
        }
        let started = Instant::now();
        let response = request.send().await;
        let latency_ms = started.elapsed().as_millis() as u64;
        // Redact the secret part of the auth header so error
        // strings never leak the API key / access token.
        let redact_secret = headers.auth.1.clone();
        Ok(into_probe_response(response, latency_ms, &redact_secret).await)
    }

    fn read_anthropic_cfg(
        &self,
        provider_id: &str,
        base_url: &str,
    ) -> Result<LlmProviderConfig, AdminRpcError> {
        let read = |dotted: &str| -> Result<Option<String>, AdminRpcError> {
            Ok(self
                .yaml
                .read_provider_field(provider_id, dotted)
                .map_err(|e| AdminRpcError::Internal(e.to_string()))?
                .and_then(|v| v.as_str().map(String::from)))
        };
        let api_key = read("api_key")?.unwrap_or_default();
        let api_key_env = read("api_key_env")?;
        // Inline `api_key` wins; fall back to env var if yaml
        // points at one (legacy single-tenant style).
        let api_key = if !api_key.is_empty() {
            api_key
        } else if let Some(env) = api_key_env {
            std::env::var(&env).unwrap_or_default()
        } else {
            String::new()
        };

        let auth_mode = read("auth.mode")?;
        let auth = auth_mode.map(|mode| LlmAuthConfig {
            mode,
            bundle: read("auth.bundle").ok().flatten(),
            setup_token_file: read("auth.setup_token_file").ok().flatten(),
            refresh_endpoint: read("auth.refresh_endpoint").ok().flatten(),
            client_id: read("auth.client_id").ok().flatten(),
        });

        Ok(LlmProviderConfig {
            api_key,
            base_url: base_url.to_string(),
            factory_type: Some(ANTHROPIC_FACTORY_ID.to_string()),
            api_key_secret_id: None,
            group_id: None,
            rate_limit: RateLimitConfig::default(),
            auth,
            api_flavor: None,
            embedding_model: None,
            safety_settings: None,
        })
    }
}

/// Build a synthetic [`LlmProviderConfig`] from a draft probe
/// payload. Used by `probe_draft` for Anthropic api_key /
/// setup_token modes (OAuth bundles aren't carried in `fields`,
/// so OAuth modes short-circuit to "fall back to catalog" before
/// reaching this helper).
fn build_anthropic_draft_cfg(
    base_url: &str,
    auth_mode: &AuthMode,
    fields: &std::collections::BTreeMap<String, String>,
) -> Result<LlmProviderConfig, AdminRpcError> {
    let api_key = fields
        .get("api_key")
        .or_else(|| fields.get("setup_token"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AdminRpcError::InvalidParams(
                "draft.fields.api_key (or setup_token) is required for anthropic".into(),
            )
        })?;
    let mode = match auth_mode {
        AuthMode::SetupToken => "setup_token".to_string(),
        _ => "api_key".to_string(),
    };
    Ok(LlmProviderConfig {
        api_key,
        base_url: base_url.to_string(),
        factory_type: Some(ANTHROPIC_FACTORY_ID.to_string()),
        api_key_secret_id: None,
        group_id: None,
        rate_limit: RateLimitConfig::default(),
        auth: Some(LlmAuthConfig {
            mode,
            bundle: None,
            setup_token_file: None,
            refresh_endpoint: None,
            client_id: None,
        }),
        api_flavor: None,
        embedding_model: None,
        safety_settings: None,
    })
}

/// Shared response builder. `redact_secret` is the cleartext
/// fragment to scrub from any error body before it surfaces to
/// the operator (api key, OAuth access token, setup-token).
async fn into_probe_response(
    response: Result<reqwest::Response, reqwest::Error>,
    latency_ms: u64,
    redact_secret: &str,
) -> LlmProviderProbeResponse {
    match response {
        Ok(r) => {
            let status = r.status().as_u16();
            let body = r.bytes().await.unwrap_or_default();
            let ok = (200..300).contains(&status);
            if ok {
                let parsed = parse_models_payload(&body);
                LlmProviderProbeResponse {
                    ok: true,
                    status,
                    latency_ms,
                    model_count: parsed.count,
                    model_names: parsed.names,
                    error: None,
                }
            } else {
                let raw_text = String::from_utf8_lossy(&body)
                    .chars()
                    .take(400)
                    .collect::<String>();
                let safe = redact_key(&raw_text, redact_secret);
                LlmProviderProbeResponse {
                    ok: false,
                    status,
                    latency_ms,
                    model_count: None,
                    model_names: None,
                    error: Some(format!("HTTP {status}: {safe}")),
                }
            }
        }
        Err(e) => {
            let safe = redact_key(&e.to_string(), redact_secret);
            LlmProviderProbeResponse {
                ok: false,
                status: 0,
                latency_ms,
                model_count: None,
                model_names: None,
                error: Some(safe),
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

/// Combined parse for model count + names from
/// an OpenAI-compat `/v1/models` payload (`{"data":[{"id": "..."}, ...]}`).
/// Returns `count = None` when the body isn't JSON or lacks `data`,
/// `names = None` when no `data[].id` strings could be extracted.
/// Names capped at 200 to bound RPC payload — paranoid against a
/// pathological provider that returns 50k variants. Order preserved
/// from the wire so the operator's UI can show "newest first" by
/// provider convention.
struct ParsedModels {
    count: Option<usize>,
    names: Option<Vec<String>>,
}

fn parse_models_payload(body: &[u8]) -> ParsedModels {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let data = parsed
        .as_ref()
        .and_then(|v| v.get("data"))
        .and_then(|d| d.as_array());
    let count = data.map(|a| a.len());
    let names = data.map(|arr| {
        arr.iter()
            .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from))
            .take(200)
            .collect::<Vec<_>>()
    });
    // Drop names when the parse extracted zero — UIs interpret
    // `Some(empty)` as "no models" rather than "fall back to
    // catalog", so distinguish "parsed but empty" from "couldn't
    // parse" by making both look like "fall back to catalog".
    let names = names.filter(|v| !v.is_empty());
    ParsedModels { count, names }
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
    fn parse_models_payload_handles_all_shapes() {
        // Garbage / non-JSON → both None.
        let p = parse_models_payload(b"not json");
        assert!(p.count.is_none());
        assert!(p.names.is_none());

        // No `data` field → both None (Anthropic / Gemini shapes).
        let p = parse_models_payload(br#"{"models":[]}"#);
        assert!(p.count.is_none());
        assert!(p.names.is_none());

        // `data` present but wrong type → both None.
        let p = parse_models_payload(br#"{"data":"oops"}"#);
        assert!(p.count.is_none());
        assert!(p.names.is_none());

        // `data` empty array → count=0, names=None (treated as
        // "fall back to catalog" rather than "no models").
        let p = parse_models_payload(br#"{"data":[]}"#);
        assert_eq!(p.count, Some(0));
        assert!(p.names.is_none());

        // Objects without `id` strings → count=2, names=None.
        let p = parse_models_payload(br#"{"data":[{},{}]}"#);
        assert_eq!(p.count, Some(2));
        assert!(p.names.is_none());

        // Happy path — OpenAI-compat with three model ids.
        let p = parse_models_payload(
            br#"{"data":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"},{"id":"gpt-4-turbo"}]}"#,
        );
        assert_eq!(p.count, Some(3));
        assert_eq!(
            p.names.as_deref(),
            Some(
                &[
                    "gpt-4o".to_string(),
                    "gpt-4o-mini".to_string(),
                    "gpt-4-turbo".to_string(),
                ][..]
            )
        );
    }

    #[test]
    fn parse_models_payload_caps_names_at_200() {
        // Defensive against a pathological provider returning
        // thousands of variants. Build 250-item payload, expect
        // count=250, names truncated to 200.
        let entries: Vec<String> = (0..250).map(|i| format!(r#"{{"id":"m-{i}"}}"#)).collect();
        let body = format!(r#"{{"data":[{}]}}"#, entries.join(","));
        let p = parse_models_payload(body.as_bytes());
        assert_eq!(p.count, Some(250));
        let names = p.names.expect("names populated");
        assert_eq!(names.len(), 200);
        assert_eq!(names[0], "m-0");
        assert_eq!(names[199], "m-199");
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
