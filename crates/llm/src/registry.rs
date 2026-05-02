use std::collections::HashMap;
use std::sync::Arc;

use nexo_config::types::agents::ModelConfig;
use nexo_config::types::llm::{LlmConfig, LlmProviderConfig, RetryConfig};

use crate::anthropic::AnthropicFactory;
use crate::client::LlmClient;
use crate::deepseek::DeepSeekFactory;
use crate::gemini::GeminiFactory;
use crate::minimax::MiniMaxClient;
use crate::openai_compat::OpenAiClient;

/// Builds a concrete `LlmClient` for one named provider.
///
/// Implementors live next to the client they construct (e.g. `MiniMaxFactory`
/// in `minimax.rs`) and are registered in `LlmRegistry::with_builtins`.
pub trait LlmProviderFactory: Send + Sync {
    /// Provider name as it appears in YAML config (`model.provider`,
    /// keys under `llm.providers`). Must be lowercase, low-cardinality.
    fn name(&self) -> &str;

    /// Construct a fresh client for this provider/model.
    fn build(
        &self,
        provider_cfg: &LlmProviderConfig,
        model: &str,
        retry: RetryConfig,
    ) -> anyhow::Result<Arc<dyn LlmClient>>;
}

/// In-process registry of LLM providers. Lookup is by `model.provider` name.
///
/// Adding a new provider only requires:
/// 1. Implement `LlmClient` for the new client struct.
/// 2. Implement `LlmProviderFactory` next to it.
/// 3. Register it in `with_builtins` (or via `register` from a downstream binary).
#[derive(Default)]
pub struct LlmRegistry {
    factories: HashMap<String, Box<dyn LlmProviderFactory>>,
}

impl LlmRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry pre-populated with every provider shipped in this crate.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        // Builtins are infallible — `register` only fails on duplicate name,
        // which cannot happen here because each factory has a unique literal.
        r.register(Box::new(MiniMaxFactory))
            .expect("builtin minimax factory");
        r.register(Box::new(OpenAiFactory))
            .expect("builtin openai factory");
        r.register(Box::new(AnthropicFactory))
            .expect("builtin anthropic factory");
        r.register(Box::new(GeminiFactory))
            .expect("builtin gemini factory");
        r.register(Box::new(DeepSeekFactory))
            .expect("builtin deepseek factory");
        r
    }

    pub fn register(&mut self, factory: Box<dyn LlmProviderFactory>) -> anyhow::Result<()> {
        let name = factory.name().to_string();
        if self.factories.contains_key(&name) {
            anyhow::bail!("LLM provider '{name}' already registered");
        }
        self.factories.insert(name, factory);
        Ok(())
    }

    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.factories.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Resolve `model.provider` against the registry and the YAML provider
    /// config, then build a client. Errors are loud — unknown providers do
    /// not silently fall back to anything else.
    pub fn build(
        &self,
        llm_cfg: &LlmConfig,
        agent_model: &ModelConfig,
    ) -> anyhow::Result<Arc<dyn LlmClient>> {
        // Phase 83.8.12.5 — back-compat shim. Single-tenant
        // deployments (and tests) keep calling the 2-arg form;
        // it now resolves with `tenant_id: None` which matches
        // pre-Phase 83.8.12.5 semantics exactly (only the
        // global `providers` table is consulted).
        self.build_for_tenant(llm_cfg, agent_model, None)
    }

    /// Phase 83.8.12.5 — tenant-aware sister of `build`. When
    /// `tenant_id` is `Some`, the registry looks up the
    /// provider via `LlmConfig::resolve_provider(tenant_id,
    /// name)` first — that prefers the tenant-scoped definition
    /// under `llm.yaml.tenants.<id>.providers.<name>` over the
    /// global one. Falls back to the global table when the
    /// tenant has no override (or when `tenant_id` is `None`).
    pub fn build_for_tenant(
        &self,
        llm_cfg: &LlmConfig,
        agent_model: &ModelConfig,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<Arc<dyn LlmClient>> {
        let factory = self.factories.get(&agent_model.provider).ok_or_else(|| {
            anyhow::anyhow!(
                "LLM provider '{}' not registered (known: {:?})",
                agent_model.provider,
                self.names()
            )
        })?;
        let provider_cfg = llm_cfg
            .resolve_provider(tenant_id, &agent_model.provider)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "LLM provider '{}' not present in config.providers (tenant_id: {:?})",
                    agent_model.provider,
                    tenant_id,
                )
            })?;
        factory.build(provider_cfg, &agent_model.model, llm_cfg.retry.clone())
    }
}

// ---- Builtin factories ----

pub struct MiniMaxFactory;

impl LlmProviderFactory for MiniMaxFactory {
    fn name(&self) -> &str {
        "minimax"
    }

    fn build(
        &self,
        provider_cfg: &LlmProviderConfig,
        model: &str,
        retry: RetryConfig,
    ) -> anyhow::Result<Arc<dyn LlmClient>> {
        Ok(Arc::new(MiniMaxClient::new(provider_cfg, model, retry)))
    }
}

pub struct OpenAiFactory;

impl LlmProviderFactory for OpenAiFactory {
    fn name(&self) -> &str {
        "openai"
    }

    fn build(
        &self,
        provider_cfg: &LlmProviderConfig,
        model: &str,
        retry: RetryConfig,
    ) -> anyhow::Result<Arc<dyn LlmClient>> {
        Ok(Arc::new(OpenAiClient::new(provider_cfg, model, retry)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_config::types::llm::RateLimitConfig;
    use std::collections::HashMap;

    fn provider_cfg() -> LlmProviderConfig {
        LlmProviderConfig {
            api_key: "k".into(),
            group_id: None,
            base_url: "http://example.invalid".into(),
            rate_limit: RateLimitConfig {
                requests_per_second: 1.0,
                quota_alert_threshold: Some(100),
            },
            auth: None,
            api_flavor: None,
            embedding_model: None,
            safety_settings: None,
        }
    }

    fn llm_cfg(provider_name: &str) -> LlmConfig {
        let mut providers = HashMap::new();
        providers.insert(provider_name.to_string(), provider_cfg());
        LlmConfig {
            providers,
            retry: RetryConfig {
                max_attempts: 1,
                initial_backoff_ms: 1,
                max_backoff_ms: 1,
                backoff_multiplier: 1.0,
            },
            context_optimization: Default::default(),
            tenants: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn builtins_present() {
        let r = LlmRegistry::with_builtins();
        let names = r.names();
        assert!(names.contains(&"minimax"));
        assert!(names.contains(&"openai"));
        assert!(names.contains(&"anthropic"));
        assert!(names.contains(&"gemini"));
        assert!(names.contains(&"deepseek"));
    }

    #[test]
    fn duplicate_register_errors() {
        let mut r = LlmRegistry::with_builtins();
        let err = r
            .register(Box::new(MiniMaxFactory))
            .expect_err("expected duplicate error");
        assert!(err.to_string().contains("already registered"));
    }

    #[test]
    fn build_unknown_provider_errors() {
        let r = LlmRegistry::with_builtins();
        let cfg = llm_cfg("minimax");
        let model = ModelConfig {
            provider: "nope".into(),
            model: "x".into(),
        };
        let err = r.build(&cfg, &model).err().expect("expected error");
        assert!(err.to_string().contains("not registered"));
    }

    #[test]
    fn build_provider_missing_in_config_errors() {
        let r = LlmRegistry::with_builtins();
        let cfg = llm_cfg("minimax"); // only minimax in providers map
        let model = ModelConfig {
            provider: "openai".into(),
            model: "gpt-x".into(),
        };
        let err = r.build(&cfg, &model).err().expect("expected error");
        assert!(err.to_string().contains("config.providers"));
    }

    #[test]
    fn build_minimax_returns_client() {
        let r = LlmRegistry::with_builtins();
        let cfg = llm_cfg("minimax");
        let model = ModelConfig {
            provider: "minimax".into(),
            model: "m1".into(),
        };
        let client = r.build(&cfg, &model).expect("client");
        assert_eq!(client.provider(), "minimax");
    }

    /// Phase 83.8.12.5 — `build_for_tenant` resolves the
    /// provider via tenant-first / global-fallback semantics.
    /// Sanity test: with a tenant-scoped override present,
    /// the call still returns a valid client (the tenant's
    /// provider def takes precedence).
    #[test]
    fn build_for_tenant_resolves_via_tenant_namespace_when_present() {
        use std::collections::HashMap;
        let r = LlmRegistry::with_builtins();
        // Global has minimax. Tenant `acme` overrides minimax
        // with a different api_key.
        let mut cfg = llm_cfg("minimax");
        let mut acme_providers = HashMap::new();
        acme_providers.insert("minimax".to_string(), provider_cfg());
        cfg.tenants.insert(
            "acme".to_string(),
            nexo_config::TenantLlmConfig {
                providers: acme_providers,
            },
        );
        let model = ModelConfig {
            provider: "minimax".into(),
            model: "m1".into(),
        };
        // tenant_id None → global path, equivalent to legacy build.
        let c1 = r.build_for_tenant(&cfg, &model, None).expect("global path");
        assert_eq!(c1.provider(), "minimax");
        // tenant_id Some("acme") → resolves tenant-scoped provider.
        let c2 = r
            .build_for_tenant(&cfg, &model, Some("acme"))
            .expect("tenant path");
        assert_eq!(c2.provider(), "minimax");
    }

    #[test]
    fn build_for_tenant_unknown_tenant_falls_back_to_global() {
        let r = LlmRegistry::with_builtins();
        let cfg = llm_cfg("minimax"); // no tenants block
        let model = ModelConfig {
            provider: "minimax".into(),
            model: "m1".into(),
        };
        // Unknown tenant → fall through to global. Build
        // succeeds because global has minimax.
        let c = r
            .build_for_tenant(&cfg, &model, Some("unknown-tenant"))
            .expect("fall-through to global");
        assert_eq!(c.provider(), "minimax");
    }
}
