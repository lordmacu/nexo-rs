use std::collections::HashMap;
use std::sync::{Arc, RwLock};

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

    /// Default `base_url` to suggest when an operator-side UI builds
    /// a fresh `llm.yaml.providers.<name>` block. Empty string when
    /// the factory has no canonical default (rare — most providers
    /// have one well-known endpoint). Used by the
    /// `nexo/admin/llm_providers/catalog` admin RPC so SPA wizards
    /// don't have to hardcode it.
    fn default_base_url(&self) -> &'static str {
        ""
    }

    /// Conventional env var that holds the API key for this
    /// provider (e.g. `MINIMAX_API_KEY`). Empty when the factory
    /// doesn't enforce a default.
    fn default_env_var(&self) -> &'static str {
        ""
    }

    /// Curated list of model ids this factory accepts. Operator UIs
    /// render these as a dropdown, locked-down so the operator
    /// can't pick a model the framework can't actually dispatch
    /// against this provider's API. Empty when the factory hasn't
    /// declared a list yet — UIs fall back to a free-text input
    /// in that case.
    fn known_models(&self) -> &'static [&'static str] {
        &[]
    }
}

/// Catalogue entry surfaced by [`LlmRegistry::catalog`]. Aggregates
/// per-factory metadata (default base URL, env var, known models)
/// so SPA wizards can render a strict provider/model dropdown
/// without keeping their own hardcoded list in sync.
#[derive(Debug, Clone)]
pub struct LlmProviderCatalogEntry {
    pub id: String,
    pub default_base_url: String,
    pub default_env_var: String,
    pub models: Vec<String>,
}

/// In-process registry of LLM providers. Lookup is by `model.provider` name.
///
/// Adding a new provider only requires:
/// 1. Implement `LlmClient` for the new client struct.
/// 2. Implement `LlmProviderFactory` next to it.
/// 3. Register it in `with_builtins` (or via `register` from a downstream binary).
///
/// Phase 81.25 — `factories` lives behind an internal `RwLock` so
/// post-Arc registration works (subprocess plugins declaring
/// `extends.llm_providers = [...]` register during init via the
/// shared `Arc<LlmRegistry>`). Read paths (`build` / `names`)
/// take a read lock per call; writes (`register` / `unregister`)
/// take a write lock briefly.
#[derive(Default)]
pub struct LlmRegistry {
    factories: RwLock<HashMap<String, Box<dyn LlmProviderFactory>>>,
}

impl LlmRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry pre-populated with every provider shipped in this crate.
    pub fn with_builtins() -> Self {
        let r = Self::new();
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

    /// Register a factory. Phase 81.25 — `&self` (was `&mut self`)
    /// so callers holding `Arc<LlmRegistry>` can register
    /// post-construction. Internal `RwLock` write-locks briefly.
    pub fn register(&self, factory: Box<dyn LlmProviderFactory>) -> anyhow::Result<()> {
        let name = factory.name().to_string();
        let mut guard = self
            .factories
            .write()
            .unwrap_or_else(|p| p.into_inner());
        if guard.contains_key(&name) {
            anyhow::bail!("LLM provider '{name}' already registered");
        }
        guard.insert(name, factory);
        Ok(())
    }

    /// Phase 81.25 — remove a factory by provider name. Used by
    /// the remote-LLM rollback path on partial registration
    /// failure. Returns `true` when removed; `false` when absent.
    pub fn unregister(&self, name: &str) -> bool {
        let mut guard = self
            .factories
            .write()
            .unwrap_or_else(|p| p.into_inner());
        guard.remove(name).is_some()
    }

    pub fn names(&self) -> Vec<String> {
        let guard = self
            .factories
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let mut names: Vec<String> = guard.keys().cloned().collect();
        names.sort_unstable();
        names
    }

    /// Snapshot of every registered factory's metadata. Sorted alpha
    /// by provider id for deterministic RPC payloads. Returns the
    /// providers the daemon can actually instantiate at runtime —
    /// builtins shipped in this crate plus any subprocess plugin
    /// that called `register` post-Arc (Phase 81.25). Operator UIs
    /// use this to render a "no custom" provider dropdown.
    pub fn catalog(&self) -> Vec<LlmProviderCatalogEntry> {
        let guard = self
            .factories
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<LlmProviderCatalogEntry> = guard
            .values()
            .map(|f| LlmProviderCatalogEntry {
                id: f.name().to_string(),
                default_base_url: f.default_base_url().to_string(),
                default_env_var: f.default_env_var().to_string(),
                models: f
                    .known_models()
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
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
        // Phase 82.10.s — split provider INSTANCE id (yaml key,
        // also `agent_model.provider`) from FACTORY id (the
        // `LlmProviderFactory::name()` we dispatch against). The
        // yaml's `factory_type` field is the explicit binding;
        // when absent, the instance id IS the factory id —
        // back-compat with single-instance-per-factory yamls.
        //
        // Resolution order (robust, fail-fast at every step):
        //   1. yaml: instance id → LlmProviderConfig (loud miss).
        //   2. provider_cfg.factory_type or instance_id → factory id.
        //   3. registry: factory id → factory (loud miss with the
        //      list of known factories so the operator can fix the
        //      typo without spelunking).
        let provider_cfg = llm_cfg
            .resolve_provider(tenant_id, &agent_model.provider)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "LLM provider instance '{}' not present in config.providers (tenant_id: {:?})",
                    agent_model.provider,
                    tenant_id,
                )
            })?;
        let factory_id = provider_cfg
            .factory_type
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&agent_model.provider);
        let guard = self
            .factories
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let factory = guard.get(factory_id).ok_or_else(|| {
            anyhow::anyhow!(
                "LLM provider instance '{}' references factory '{}' which is not registered (known: {:?})",
                agent_model.provider,
                factory_id,
                self.names_locked(&guard)
            )
        })?;
        factory.build(provider_cfg, &agent_model.model, llm_cfg.retry.clone())
    }

    /// Helper used inside `build_for_tenant` so we don't double-
    /// lock the RwLock when constructing the diagnostic.
    fn names_locked(
        &self,
        guard: &HashMap<String, Box<dyn LlmProviderFactory>>,
    ) -> Vec<String> {
        let mut names: Vec<String> = guard.keys().cloned().collect();
        names.sort_unstable();
        names
    }

    /// Phase 82.10.s — boot-time sanity check across every yaml
    /// provider instance (global + tenant-scoped). Each instance's
    /// resolved factory id (explicit `factory_type` or fallback to
    /// instance id) MUST be a registered factory; otherwise the
    /// instance can't dispatch agent traffic at runtime.
    ///
    /// Robust by design:
    /// - Walks ALL instances, collecting EVERY error in one pass —
    ///   operators see the full broken set in a single boot output
    ///   instead of fix-restart-loop. Mirrors
    ///   `LlmConfig::resolve_all_keys` from step .1.
    /// - Returns sorted, deterministic error vec for stable CI
    ///   diffing.
    /// - Empty `factory_type: Some("")` is treated as absent so a
    ///   yaml typo doesn't deceptively pass under "no factory_type
    ///   means use instance id".
    /// - Single read-lock on the factories table for the entire
    ///   walk; fast even with hundreds of instances.
    pub fn validate_config(&self, llm_cfg: &LlmConfig) -> Result<(), Vec<String>> {
        let guard = self
            .factories
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let mut errors: Vec<String> = Vec::new();
        let known: Vec<String> = self.names_locked(&guard);

        let check = |scope: &str,
                     instance_id: &str,
                     cfg: &LlmProviderConfig,
                     errors: &mut Vec<String>| {
            let factory_id = cfg
                .factory_type
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(instance_id);
            if !guard.contains_key(factory_id) {
                let prefix = if scope.is_empty() {
                    format!("provider '{instance_id}'")
                } else {
                    format!("provider '{scope}.{instance_id}'")
                };
                let factory_clause = if cfg
                    .factory_type
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .is_some()
                {
                    format!("references factory_type '{factory_id}'")
                } else {
                    format!(
                        "has no factory_type and id '{factory_id}' is not a registered factory"
                    )
                };
                errors.push(format!(
                    "{prefix} {factory_clause} (known factories: {known:?})"
                ));
            }
        };

        for (id, cfg) in &llm_cfg.providers {
            check("", id, cfg, &mut errors);
        }
        for (tenant_id, t) in &llm_cfg.tenants {
            let scope = format!("tenants.{tenant_id}");
            for (id, cfg) in &t.providers {
                check(&scope, id, cfg, &mut errors);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            errors.sort();
            Err(errors)
        }
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

    fn default_base_url(&self) -> &'static str {
        "https://api.minimax.chat/v1"
    }

    fn default_env_var(&self) -> &'static str {
        "MINIMAX_API_KEY"
    }

    fn known_models(&self) -> &'static [&'static str] {
        &["MiniMax-M2.5", "MiniMax-M2.7"]
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

    fn default_base_url(&self) -> &'static str {
        "https://api.openai.com/v1"
    }

    fn default_env_var(&self) -> &'static str {
        "OPENAI_API_KEY"
    }

    fn known_models(&self) -> &'static [&'static str] {
        &["gpt-4o", "gpt-4o-mini", "gpt-4-turbo"]
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
            safety_settings: None, factory_type: None, api_key_secret_id: None,
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
        assert!(names.iter().any(|n| n == "minimax"));
        assert!(names.iter().any(|n| n == "openai"));
        assert!(names.iter().any(|n| n == "anthropic"));
        assert!(names.iter().any(|n| n == "gemini"));
        assert!(names.iter().any(|n| n == "deepseek"));
    }

    #[test]
    fn duplicate_register_errors() {
        let r = LlmRegistry::with_builtins();
        let err = r
            .register(Box::new(MiniMaxFactory))
            .expect_err("expected duplicate error");
        assert!(err.to_string().contains("already registered"));
    }

    #[test]
    fn unregister_removes_factory() {
        let r = LlmRegistry::with_builtins();
        assert!(r.unregister("minimax"));
        assert!(!r.names().iter().any(|n| n == "minimax"));
        assert!(!r.unregister("absent_provider"));
    }

    #[test]
    fn build_unknown_provider_errors() {
        // Phase 82.10.s.2 — message moved from "not registered"
        // (factory-side) to "not present in config.providers"
        // (yaml-side) since the build flow now checks yaml first.
        let r = LlmRegistry::with_builtins();
        let cfg = llm_cfg("minimax");
        let model = ModelConfig {
            provider: "nope".into(),
            model: "x".into(),
        };
        let err = r.build(&cfg, &model).err().expect("expected error");
        assert!(
            err.to_string().contains("not present in config.providers"),
            "got: {err}"
        );
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

    // ────────────────────────────────────────────────────────────
    // Phase 82.10.s.2 — factory_type ↔ instance_id split
    // ────────────────────────────────────────────────────────────

    /// Build an `LlmConfig` with a single instance whose YAML key
    /// can differ from its factory_type.
    fn cfg_with_instance(
        instance_id: &str,
        factory_type: Option<&str>,
    ) -> LlmConfig {
        let mut cfg = provider_cfg();
        cfg.factory_type = factory_type.map(String::from);
        let mut providers = HashMap::new();
        providers.insert(instance_id.to_string(), cfg);
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
    fn build_uses_factory_type_when_set() {
        let r = LlmRegistry::with_builtins();
        // Instance "minimax-cliente-a" → factory "minimax".
        let cfg = cfg_with_instance("minimax-cliente-a", Some("minimax"));
        let model = ModelConfig {
            provider: "minimax-cliente-a".into(),
            model: "m1".into(),
        };
        let c = r.build(&cfg, &model).expect("factory_type drives lookup");
        assert_eq!(c.provider(), "minimax");
    }

    #[test]
    fn build_falls_back_to_instance_id_when_factory_type_none() {
        let r = LlmRegistry::with_builtins();
        // Legacy yaml: provider id IS factory id, no factory_type.
        let cfg = cfg_with_instance("minimax", None);
        let model = ModelConfig {
            provider: "minimax".into(),
            model: "m1".into(),
        };
        r.build(&cfg, &model).expect("legacy path");
    }

    #[test]
    fn build_treats_blank_factory_type_as_absent() {
        let r = LlmRegistry::with_builtins();
        // Defensive: empty string in factory_type must NOT match
        // an empty-string factory id. Falls back to the instance id.
        let cfg = cfg_with_instance("minimax", Some(""));
        let model = ModelConfig {
            provider: "minimax".into(),
            model: "m1".into(),
        };
        r.build(&cfg, &model).expect("blank factory_type -> instance_id fallback");
    }

    #[test]
    fn build_loud_when_factory_type_unknown() {
        let r = LlmRegistry::with_builtins();
        let cfg = cfg_with_instance("custom", Some("nonexistent-factory"));
        let model = ModelConfig {
            provider: "custom".into(),
            model: "m1".into(),
        };
        let err = match r.build(&cfg, &model) {
            Ok(_) => panic!("expected unknown-factory error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("custom"));
        assert!(err.contains("nonexistent-factory"));
        assert!(err.contains("known:"));
    }

    #[test]
    fn build_loud_when_instance_missing_from_yaml() {
        let r = LlmRegistry::with_builtins();
        let cfg = cfg_with_instance("minimax", None);
        let model = ModelConfig {
            // Agent points to instance not in providers/.
            provider: "ghost-instance".into(),
            model: "m1".into(),
        };
        let err = match r.build(&cfg, &model) {
            Ok(_) => panic!("expected missing-instance error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("ghost-instance"));
        assert!(err.contains("not present in config.providers"));
    }

    #[test]
    fn validate_config_accepts_legacy_yaml() {
        let r = LlmRegistry::with_builtins();
        let cfg = cfg_with_instance("minimax", None);
        r.validate_config(&cfg).expect("legacy yaml ok");
    }

    #[test]
    fn validate_config_accepts_explicit_factory_type() {
        let r = LlmRegistry::with_builtins();
        let cfg = cfg_with_instance("minimax-cliente-a", Some("minimax"));
        r.validate_config(&cfg).expect("explicit factory_type ok");
    }

    #[test]
    fn validate_config_collects_all_unknown_factories() {
        let r = LlmRegistry::with_builtins();
        let mut cfg = LlmConfig {
            providers: HashMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                initial_backoff_ms: 1,
                max_backoff_ms: 1,
                backoff_multiplier: 1.0,
            },
            context_optimization: Default::default(),
            tenants: HashMap::new(),
        };
        // 1: legacy id matches factory ✓
        cfg.providers
            .insert("minimax".to_string(), provider_cfg());
        // 2: factory_type unknown ✗
        let mut a = provider_cfg();
        a.factory_type = Some("nope".to_string());
        cfg.providers.insert("custom-a".to_string(), a);
        // 3: no factory_type, instance id not a factory ✗
        cfg.providers
            .insert("not-a-factory".to_string(), provider_cfg());

        let errs = r.validate_config(&cfg).unwrap_err();
        assert_eq!(errs.len(), 2);
        // Both errors must mention their respective instance ids.
        let joined = errs.join("\n");
        assert!(joined.contains("custom-a"));
        assert!(joined.contains("not-a-factory"));
        // Errors are sorted for deterministic CI output.
        let mut sorted = errs.clone();
        sorted.sort();
        assert_eq!(errs, sorted);
    }

    #[test]
    fn validate_config_walks_tenants() {
        let r = LlmRegistry::with_builtins();
        let mut cfg = LlmConfig {
            providers: HashMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                initial_backoff_ms: 1,
                max_backoff_ms: 1,
                backoff_multiplier: 1.0,
            },
            context_optimization: Default::default(),
            tenants: HashMap::new(),
        };
        let mut t = nexo_config::TenantLlmConfig::default();
        let mut bad = provider_cfg();
        bad.factory_type = Some("ghost".to_string());
        t.providers.insert("custom".to_string(), bad);
        cfg.tenants.insert("acme".to_string(), t);
        let errs = r.validate_config(&cfg).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("tenants.acme.custom"));
        assert!(errs[0].contains("ghost"));
    }
}
