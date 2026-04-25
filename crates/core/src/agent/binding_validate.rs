//! Boot-time validation for per-binding capability overrides.
//!
//! The runtime can only enforce what the boot path has sanity-checked. This
//! module catches the obvious misconfigurations before any agent task
//! spawns, so a typo in a YAML file surfaces as a clear error at startup
//! instead of a silent capability drift during operation.
//!
//! Validations (hard errors):
//! 1. Duplicate `(plugin, instance)` tuples inside one agent's bindings.
//! 2. A binding references a Telegram `instance` that is not declared in
//!    the telegram plugin config.
//! 3. A binding's `allowed_tools` lists a tool name that is not
//!    registered anywhere in the system (when the caller supplies the
//!    known-tools catalogue).
//! 4. A binding's `skills` references a skill that does not exist on
//!    disk under the agent's `skills_dir`.
//!
//! Soft signals (tracing warnings, not errors):
//! 5. A binding that sets no overrides at all — the YAML likely meant to
//!    narrow capabilities but forgot. We still boot; we just log a warn.
//!
//! The hard checks are intentionally cheap (pure data, no I/O beyond the
//! skill-directory stat) so the full bootstrap cost stays negligible.

use std::collections::HashSet;
use std::path::Path;

use nexo_config::{AgentConfig, TelegramPluginConfig};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BindingValidationError {
    #[error(
        "agent '{agent}': duplicate binding (plugin='{plugin}', instance={instance}) \
         — each (plugin, instance) pair must appear at most once"
    )]
    DuplicateBinding {
        agent: String,
        plugin: String,
        instance: String,
    },

    #[error(
        "agent '{agent}' binding[{index}]: plugin='telegram' instance='{instance}' is not \
         declared in config/plugins/telegram.yaml (known instances: {known})"
    )]
    UnknownTelegramInstance {
        agent: String,
        index: usize,
        instance: String,
        known: String,
    },

    #[error(
        "agent '{agent}' binding[{index}]: allowed_tools entry '{tool}' does not match any \
         registered tool (known tools: {known})"
    )]
    UnknownTool {
        agent: String,
        index: usize,
        tool: String,
        known: String,
    },

    #[error(
        "agent '{agent}' binding[{index}]: skill '{skill}' not found under skills_dir '{dir}'"
    )]
    UnknownSkill {
        agent: String,
        index: usize,
        skill: String,
        dir: String,
    },

    #[error(
        "agent '{agent}' binding[{index}]: model override provider '{binding_provider}' \
         does not match the agent provider '{agent_provider}'. Per-binding model switching is \
         only supported within the same provider (the LLM client is wired once per agent)."
    )]
    ProviderMismatch {
        agent: String,
        index: usize,
        binding_provider: String,
        agent_provider: String,
    },

    #[error("agent '{agent}': unknown LLM provider '{provider}' (known: {known})")]
    UnknownProvider {
        agent: String,
        provider: String,
        known: String,
    },
}

/// Known-tools catalogue used by [`validate_agents`]. An empty set turns
/// off the `allowed_tools` check (useful in tests and early-boot flows
/// where the full tool registry is not yet assembled).
#[derive(Debug, Default, Clone)]
pub struct KnownTools<'a> {
    names: HashSet<&'a str>,
}

/// Known-providers catalogue used by [`validate_agents`]. An empty set
/// turns off the provider check (same rationale as `KnownTools`: the
/// LLM registry is populated at boot alongside the config load).
#[derive(Debug, Default, Clone)]
pub struct KnownProviders<'a> {
    names: HashSet<&'a str>,
}

impl<'a> KnownProviders<'a> {
    pub fn new<I>(names: I) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        Self {
            names: names.into_iter().collect(),
        }
    }

    fn is_enabled(&self) -> bool {
        !self.names.is_empty()
    }

    fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    fn listed(&self) -> String {
        let mut v: Vec<&&str> = self.names.iter().collect();
        v.sort();
        v.iter().copied().copied().collect::<Vec<_>>().join(", ")
    }
}

impl<'a> KnownTools<'a> {
    pub fn new<I>(names: I) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        Self {
            names: names.into_iter().collect(),
        }
    }

    fn is_enabled(&self) -> bool {
        !self.names.is_empty()
    }

    fn contains(&self, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        // Tolerate the trailing-'*' glob convention used elsewhere in the
        // codebase (`memory_*`, `google_*`). The glob matches as long as
        // at least one registered tool shares the prefix.
        if let Some(prefix) = pattern.strip_suffix('*') {
            return self.names.iter().any(|t| t.starts_with(prefix));
        }
        self.names.contains(pattern)
    }

    fn listed(&self) -> String {
        let mut v: Vec<&&str> = self.names.iter().collect();
        v.sort();
        v.iter().copied().copied().collect::<Vec<_>>().join(", ")
    }
}

/// Validate every agent's bindings against the surrounding config and
/// return **every** error found, not just the first. Callers that only
/// want to fail boot on the first problem can check `.is_empty()`; the
/// aggregate form exists so operators fixing multi-agent configs see
/// the full list of problems in one pass instead of restart-and-repeat.
pub fn collect_binding_errors(
    agents: &[AgentConfig],
    telegram_instances: &[TelegramPluginConfig],
    known_tools: &KnownTools<'_>,
) -> Vec<BindingValidationError> {
    collect_binding_errors_with_providers(
        agents,
        telegram_instances,
        known_tools,
        &KnownProviders::default(),
    )
}

/// Same as [`collect_binding_errors`] but also validates that every
/// agent's `model.provider` (and any binding-level `model.provider`
/// override) is present in `known_providers`. Intended for the boot
/// path, where the LLM registry already knows which providers are
/// wired in.
pub fn collect_binding_errors_with_providers(
    agents: &[AgentConfig],
    telegram_instances: &[TelegramPluginConfig],
    known_tools: &KnownTools<'_>,
    known_providers: &KnownProviders<'_>,
) -> Vec<BindingValidationError> {
    let mut errors = Vec::new();
    for agent in agents {
        validate_agent_into(
            agent,
            telegram_instances,
            known_tools,
            known_providers,
            &mut errors,
        );
    }
    errors
}

/// Validate every agent's bindings. Returns `Ok(())` when the
/// aggregate error list is empty, otherwise an [`anyhow::Error`]
/// whose `Display` includes every error separated by `\n  - ` so the
/// operator sees each problem on its own line.
pub fn validate_agents(
    agents: &[AgentConfig],
    telegram_instances: &[TelegramPluginConfig],
    known_tools: &KnownTools<'_>,
) -> anyhow::Result<()> {
    validate_agents_with_providers(
        agents,
        telegram_instances,
        known_tools,
        &KnownProviders::default(),
    )
}

pub fn validate_agents_with_providers(
    agents: &[AgentConfig],
    telegram_instances: &[TelegramPluginConfig],
    known_tools: &KnownTools<'_>,
    known_providers: &KnownProviders<'_>,
) -> anyhow::Result<()> {
    let errors = collect_binding_errors_with_providers(
        agents,
        telegram_instances,
        known_tools,
        known_providers,
    );
    if errors.is_empty() {
        return Ok(());
    }
    let mut msg = format!(
        "per-binding override validation failed ({} issue{}):",
        errors.len(),
        if errors.len() == 1 { "" } else { "s" },
    );
    for e in &errors {
        msg.push_str("\n  - ");
        msg.push_str(&e.to_string());
    }
    Err(anyhow::anyhow!(msg))
}

/// Validate a single agent and return the first error. Kept for
/// call sites that prefer early-exit semantics and for the unit test
/// suite; the boot path uses [`validate_agents`] which aggregates.
pub fn validate_agent(
    agent: &AgentConfig,
    telegram_instances: &[TelegramPluginConfig],
    known_tools: &KnownTools<'_>,
) -> Result<(), BindingValidationError> {
    let mut errors = Vec::new();
    validate_agent_into(
        agent,
        telegram_instances,
        known_tools,
        &KnownProviders::default(),
        &mut errors,
    );
    if let Some(first) = errors.into_iter().next() {
        Err(first)
    } else {
        Ok(())
    }
}

fn validate_agent_into(
    agent: &AgentConfig,
    telegram_instances: &[TelegramPluginConfig],
    known_tools: &KnownTools<'_>,
    known_providers: &KnownProviders<'_>,
    errors: &mut Vec<BindingValidationError>,
) {
    // 0. Known provider check (agent-level + any binding-level override).
    if known_providers.is_enabled() {
        if !known_providers.contains(&agent.model.provider) {
            errors.push(BindingValidationError::UnknownProvider {
                agent: agent.id.clone(),
                provider: agent.model.provider.clone(),
                known: known_providers.listed(),
            });
        }
        for b in &agent.inbound_bindings {
            let Some(m) = &b.model else { continue };
            if !known_providers.contains(&m.provider) {
                errors.push(BindingValidationError::UnknownProvider {
                    agent: agent.id.clone(),
                    provider: m.provider.clone(),
                    known: known_providers.listed(),
                });
            }
        }
    }

    // 1. Duplicate bindings.
    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
    for b in &agent.inbound_bindings {
        let key = (b.plugin.clone(), b.instance.clone());
        if !seen.insert(key.clone()) {
            errors.push(BindingValidationError::DuplicateBinding {
                agent: agent.id.clone(),
                plugin: b.plugin.clone(),
                instance: b.instance.clone().unwrap_or_else(|| "<wildcard>".into()),
            });
        }
    }

    // 2. Telegram instances referenced by bindings must exist.
    //    A binding without an instance matches any telegram bot (wildcard)
    //    and does not require declared instances.
    for (idx, b) in agent.inbound_bindings.iter().enumerate() {
        if b.plugin != "telegram" {
            continue;
        }
        let Some(inst) = b.instance.as_deref() else {
            continue;
        };
        let declared = telegram_instances
            .iter()
            .any(|t| t.instance.as_deref() == Some(inst));
        if !declared {
            let known = telegram_instances
                .iter()
                .filter_map(|t| t.instance.clone())
                .collect::<Vec<_>>()
                .join(", ");
            errors.push(BindingValidationError::UnknownTelegramInstance {
                agent: agent.id.clone(),
                index: idx,
                instance: inst.to_string(),
                known: if known.is_empty() {
                    "<none>".into()
                } else {
                    known
                },
            });
        }
    }

    // 3. Unknown tool names. Skipped if the caller didn't supply a
    //    catalogue (known_tools is empty).
    if known_tools.is_enabled() {
        for (idx, b) in agent.inbound_bindings.iter().enumerate() {
            let Some(list) = b.allowed_tools.as_ref() else {
                continue;
            };
            for tool in list {
                if !known_tools.contains(tool) {
                    errors.push(BindingValidationError::UnknownTool {
                        agent: agent.id.clone(),
                        index: idx,
                        tool: tool.clone(),
                        known: known_tools.listed(),
                    });
                }
            }
        }
    }

    // 4. Model override must keep the same provider.
    for (idx, b) in agent.inbound_bindings.iter().enumerate() {
        let Some(m) = &b.model else { continue };
        if m.provider != agent.model.provider {
            errors.push(BindingValidationError::ProviderMismatch {
                agent: agent.id.clone(),
                index: idx,
                binding_provider: m.provider.clone(),
                agent_provider: agent.model.provider.clone(),
            });
        }
    }

    // 5. Skills exist on disk.
    for (idx, b) in agent.inbound_bindings.iter().enumerate() {
        let Some(skills) = b.skills.as_ref() else {
            continue;
        };
        for skill in skills {
            let skill_dir = Path::new(&agent.skills_dir).join(skill);
            if !skill_dir.is_dir() {
                errors.push(BindingValidationError::UnknownSkill {
                    agent: agent.id.clone(),
                    index: idx,
                    skill: skill.clone(),
                    dir: agent.skills_dir.clone(),
                });
            }
        }
    }

    // 6. Soft warn: binding with no overrides at all.
    for (idx, b) in agent.inbound_bindings.iter().enumerate() {
        if !has_any_override(b) {
            tracing::warn!(
                agent = %agent.id,
                binding_index = idx,
                plugin = %b.plugin,
                instance = b.instance.as_deref().unwrap_or("<wildcard>"),
                "inbound binding defines no overrides — inherits every agent-level setting \
                 (consider removing the binding entry if this was unintentional)"
            );
        }
    }

    // 7. Soft warn: wildcard + specific-instance binding overlap on
    //    the same plugin. Both match the specific event; order in the
    //    Vec decides the winner (first match wins) — easy to miss
    //    when staring at YAML. Flag it so the operator confirms the
    //    order was intentional.
    let mut seen_wildcard: HashSet<&str> = HashSet::new();
    let mut seen_specific: HashSet<&str> = HashSet::new();
    for b in &agent.inbound_bindings {
        if b.instance.is_none() {
            seen_wildcard.insert(b.plugin.as_str());
        } else {
            seen_specific.insert(b.plugin.as_str());
        }
    }
    for plugin in seen_wildcard.intersection(&seen_specific) {
        tracing::warn!(
            agent = %agent.id,
            plugin = %plugin,
            "inbound bindings contain both a wildcard (instance=None) and a \
             specific-instance entry for the same plugin — first-match wins; \
             list the specific binding before the wildcard if you want it to \
             take priority"
        );
    }
}

fn has_any_override(b: &nexo_config::InboundBinding) -> bool {
    b.allowed_tools.is_some()
        || b.outbound_allowlist.is_some()
        || b.skills.is_some()
        || b.model.is_some()
        || b.system_prompt_extra.is_some()
        || b.allowed_delegates.is_some()
        || !matches!(
            b.sender_rate_limit,
            nexo_config::SenderRateLimitOverride::Keyword(
                nexo_config::SenderRateLimitKeyword::Inherit
            )
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_config::{
        AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, InboundBinding, ModelConfig,
        OutboundAllowlistConfig, TelegramAllowlistConfig, TelegramAutoTranscribeConfig,
        TelegramPluginConfig, TelegramPollingConfig, WorkspaceGitConfig,
    };
    use std::fs;
    use tempfile::TempDir;

    fn agent(id: &str, skills_dir: &str) -> AgentConfig {
        AgentConfig {
            id: id.into(),
            model: ModelConfig {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
            },
            plugins: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: Vec::new(),
            skills_dir: skills_dir.into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: DreamingYamlConfig::default(),
            workspace_git: WorkspaceGitConfig::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            outbound_allowlist: OutboundAllowlistConfig::default(),
            context_optimization: None,
        }
    }

    fn tg_instance(name: &str) -> TelegramPluginConfig {
        TelegramPluginConfig {
            token: "t".into(),
            polling: TelegramPollingConfig::default(),
            allowlist: TelegramAllowlistConfig::default(),
            auto_transcribe: TelegramAutoTranscribeConfig::default(),
            bridge_timeout_ms: 120_000,
            instance: Some(name.into()),
            allow_agents: Vec::new(),
        }
    }

    #[test]
    fn duplicate_binding_rejected() {
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            instance: Some("ana_tg".into()),
            ..Default::default()
        });
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            instance: Some("ana_tg".into()),
            ..Default::default()
        });
        let tg = vec![tg_instance("ana_tg")];
        let err = validate_agent(&a, &tg, &KnownTools::default()).unwrap_err();
        assert!(matches!(
            err,
            BindingValidationError::DuplicateBinding { .. }
        ));
    }

    #[test]
    fn unknown_telegram_instance_rejected() {
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            instance: Some("missing".into()),
            ..Default::default()
        });
        let tg = vec![tg_instance("ana_tg")];
        let err = validate_agent(&a, &tg, &KnownTools::default()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing"));
        assert!(msg.contains("ana_tg"));
    }

    #[test]
    fn wildcard_telegram_binding_accepts_no_instances() {
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            instance: None,
            ..Default::default()
        });
        let tg: Vec<TelegramPluginConfig> = Vec::new();
        validate_agent(&a, &tg, &KnownTools::default()).expect("wildcard must pass");
    }

    #[test]
    fn unknown_tool_rejected_when_catalogue_supplied() {
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "whatsapp".into(),
            allowed_tools: Some(vec!["nonexistent_tool".into()]),
            ..Default::default()
        });
        let tools = KnownTools::new(["whatsapp_send_message", "weather"]);
        let err = validate_agent(&a, &[], &tools).unwrap_err();
        assert!(matches!(err, BindingValidationError::UnknownTool { .. }));
    }

    #[test]
    fn wildcard_tool_always_passes() {
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            allowed_tools: Some(vec!["*".into()]),
            ..Default::default()
        });
        let tools = KnownTools::new(["whatsapp_send_message"]);
        validate_agent(&a, &[], &tools).expect("'*' is always valid");
    }

    #[test]
    fn trailing_star_glob_matches_prefix() {
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            allowed_tools: Some(vec!["memory_*".into()]),
            ..Default::default()
        });
        let tools = KnownTools::new(["memory_write", "memory_query"]);
        validate_agent(&a, &[], &tools).expect("prefix glob should match");
    }

    #[test]
    fn empty_tools_catalogue_disables_check() {
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            allowed_tools: Some(vec!["anything".into()]),
            ..Default::default()
        });
        validate_agent(&a, &[], &KnownTools::default()).expect("empty catalogue = check disabled");
    }

    #[test]
    fn missing_skill_rejected() {
        let dir = TempDir::new().unwrap();
        let skills_dir = dir.path().to_str().unwrap().to_string();
        let mut a = agent("ana", &skills_dir);
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            skills: Some(vec!["no_such_skill".into()]),
            ..Default::default()
        });
        let err = validate_agent(&a, &[], &KnownTools::default()).unwrap_err();
        assert!(matches!(err, BindingValidationError::UnknownSkill { .. }));
    }

    #[test]
    fn existing_skill_dir_passes() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("weather")).unwrap();
        let mut a = agent("ana", dir.path().to_str().unwrap());
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            skills: Some(vec!["weather".into()]),
            ..Default::default()
        });
        validate_agent(&a, &[], &KnownTools::default()).expect("skill present");
    }

    #[test]
    fn binding_without_overrides_passes_and_warns() {
        // We don't assert on the warn output, just that the function
        // returns Ok — the warn is a best-effort signal.
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "whatsapp".into(),
            ..Default::default()
        });
        validate_agent(&a, &[], &KnownTools::default()).expect("must still boot");
    }

    #[test]
    fn provider_mismatch_rejected() {
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            model: Some(ModelConfig {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            }),
            ..Default::default()
        });
        let err = validate_agent(&a, &[], &KnownTools::default()).unwrap_err();
        assert!(matches!(
            err,
            BindingValidationError::ProviderMismatch { .. }
        ));
    }

    #[test]
    fn same_provider_model_switch_accepted() {
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            model: Some(ModelConfig {
                provider: "anthropic".into(),
                model: "claude-sonnet-4-5".into(),
            }),
            ..Default::default()
        });
        validate_agent(&a, &[], &KnownTools::default())
            .expect("same-provider model switch must pass");
    }

    #[test]
    fn collect_binding_errors_aggregates_multi_agent_problems() {
        // Two agents, each broken in a different way. Aggregate form
        // should surface both so the operator fixes them in one pass.
        let mut a1 = agent("ana", "./skills");
        a1.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            instance: Some("missing".into()),
            ..Default::default()
        });
        let mut a2 = agent("bob", "./skills");
        a2.inbound_bindings.push(InboundBinding {
            plugin: "whatsapp".into(),
            model: Some(ModelConfig {
                provider: "minimax".into(),
                model: "x".into(),
            }),
            ..Default::default()
        });
        let errors = collect_binding_errors(&[a1, a2], &[], &KnownTools::default());
        assert_eq!(errors.len(), 2, "both agent errors should be collected");
        assert!(errors
            .iter()
            .any(|e| matches!(e, BindingValidationError::UnknownTelegramInstance { .. })));
        assert!(errors
            .iter()
            .any(|e| matches!(e, BindingValidationError::ProviderMismatch { .. })));
    }

    #[test]
    fn validate_agents_aggregates_into_single_error_message() {
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            instance: Some("ana_tg".into()),
            ..Default::default()
        });
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            instance: Some("ana_tg".into()),
            ..Default::default()
        });
        a.inbound_bindings.push(InboundBinding {
            plugin: "whatsapp".into(),
            model: Some(ModelConfig {
                provider: "minimax".into(),
                model: "x".into(),
            }),
            ..Default::default()
        });
        let tg = vec![tg_instance("ana_tg")];
        let err = validate_agents(&[a], &tg, &KnownTools::default()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("duplicate binding"));
        assert!(msg.contains("provider"));
        assert!(
            msg.contains("issue"),
            "message should count issues, got: {msg}"
        );
    }

    #[test]
    fn unknown_provider_rejected_when_catalogue_supplied() {
        // Agent-level provider typo.
        let mut a = agent("ana", "./skills");
        a.model.provider = "anthopic".into();
        let known = KnownProviders::new(["anthropic", "minimax", "openai"]);
        let err =
            validate_agents_with_providers(&[a], &[], &KnownTools::default(), &known).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("anthopic"));
        assert!(msg.contains("anthropic"));
    }

    #[test]
    fn empty_providers_catalogue_disables_check() {
        // Without a catalogue, any string passes — used in the early
        // boot path before the LLM registry exists.
        let mut a = agent("ana", "./skills");
        a.model.provider = "bogus".into();
        validate_agents(&[a], &[], &KnownTools::default())
            .expect("empty catalogue disables the check");
    }

    #[test]
    fn binding_provider_typo_rejected() {
        let mut a = agent("ana", "./skills");
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            model: Some(ModelConfig {
                provider: "anthopic".into(),
                model: "x".into(),
            }),
            ..Default::default()
        });
        // Both typo provider AND mismatch will fire; aggregate returns
        // both so operator fixes them together.
        let known = KnownProviders::new(["anthropic", "minimax"]);
        let errors =
            collect_binding_errors_with_providers(&[a], &[], &KnownTools::default(), &known);
        assert!(errors
            .iter()
            .any(|e| matches!(e, BindingValidationError::UnknownProvider { .. })));
        assert!(errors
            .iter()
            .any(|e| matches!(e, BindingValidationError::ProviderMismatch { .. })));
    }

    #[test]
    fn happy_path_with_multiple_checks() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("weather")).unwrap();
        let mut a = agent("ana", dir.path().to_str().unwrap());
        a.inbound_bindings.push(InboundBinding {
            plugin: "whatsapp".into(),
            allowed_tools: Some(vec!["whatsapp_send_message".into()]),
            ..Default::default()
        });
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            instance: Some("ana_tg".into()),
            allowed_tools: Some(vec!["*".into()]),
            skills: Some(vec!["weather".into()]),
            ..Default::default()
        });
        let tg = vec![tg_instance("ana_tg")];
        let tools = KnownTools::new(["whatsapp_send_message", "weather"]);
        validate_agent(&a, &tg, &tools).expect("happy path must pass");
    }
}
