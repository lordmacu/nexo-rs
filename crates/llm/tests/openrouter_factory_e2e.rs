//! Phase 100 e2e: drive `OpenRouterFactory` through the public
//! [`LlmRegistry`] API without making real HTTP requests. Asserts
//! the registry wires the factory correctly + the resulting client
//! brands its provider/model labels for telemetry.

use nexo_config::types::agents::ModelConfig;
use nexo_config::types::llm::{LlmConfig, LlmProviderConfig, RateLimitConfig, RetryConfig};
use nexo_llm::LlmRegistry;
use std::collections::HashMap;

fn provider_cfg(api_key: &str, base_url: &str) -> LlmProviderConfig {
    LlmProviderConfig {
        api_key: api_key.to_string(),
        base_url: base_url.to_string(),
        group_id: None,
        rate_limit: RateLimitConfig::default(),
        auth: None,
        api_flavor: None,
        embedding_model: None,
        safety_settings: None,
        factory_type: None,
        api_key_secret_id: None,
    }
}

fn llm_cfg_with(provider_id: &str, p: LlmProviderConfig) -> LlmConfig {
    let mut providers = HashMap::new();
    providers.insert(provider_id.to_string(), p);
    LlmConfig {
        providers,
        retry: RetryConfig {
            max_attempts: 1,
            initial_backoff_ms: 1,
            max_backoff_ms: 1,
            backoff_multiplier: 1.0,
        },
        context_optimization: Default::default(),
        tenants: HashMap::new(),
    }
}

#[test]
fn registry_builds_openrouter_client_with_default_base_url() {
    let r = LlmRegistry::with_builtins();
    let cfg = llm_cfg_with("openrouter", provider_cfg("sk-or-test", ""));
    let model = ModelConfig {
        provider: "openrouter".into(),
        model: "anthropic/claude-opus-4-7".into(),
    };
    let client = r
        .build(&cfg, &model)
        .expect("registry should build openrouter client");
    assert_eq!(client.provider(), "openrouter");
    assert_eq!(client.model_id(), "anthropic/claude-opus-4-7");
}

#[test]
fn registry_builds_openrouter_with_legacy_v1_url() {
    // Operator copies the wrong URL into yaml — factory must
    // canonicalise rather than bail. Build path is the public proxy
    // for the canonicalize_base_url helper.
    let r = LlmRegistry::with_builtins();
    let cfg = llm_cfg_with(
        "openrouter",
        provider_cfg("sk-or-test", "https://openrouter.ai/v1"),
    );
    let model = ModelConfig {
        provider: "openrouter".into(),
        model: "openrouter/auto".into(),
    };
    let client = r
        .build(&cfg, &model)
        .expect("legacy /v1 url should canonicalise + build");
    assert_eq!(client.provider(), "openrouter");
    assert_eq!(client.model_id(), "openrouter/auto");
}

#[test]
fn registry_rejects_openrouter_double_prefix_slug() {
    let r = LlmRegistry::with_builtins();
    let cfg = llm_cfg_with("openrouter", provider_cfg("sk-or-test", ""));
    let model = ModelConfig {
        provider: "openrouter".into(),
        model: "openrouter/openrouter/anthropic/claude".into(),
    };
    let err = r.build(&cfg, &model).err().expect("expected slug error");
    assert!(
        err.to_string().contains("duplicated"),
        "error must mention duplicated prefix, got: {err}"
    );
}

#[test]
fn registry_openrouter_present_in_catalog() {
    let cat = LlmRegistry::with_builtins().catalog();
    let or = cat
        .iter()
        .find(|e| e.id == "openrouter")
        .expect("openrouter present in catalog");
    assert_eq!(or.default_base_url, "https://openrouter.ai/api/v1");
    assert_eq!(or.default_env_var, "OPENROUTER_API_KEY");
    assert!(or.models.contains(&"openrouter/auto".to_string()));
    assert!(or.supports_models_probe);
}
