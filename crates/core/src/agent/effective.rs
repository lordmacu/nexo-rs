//! Per-binding effective policy.
//!
//! At intake time, the runtime matches an inbound event against the agent's
//! `inbound_bindings` and picks the binding whose `(plugin, instance)` tuple
//! wins. [`EffectiveBindingPolicy::resolve`] then folds that binding's
//! optional overrides over the agent-level settings into the concrete set of
//! capabilities the session will use: tool allowlist, outbound allowlist,
//! skills, model, system prompt, sender rate limit, and delegate allowlist.
//!
//! Merge rules (documented in the feature spec):
//!
//! | Field                 | Strategy                                       |
//! |-----------------------|------------------------------------------------|
//! | `allowed_tools`       | replace if `Some`; `["*"]` = wildcard          |
//! | `outbound_allowlist`  | replace (whole struct)                         |
//! | `skills`              | replace                                        |
//! | `model`               | replace                                        |
//! | `system_prompt`       | agent base + `\n\n# CHANNEL ADDENDUM\n<extra>` |
//! | `sender_rate_limit`   | `Inherit` / `Disable` / `Config(cfg)` keyword  |
//! | `allowed_delegates`   | replace                                        |
//!
//! Bindings without overrides resolve to exactly the agent-level values, so
//! pre-binding YAML keeps working unchanged.

use std::sync::Arc;

use agent_config::{
    AgentConfig, InboundBinding, ModelConfig, OutboundAllowlistConfig, SenderRateLimitConfig,
    SenderRateLimitKeyword, SenderRateLimitOverride,
};

/// Concrete capability snapshot for one session attached to one binding.
///
/// Held behind an `Arc` so it can be cheaply cloned into session tasks,
/// tool handlers, and rate-limiter lookups.
#[derive(Debug, Clone)]
pub struct EffectiveBindingPolicy {
    /// Index of the matched binding in `AgentConfig::inbound_bindings`
    /// when the runtime resolved the event to a concrete binding.
    /// `None` for policies synthesised from agent-level defaults
    /// (legacy bindingless agents, delegation receive, heartbeat).
    /// Used for tracing/telemetry and as the cache key for per-binding
    /// tool registries and sender rate limiters.
    pub binding_index: Option<usize>,
    /// Replaces `AgentConfig::allowed_tools` for this session. The special
    /// value `["*"]` means "every registered tool"; any other content is
    /// matched with the usual trailing-`*` glob convention.
    pub allowed_tools: Vec<String>,
    pub outbound_allowlist: OutboundAllowlistConfig,
    pub skills: Vec<String>,
    pub model: ModelConfig,
    /// Fully composed system prompt (agent base + optional addendum).
    pub system_prompt: String,
    /// Resolved rate limit: `None` means "no per-sender cap on this
    /// binding", `Some(cfg)` means "apply `cfg`". Both the `Disable`
    /// keyword and an absent agent-level limit resolve to `None`.
    pub sender_rate_limit: Option<SenderRateLimitConfig>,
    pub allowed_delegates: Vec<String>,
}

impl EffectiveBindingPolicy {
    /// Build the effective policy for the `binding_index`-th binding of
    /// `agent`. Out-of-range indices fall back to the agent-level defaults
    /// so callers in legacy/unbound code paths can still produce a policy.
    pub fn resolve(agent: &AgentConfig, binding_index: usize) -> Self {
        let binding = agent.inbound_bindings.get(binding_index);
        Self {
            binding_index: Some(binding_index),
            allowed_tools: resolve_allowed_tools(agent, binding),
            outbound_allowlist: resolve_outbound(agent, binding),
            skills: resolve_skills(agent, binding),
            model: resolve_model(agent, binding),
            system_prompt: resolve_prompt(agent, binding),
            sender_rate_limit: resolve_rate_limit(agent, binding),
            allowed_delegates: resolve_delegates(agent, binding),
        }
    }

    /// Build a policy that simply mirrors agent-level settings, used by
    /// code paths that don't have a matched binding (delegation intake,
    /// heartbeat wake-ups, tests). `binding_index` is `None` so the
    /// cache key space for real bindings (0..N) stays disjoint from
    /// the legacy/unbound path.
    pub fn from_agent_defaults(agent: &AgentConfig) -> Self {
        Self {
            binding_index: None,
            allowed_tools: agent.allowed_tools.clone(),
            outbound_allowlist: agent.outbound_allowlist.clone(),
            skills: agent.skills.clone(),
            model: agent.model.clone(),
            system_prompt: agent.system_prompt.clone(),
            sender_rate_limit: agent.sender_rate_limit.clone(),
            allowed_delegates: agent.allowed_delegates.clone(),
        }
    }

    /// Convenience helper for the common Arc-wrapped usage.
    pub fn resolved(agent: &AgentConfig, binding_index: usize) -> Arc<Self> {
        Arc::new(Self::resolve(agent, binding_index))
    }

    /// Check whether a tool `name` is permitted by this binding's
    /// allowlist. Rules:
    /// - empty list → every tool allowed (back-compat: agents that
    ///   don't narrow the set).
    /// - `"*"` entry → every tool allowed.
    /// - pattern ending in `*` → prefix match.
    /// - anything else → exact match.
    ///
    /// Used in the LLM turn loop both to prune the tool list shown to
    /// the model and to deny execution of anything the model calls
    /// from outside the allowlist (defense-in-depth).
    pub fn tool_allowed(&self, name: &str) -> bool {
        allowlist_matches(&self.allowed_tools, name)
    }
}

/// Shared allowlist matcher used by both [`EffectiveBindingPolicy::tool_allowed`]
/// and [`crate::agent::tool_registry::ToolRegistry::retain_matching`]. Kept as
/// a free function so the exact matching semantics stay in one place and
/// cannot drift between the two call sites.
pub fn allowlist_matches(patterns: &[String], name: &str) -> bool {
    if patterns.is_empty() {
        return true;
    }
    patterns.iter().any(|p| {
        if p == "*" {
            return true;
        }
        match p.strip_suffix('*') {
            Some(stem) => name.starts_with(stem),
            None => p == name,
        }
    })
}

fn resolve_allowed_tools(agent: &AgentConfig, binding: Option<&InboundBinding>) -> Vec<String> {
    binding
        .and_then(|b| b.allowed_tools.clone())
        .unwrap_or_else(|| agent.allowed_tools.clone())
}

fn resolve_outbound(
    agent: &AgentConfig,
    binding: Option<&InboundBinding>,
) -> OutboundAllowlistConfig {
    binding
        .and_then(|b| b.outbound_allowlist.clone())
        .unwrap_or_else(|| agent.outbound_allowlist.clone())
}

fn resolve_skills(agent: &AgentConfig, binding: Option<&InboundBinding>) -> Vec<String> {
    binding
        .and_then(|b| b.skills.clone())
        .unwrap_or_else(|| agent.skills.clone())
}

fn resolve_model(agent: &AgentConfig, binding: Option<&InboundBinding>) -> ModelConfig {
    binding
        .and_then(|b| b.model.clone())
        .unwrap_or_else(|| agent.model.clone())
}

fn resolve_prompt(agent: &AgentConfig, binding: Option<&InboundBinding>) -> String {
    let base = agent.system_prompt.clone();
    let Some(extra) = binding
        .and_then(|b| b.system_prompt_extra.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return base;
    };
    if base.is_empty() {
        format!("# CHANNEL ADDENDUM\n{extra}")
    } else {
        // The addendum is a separate block so the agent-level prompt
        // (personality, hard rules) stays visually distinct from the
        // channel-specific add-on.
        format!("{}\n\n# CHANNEL ADDENDUM\n{}", base.trim_end(), extra)
    }
}

fn resolve_rate_limit(
    agent: &AgentConfig,
    binding: Option<&InboundBinding>,
) -> Option<SenderRateLimitConfig> {
    match binding.map(|b| &b.sender_rate_limit) {
        None | Some(SenderRateLimitOverride::Keyword(SenderRateLimitKeyword::Inherit)) => {
            agent.sender_rate_limit.clone()
        }
        Some(SenderRateLimitOverride::Keyword(SenderRateLimitKeyword::Disable)) => None,
        Some(SenderRateLimitOverride::Config(cfg)) => Some(cfg.clone()),
    }
}

fn resolve_delegates(agent: &AgentConfig, binding: Option<&InboundBinding>) -> Vec<String> {
    binding
        .and_then(|b| b.allowed_delegates.clone())
        .unwrap_or_else(|| agent.allowed_delegates.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_config::{
        AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, SenderRateLimitConfig, SenderRateLimitKeyword,
        SenderRateLimitOverride, WorkspaceGitConfig,
    };

    fn sample_agent() -> AgentConfig {
        AgentConfig {
            id: "ana".into(),
            model: ModelConfig {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
            },
            plugins: vec!["whatsapp".into(), "telegram".into()],
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: "You are Ana.".into(),
            workspace: String::new(),
            skills: vec!["weather".into()],
            skills_dir: "./skills".into(),
            transcripts_dir: String::new(),
            dreaming: DreamingYamlConfig::default(),
            workspace_git: WorkspaceGitConfig::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: vec!["whatsapp_send_message".into()],
            sender_rate_limit: Some(SenderRateLimitConfig {
                rps: 1.0,
                burst: 5,
            }),
            allowed_delegates: vec!["peer_a".into()],
            accept_delegates_from: Vec::new(),
            description: String::new(),
            google_auth: None,
            credentials: Default::default(),
            outbound_allowlist: OutboundAllowlistConfig {
                whatsapp: vec!["573000000000".into()],
                telegram: Vec::new(),
            },
        }
    }

    fn legacy_binding() -> InboundBinding {
        InboundBinding {
            plugin: "whatsapp".into(),
            ..Default::default()
        }
    }

    #[test]
    fn legacy_binding_inherits_everything() {
        let mut a = sample_agent();
        a.inbound_bindings.push(legacy_binding());
        let eff = EffectiveBindingPolicy::resolve(&a, 0);
        assert_eq!(eff.allowed_tools, a.allowed_tools);
        assert_eq!(eff.skills, a.skills);
        assert_eq!(eff.allowed_delegates, a.allowed_delegates);
        assert_eq!(eff.model.provider, a.model.provider);
        assert_eq!(eff.system_prompt, a.system_prompt);
        assert_eq!(eff.sender_rate_limit.as_ref().unwrap().rps, 1.0);
        assert_eq!(eff.outbound_allowlist.whatsapp, a.outbound_allowlist.whatsapp);
    }

    #[test]
    fn allowed_tools_override_replaces() {
        let mut a = sample_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            allowed_tools: Some(vec!["*".into()]),
            ..Default::default()
        });
        let eff = EffectiveBindingPolicy::resolve(&a, 0);
        assert_eq!(eff.allowed_tools, vec!["*".to_string()]);
    }

    #[test]
    fn skills_override_replaces() {
        let mut a = sample_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            skills: Some(vec!["browser".into(), "github".into()]),
            ..Default::default()
        });
        let eff = EffectiveBindingPolicy::resolve(&a, 0);
        assert_eq!(eff.skills, vec!["browser".to_string(), "github".to_string()]);
    }

    #[test]
    fn model_override_replaces() {
        let mut a = sample_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            model: Some(ModelConfig {
                provider: "anthropic".into(),
                model: "claude-sonnet-4-5".into(),
            }),
            ..Default::default()
        });
        let eff = EffectiveBindingPolicy::resolve(&a, 0);
        assert_eq!(eff.model.model, "claude-sonnet-4-5");
    }

    #[test]
    fn outbound_allowlist_override_replaces() {
        let mut a = sample_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            outbound_allowlist: Some(OutboundAllowlistConfig {
                whatsapp: Vec::new(),
                telegram: vec![42],
            }),
            ..Default::default()
        });
        let eff = EffectiveBindingPolicy::resolve(&a, 0);
        assert!(eff.outbound_allowlist.whatsapp.is_empty());
        assert_eq!(eff.outbound_allowlist.telegram, vec![42]);
    }

    #[test]
    fn system_prompt_extra_appends_addendum_block() {
        let mut a = sample_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            system_prompt_extra: Some("Private Telegram.".into()),
            ..Default::default()
        });
        let eff = EffectiveBindingPolicy::resolve(&a, 0);
        assert!(eff.system_prompt.starts_with("You are Ana."));
        assert!(eff.system_prompt.contains("# CHANNEL ADDENDUM"));
        assert!(eff.system_prompt.contains("Private Telegram."));
    }

    #[test]
    fn system_prompt_extra_whitespace_only_is_ignored() {
        let mut a = sample_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            system_prompt_extra: Some("   \n  ".into()),
            ..Default::default()
        });
        let eff = EffectiveBindingPolicy::resolve(&a, 0);
        assert_eq!(eff.system_prompt, "You are Ana.");
        assert!(!eff.system_prompt.contains("CHANNEL ADDENDUM"));
    }

    #[test]
    fn rate_limit_inherit_keeps_agent_value() {
        let mut a = sample_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            sender_rate_limit: SenderRateLimitOverride::Keyword(SenderRateLimitKeyword::Inherit),
            ..Default::default()
        });
        let eff = EffectiveBindingPolicy::resolve(&a, 0);
        assert!(eff.sender_rate_limit.is_some());
        assert_eq!(eff.sender_rate_limit.unwrap().rps, 1.0);
    }

    #[test]
    fn rate_limit_disable_clears_agent_value() {
        let mut a = sample_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            sender_rate_limit: SenderRateLimitOverride::Keyword(SenderRateLimitKeyword::Disable),
            ..Default::default()
        });
        let eff = EffectiveBindingPolicy::resolve(&a, 0);
        assert!(eff.sender_rate_limit.is_none());
    }

    #[test]
    fn rate_limit_config_replaces() {
        let mut a = sample_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            sender_rate_limit: SenderRateLimitOverride::Config(SenderRateLimitConfig {
                rps: 0.1,
                burst: 1,
            }),
            ..Default::default()
        });
        let eff = EffectiveBindingPolicy::resolve(&a, 0);
        let rl = eff.sender_rate_limit.unwrap();
        assert_eq!(rl.rps, 0.1);
        assert_eq!(rl.burst, 1);
    }

    #[test]
    fn allowed_delegates_override_replaces() {
        let mut a = sample_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            allowed_delegates: Some(vec!["*".into()]),
            ..Default::default()
        });
        let eff = EffectiveBindingPolicy::resolve(&a, 0);
        assert_eq!(eff.allowed_delegates, vec!["*".to_string()]);
    }

    #[test]
    fn out_of_range_binding_index_falls_back_to_agent_defaults() {
        let a = sample_agent();
        let eff = EffectiveBindingPolicy::resolve(&a, 99);
        // Out-of-range index quietly yields agent-level values (callers in
        // unbound paths rely on this).
        assert_eq!(eff.allowed_tools, a.allowed_tools);
        assert_eq!(eff.skills, a.skills);
    }

    #[test]
    fn from_agent_defaults_uses_none_as_binding_sentinel() {
        let a = sample_agent();
        let eff = EffectiveBindingPolicy::from_agent_defaults(&a);
        assert_eq!(eff.binding_index, None);
        assert_eq!(eff.allowed_tools, a.allowed_tools);
    }
}
