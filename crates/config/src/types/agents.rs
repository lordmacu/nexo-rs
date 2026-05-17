use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Closed-enum validator
/// for `agents.<id>.language` and `InboundBinding.language` at
/// YAML deserialise time. The daemon used to silently treat
/// an unknown `language: "klingon"` as `None`; the validator
/// surfaces the typo as a hard parse error so a hand-edit of the
/// YAML never ships an unsupported locale to production.
///
/// Wire shape stays `Option<String>` (admin RPC + microapp
/// parsers consume strings); only the validation gate changes.
fn deserialize_locale_string<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    use std::str::FromStr;

    let opt: Option<String> = Option::deserialize(d)?;
    if let Some(ref s) = opt {
        nexo_tool_meta::locale::Locale::from_str(s).map_err(|e| {
            D::Error::custom(format!(
                "invalid locale {s:?}: {e}. Supported codes: see \
                 nexo_tool_meta::locale::Locale or run \
                 `cargo run -p nexo-microapp-sdk --bin locale_dump`"
            ))
        })?;
    }
    Ok(opt)
}

/// Phase 81.31 — strict BCP-47 validation on every key of the
/// `locale_prompts` map. Mirrors `deserialize_locale_string` so a
/// typo in any key aborts boot with a typed error citing the bad
/// entry, instead of silently shipping an unreachable prompt.
fn deserialize_locale_prompts<'de, D>(d: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    use std::str::FromStr;

    let raw: BTreeMap<String, String> = BTreeMap::deserialize(d)?;
    for (k, _) in raw.iter() {
        nexo_tool_meta::locale::Locale::from_str(k).map_err(|e| {
            D::Error::custom(format!(
                "invalid locale_prompts key {k:?}: {e}. Supported \
                 codes: see nexo_tool_meta::locale::Locale or run \
                 `cargo run -p nexo-microapp-sdk --bin locale_dump`"
            ))
        })?;
    }
    Ok(raw)
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
}

/// Skill dependency-failure mode. Skill authors set this in
/// `requires.mode`; operators override it per-agent via
/// `agents.<id>.skill_overrides`. Defined in `nexo-config` rather
/// than `nexo-core` so the config layer can carry it without
/// pulling in the runtime crate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDepsMode {
    /// Default — skip the skill when any dep is missing.
    #[default]
    Strict,
    /// Load anyway and prepend a `> ⚠️ MISSING DEPS …` banner so the
    /// LLM knows the surface is degraded.
    Warn,
    /// Always skip, even if every dep is satisfied.
    Disable,
}

fn default_active() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub id: String,
    /// SaaS tenant owner. `None` =
    /// global agent (operator-level, not tenant-owned).
    /// Multi-tenant filtering across admin RPC + microapp tools
    /// keys on this field.
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// Whether the runtime should bind this agent's plugins +
    /// inbound subscribers at boot. The admin RPC `agents/upsert`
    /// handler writes this top-level field via `YamlPatcher`, so
    /// the loader needs to accept it (otherwise the daemon
    /// refuses to start the next time it boots after an upsert
    /// — `deny_unknown_fields` flagged it as unknown). Defaults
    /// to `true` for legacy yaml without the field.
    #[serde(default = "default_active")]
    pub active: bool,
    pub model: ModelConfig,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
    #[serde(default)]
    pub config: AgentRuntimeConfig,
    /// System prompt prepended to every LLM turn. Defines the agent's persona,
    /// style, and hard constraints. Empty string = no system message.
    #[serde(default)]
    pub system_prompt: String,
    /// Output language directive for LLM replies. ISO code (`"es"`,
    /// `"en"`, `"pt"`) or human name (`"español"`). When set, the
    /// runtime injects a `# OUTPUT LANGUAGE` block at the top of the
    /// system prompt so the model speaks the configured language to
    /// the user — workspace docs (IDENTITY, SOUL, MEMORY) stay in
    /// English regardless. `None` = no directive (model picks based
    /// on user input).
    ///
    /// Per-binding `InboundBinding::language` overrides this for the
    /// matched channel.
    ///
    /// Strict-validated at YAML deserialise time
    /// against `nexo_tool_meta::locale::Locale::from_str`.
    /// `language: "klingon"` aborts boot with a typed parse error
    /// instead of silently dropping to `None`.
    #[serde(default, deserialize_with = "deserialize_locale_string")]
    pub language: Option<String>,
    /// Phase 81.31 — per-locale `system_prompt` variants. Map of
    /// BCP-47 locale tag → prompt text. When set and the agent's
    /// `language` matches one of the keys, the resolver uses the
    /// localised variant; otherwise the top-level `system_prompt`
    /// is used. Keys are strict-validated against
    /// `nexo_tool_meta::locale::Locale::from_str` at deserialise
    /// time — `locale_prompts: { "klingon": "..." }` aborts boot.
    /// Empty map (default) = legacy single-locale behavior.
    #[serde(default, deserialize_with = "deserialize_locale_prompts")]
    pub locale_prompts: BTreeMap<String, String>,
    /// Link understanding. When enabled, the runtime
    /// detects URLs in inbound messages, fetches each one once per
    /// turn, and renders a `# LINK CONTEXT` system block with the
    /// extracted text. Disabled by default. Schema lives in
    /// the config crate sees only the YAML shape via this opaque
    /// JSON value to avoid a config → core dep cycle.
    #[serde(default)]
    pub link_understanding: serde_json::Value,
    /// Web search. Toggle + provider + caps for the
    /// `web_search` built-in tool. Same opaque-Value discipline as
    /// `link_understanding`: parsed lazily by `EffectiveBindingPolicy`
    /// so the config crate stays nexo-web-search-free.
    #[serde(default)]
    pub web_search: serde_json::Value,
    /// Pairing policy default (per-binding overrides this).
    /// Same opaque-Value discipline; `Value::Null` (default) = the
    /// gate is a no-op so existing setups don't see any change.
    #[serde(default)]
    pub pairing_policy: serde_json::Value,
    /// Optional workspace directory (IDENTITY.md, SOUL.md, USER.md, AGENTS.md,
    /// MEMORY.md, memory/YYYY-MM-DD.md). Loaded at turn start and prepended
    /// to the system prompt. Empty = no workspace layer.
    #[serde(default)]
    pub workspace: String,
    /// Optional list of local skills to inject into the system prompt.
    /// Each entry resolves to `<skills_dir>/<skill>/SKILL.md`.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Base directory for local skills. Relative paths are resolved from the
    /// process working directory. Default: `./skills`.
    #[serde(default = "default_skills_dir")]
    pub skills_dir: String,
    /// Per-skill mode override. Takes precedence over the skill's
    /// `requires.mode` frontmatter. Empty map by default.
    #[serde(default)]
    pub skill_overrides: BTreeMap<String, SkillDepsMode>,
    /// Optional directory for per-session JSONL transcripts. Kept separate
    /// from `workspace` because workspaces are typically git-committed while
    /// transcripts contain PII. Empty = transcripts disabled.
    #[serde(default)]
    pub transcripts_dir: String,
    /// Dreaming sweep config — runs the memory consolidation pass on an
    /// interval when `dreaming.enabled` is true.
    #[serde(default)]
    pub dreaming: DreamingYamlConfig,
    /// Wrap the workspace directory in a local git repo for
    /// forensics, rollback, and LLM-inspectable history. Off by default.
    #[serde(default)]
    pub workspace_git: WorkspaceGitConfig,
    /// Per-tool rate limits. `None` = no limits.
    #[serde(default)]
    pub tool_rate_limits: Option<ToolRateLimitsConfig>,
    /// Opt-out JSON Schema validation of tool
    /// args. `None` defaults to `true` when `schema-validation` feature
    /// is on, `false` otherwise.
    #[serde(default)]
    pub tool_args_validation: Option<ToolArgsValidationConfig>,
    /// Extra workspace-relative markdown files appended to the system
    /// prompt alongside IDENTITY/SOUL/USER/AGENTS. Use for topic-scoped
    /// rules that shouldn't bleed into the personality (e.g.
    /// `SALES_SCRIPT.md`, `PRODUCT_CATALOG.md`). Each file renders as
    /// its own `# RULES — <filename>` block.
    #[serde(default)]
    pub extra_docs: Vec<String>,
    /// Which plugin topics this agent accepts inbound from. Empty means
    /// "accept nothing from plugin.inbound.*" (strict mode); operators must
    /// declare at least one binding per channel they want to receive.
    ///
    /// Topic parse rules:
    ///   `plugin.inbound.<plugin>`                → (plugin, None)
    ///   `plugin.inbound.<plugin>.<instance>`     → (plugin, Some(inst))
    /// Matching is strict on the instance axis:
    /// - `instance=None` matches only `plugin.inbound.<plugin>`
    /// - `instance=Some(x)` matches only `plugin.inbound.<plugin>.<x>`
    #[serde(default)]
    pub inbound_bindings: Vec<InboundBinding>,
    /// Explicit allowlist of tool names this agent may call. Glob with
    /// trailing `*` allowed (`memory_*`). Empty = every registered tool
    /// is callable (back-compat). Populated = strict — any tool whose
    /// name matches no pattern is dropped from the registry at build
    /// time so the LLM never even sees it. Combine with `tool_policy.
    /// per_agent.parallel_safe` for fine-grained execution control.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Per-sender inbound rate limit. Applied in the runtime loop
    /// before the message is enqueued — if a `sender_id` exceeds its
    /// bucket, the event is dropped with a trace log. Protects agents
    /// exposed to public plugin surfaces (Telegram bot, WhatsApp)
    /// from flood / cost griefing. `None` = unlimited (back-compat).
    #[serde(default)]
    pub sender_rate_limit: Option<SenderRateLimitConfig>,
    /// Which peer agents this agent is allowed to delegate to. Glob
    /// with trailing `*` supported. Empty = no restriction (back-compat,
    /// delegate to anyone). Populated = strict — attempts to delegate
    /// outside the list are rejected at tool-call time. Stops runaway
    /// delegation chains and enforces org boundaries (sales agent
    /// shouldn't delegate to the ops agent, etc.).
    #[serde(default)]
    pub allowed_delegates: Vec<String>,
    /// Inverse gate — which peer agents are allowed to delegate TO
    /// this agent. Empty (default) accepts delegations from anyone
    /// (back-compat). Populated = only these senders are honored;
    /// closes the attack vector where a compromised peer bypasses
    /// the caller-side `allowed_delegates` check by publishing
    /// directly to the broker. Trailing `*` glob matches the caller
    /// semantics.
    #[serde(default)]
    pub accept_delegates_from: Vec<String>,
    /// One-line human-readable role description. Fed into the auto-
    /// generated `# PEERS` block other agents see at system-prompt
    /// build time, so the LLM knows who to delegate to. Empty = the
    /// agent's id appears in peers lists with no annotation.
    #[serde(default)]
    pub description: String,
    /// Google OAuth 2.0 — when present, the agent gets the
    /// `google_*` tool family (auth_start / auth_status / call /
    /// auth_revoke). `None` = no Google integration.
    #[serde(default)]
    pub google_auth: Option<GoogleAuthAgentConfig>,
    /// Allowlist of recipients per outbound channel. Enforced by the
    /// `whatsapp_*` and `telegram_*` tools before publishing to the
    /// broker. Empty list = no restriction (back-compat). Populated =
    /// only those recipients may be reached — blocks an agent whose
    /// system prompt was jailbroken from spamming arbitrary numbers.
    #[serde(default)]
    pub outbound_allowlist: OutboundAllowlistConfig,
    /// Per-agent credential bindings. Declares which
    /// plugin instance / Google account the agent uses for outbound
    /// traffic. The gauntlet validates every entry at boot. Empty =
    /// back-compat (resolver infers from `inbound_bindings` when a
    /// single instance matches, otherwise outbound tools are
    /// unavailable).
    #[serde(default)]
    pub credentials: crate::types::credentials::AgentCredentialsConfig,
    /// Per-agent override of `llm.context_optimization`
    /// kill switches. Only the four enables are overridable per-agent
    /// (numeric knobs stay global). `None` on a sub-flag inherits.
    /// Shape:
    /// ```yaml
    /// agents:
    ///   - id: ana
    ///     context_optimization:
    ///       compaction: true   # opt this agent in early
    ///       prompt_cache: false  # disable for testing
    /// ```
    #[serde(default)]
    pub context_optimization: Option<crate::types::llm::AgentContextOptimizationOverride>,
    /// Dispatch policy for project-tracker tools
    /// (`program_phase`, `cancel_agent`, etc). Default keeps the
    /// dispatch surface OFF for back-compat agents — the operator
    /// must opt in by raising `mode` to `read_only` or `full`.
    #[serde(default)]
    pub dispatch_policy: DispatchPolicy,
    /// Plan-mode policy default (per-binding overrides
    /// this via `InboundBinding::plan_mode`). The field is always
    /// present so an operator can pin behaviour at the agent level
    /// even without binding-level overrides.
    #[serde(default)]
    pub plan_mode: crate::types::plan_mode::PlanModePolicy,
    /// Allowlist of remote-trigger destinations the
    /// agent's `RemoteTrigger` tool may publish to. Empty (the
    /// default) keeps the tool registered but every call refuses
    /// with a clear "name not in allowlist" error.
    #[serde(default)]
    pub remote_triggers: Vec<crate::types::remote_triggers::RemoteTriggerEntry>,
    /// Agent-level LSP policy. Per-binding override
    /// is a follow-up; today the field on `AgentConfig` is the
    /// single source of truth for whether the `Lsp` tool is
    /// registered for this agent's goals.
    #[serde(default)]
    pub lsp: crate::types::lsp::LspPolicy,
    /// Agent-level `ConfigTool` policy. Default
    /// `self_edit: false` (opt-in). When enabled, the `Config`
    /// tool is registered for this agent's goals and the
    /// approval correlator listens for operator approvals on the
    /// originating channel.
    ///
    /// YAML key is `config_tool` — `config` is already taken by
    /// `AgentRuntimeConfig` (line ~38). Operators write
    /// `agents[].config_tool: { self_edit: true, ... }`.
    #[serde(default)]
    pub config_tool: crate::types::config_tool::ConfigToolPolicy,
    /// Agent-level team policy. Default
    /// `enabled: false` (opt-in). When enabled, the 5 `Team*`
    /// tools register for this agent's goals.
    #[serde(default)]
    pub team: crate::types::team::TeamPolicy,
    /// Proactive tick-loop config. When `enabled: true` the
    /// driver-loop keeps the goal alive after every turn and injects
    /// periodic `<tick>` prompts. The agent controls its own wake-up
    /// interval via `Sleep { duration_ms }`.
    #[serde(default)]
    pub proactive: crate::types::proactive::ProactiveConfig,
    /// Stateful REPL tool config. When `enabled: true` the
    /// `Repl` tool is registered for this agent, allowing persistent
    /// Python/Node/bash subprocesses across turns.
    #[serde(default)]
    pub repl: crate::types::repl::ReplConfig,
    /// autoDream consolidation config. `None` disables
    /// the post-turn fork. When `Some(cfg)` AND `cfg.enabled = true`,
    /// the runner fires from the driver-loop per-turn hook.
    #[serde(default)]
    pub auto_dream: Option<crate::types::dream::AutoDreamConfig>,

    /// Per-binding assistant-mode toggle. `None` keeps
    /// the legacy behaviour (no system-prompt addendum, no
    /// initial-team spawn). `Some(cfg)` opt-ins to the proactive
    /// posture when `cfg.enabled = true`. Boot-immutable flag —
    /// toggling enabled requires a daemon restart; the addendum
    /// content itself is hot-reloadable.
    #[serde(default)]
    pub assistant_mode: Option<crate::types::assistant::AssistantConfig>,

    /// Re-connection digest config. `None` keeps the
    /// runtime quiet (default). `Some(cfg)` with `cfg.enabled = true`
    /// opts into the AWAY_SUMMARY behaviour: on the first inbound
    /// after `cfg.threshold_hours` of silence, the runtime composes
    /// a short markdown digest summarising goals/aborts/failures
    /// recorded in the turn-log during the silence window
    /// and delivers it before processing the user's message.
    #[serde(default)]
    pub away_summary: Option<crate::types::away_summary::AwaySummaryConfig>,

    /// MCP channel routing. `None` keeps the legacy
    /// behaviour (no inbound from MCP servers — channel
    /// notifications fall through with `Skip`). `Some(cfg)` with
    /// `cfg.enabled = true` arms the 5-step gate; per-binding
    /// allowlists (`InboundBinding::allowed_channel_servers`) close
    /// the loop for which servers a given binding will accept
    /// notifications from. Hot-reloadable.
    #[serde(default)]
    pub channels: Option<crate::types::channels::ChannelsConfig>,

    /// Brief-mode + `send_user_message` tool. `None`
    /// keeps the legacy behaviour (no extra tool, no extra system
    /// section). `Some(cfg)` with `cfg.enabled = true` registers
    /// the tool for every binding of this agent and appends the
    /// "talking to the user" section to the system prompt. When
    /// `assistant_mode.enabled = true`, brief is implicitly active
    /// even if `brief.enabled = false` — assistant mode already
    /// hard-codes the same instruction.
    #[serde(default)]
    pub brief: Option<crate::types::brief::BriefConfig>,

    /// Auto-approve dial for the curated tool subset.
    /// `false` (default) keeps current interactive-approval behaviour;
    /// `true` flips skipping the prompt for read-only / scoped-write
    /// tools while destructive Bash + writes outside workspace +
    /// ConfigTool + REPL + remote_trigger always ask. Composes with
    /// the binding policy — never adds tools to the surface.
    /// Per-binding override available via `InboundBinding::auto_approve`.
    #[serde(default)]
    pub auto_approve: bool,

    /// Post-turn LLM memory extraction. `None`
    /// keeps the legacy behaviour (no extraction). `Some(cfg)`
    /// with `cfg.enabled = true` opts the agent in; the boot
    /// loop constructs `ExtractMemories` + `LlmClientAdapter`
    /// (defined in `nexo-driver-loop`) and wires them into
    /// `LlmAgentBehavior` via `with_memory_extractor` so every
    /// regular turn fires post-turn extraction. Boot-immutable
    /// today — toggling `enabled` requires a daemon restart.
    /// Wire-shape struct is
    /// duplicated here (mirror of the
    /// `nexo_driver_types::ExtractMemoriesConfig` schema) to
    /// avoid creating a `nexo-config -> nexo-driver-types`
    /// edge that would cycle through the existing
    /// `nexo-driver-types -> nexo-config` dep — same precedent
    /// as `SecretGuardYamlConfig` in `crates/config/src/types/
    /// memory.rs`. Conversion happens in `src/main.rs`
    /// (`build_extract_memories_config_from_yaml`).
    #[serde(default)]
    pub extract_memories: Option<ExtractMemoriesYamlConfig>,

    /// Per-agent NATS event subscribers. Each
    /// binding subscribes to a subject pattern and translates
    /// matching events into the standard inbound flow (republished
    /// to `plugin.inbound.event.<id>`). Empty by default; existing
    /// agents are unaffected until the operator opts in.
    #[serde(default)]
    pub event_subscribers: Vec<crate::types::event_subscriber::EventSubscriberBinding>,

    /// Per-agent extension config. Operator declares
    /// per-extension knobs in `agents.yaml` so a single microapp
    /// subprocess that serves multiple personas / tenants can
    /// look up the right config in O(1) by `agent_id`. The shape
    /// is `{ <extension_id>: <opaque YAML> }` — opaque to the
    /// daemon, validated by the microapp itself (boot-time schema
    /// validation is opt-in).
    ///
    /// ```yaml
    /// agents:
    ///   - id: ana
    ///     extensions_config:
    ///       ventas-etb:
    ///         regional: bogota
    ///         asesor_phone: "573115728852"
    /// ```
    ///
    /// Empty by default; agents that don't bind to any
    /// per-extension config keep working unchanged. Propagated
    /// to the microapp via the JSON-RPC `initialize` method.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions_config: BTreeMap<String, serde_yaml::Value>,
}

/// Wire-shape mirror of
/// `nexo_driver_types::ExtractMemoriesConfig`. Adding
/// `nexo-driver-types` to `nexo-config`'s dep tree would create
/// a cycle (`nexo-driver-types` already depends on
/// `nexo-config`). Mirror the schema 1:1 here; `src/main.rs`
/// converts to the canonical type at boot. Same dual-write
/// contract as `SecretGuardYamlConfig`: when one struct
/// changes, update the other.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExtractMemoriesYamlConfig {
    /// Master switch. Default false — opt-in.
    pub enabled: bool,
    /// Run extraction every N eligible turns (1 = every turn).
    pub turns_throttle: u32,
    /// Hard cap on LLM turns per extraction.
    pub max_turns: u32,
    /// Consecutive failures that trip the circuit breaker
    /// (0 = disabled).
    pub max_consecutive_failures: u32,
}

impl Default for ExtractMemoriesYamlConfig {
    fn default() -> Self {
        // Mirrors `nexo_driver_types::ExtractMemoriesConfig::default`.
        Self {
            enabled: false,
            turns_throttle: 1,
            max_turns: 5,
            max_consecutive_failures: 3,
        }
    }
}

impl AgentConfig {
    /// Phase 81.31 — resolve the system prompt to use for a given
    /// locale. When `locale_prompts` is populated and contains the
    /// requested locale, that variant wins; otherwise the top-level
    /// `system_prompt` is returned. The caller may pass `None` (or
    /// `Some(&self.language)`) — both fall back to `system_prompt`
    /// when no exact match is found. Empty-string entries in the
    /// map are returned verbatim — callers decide how to treat
    /// "empty intentionally" vs "missing".
    pub fn effective_system_prompt(&self, locale: Option<&str>) -> &str {
        if let Some(lang) = locale {
            if let Some(s) = self.locale_prompts.get(lang) {
                return s;
            }
        }
        &self.system_prompt
    }
}

#[cfg(test)]
mod locale_prompts_tests {
    use super::*;

    fn agent_with_prompts(top: &str, prompts: &[(&str, &str)]) -> AgentConfig {
        let mut cfg: AgentConfig = serde_yaml::from_str(
            r#"id: x
model:
  provider: anthropic
  model: claude-opus-4-7
"#,
        )
        .unwrap();
        cfg.system_prompt = top.into();
        cfg.locale_prompts = prompts
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        cfg
    }

    #[test]
    fn effective_returns_locale_match_when_present() {
        let cfg = agent_with_prompts("top", &[("en", "english"), ("es", "spanish")]);
        assert_eq!(cfg.effective_system_prompt(Some("es")), "spanish");
        assert_eq!(cfg.effective_system_prompt(Some("en")), "english");
    }

    #[test]
    fn effective_falls_back_when_locale_missing() {
        let cfg = agent_with_prompts("top", &[("en", "english")]);
        assert_eq!(cfg.effective_system_prompt(Some("es")), "top");
    }

    #[test]
    fn effective_falls_back_when_locale_prompts_empty() {
        let cfg = agent_with_prompts("top", &[]);
        assert_eq!(cfg.effective_system_prompt(Some("en")), "top");
        assert_eq!(cfg.effective_system_prompt(None), "top");
    }

    #[test]
    fn yaml_parse_accepts_locale_prompts_block() {
        let yaml = r#"
id: ana
model:
  provider: anthropic
  model: claude-opus-4-7
system_prompt: top
language: en
locale_prompts:
  en: english variant
  es: spanish variant
"#;
        let cfg: AgentConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.locale_prompts.len(), 2);
        assert_eq!(
            cfg.locale_prompts.get("es").map(String::as_str),
            Some("spanish variant")
        );
    }

    #[test]
    fn yaml_parse_rejects_invalid_locale_key() {
        let yaml = r#"
id: ana
model:
  provider: anthropic
  model: claude-opus-4-7
system_prompt: top
locale_prompts:
  klingon: nope
"#;
        let err = serde_yaml::from_str::<AgentConfig>(yaml)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("locale_prompts key") || err.contains("klingon"),
            "expected locale-key validation error, got: {err}"
        );
    }
}

#[cfg(test)]
mod auto_dream_yaml_tests {
    use super::*;

    fn minimal_yaml() -> &'static str {
        // Most fields default via `#[serde(default)]` — only id +
        // model are strictly required.
        r#"id: test_agent
model:
  provider: anthropic
  model: claude-opus-4-7
"#
    }

    /// YAML lacking `extensions_config` deserialises
    /// to an empty map (back-compat via `#[serde(default)]`).
    #[test]
    fn agent_config_yaml_without_extensions_config_parses() {
        let cfg: AgentConfig = serde_yaml::from_str(minimal_yaml()).unwrap();
        assert!(cfg.extensions_config.is_empty());
    }

    /// YAML with `extensions_config` round-trips per
    /// `<extension_id>` and preserves the opaque YAML payload.
    #[test]
    fn agent_config_yaml_with_extensions_config_parses() {
        let yaml = format!(
            "{}extensions_config:\n  ventas-etb:\n    regional: bogota\n    asesor_phone: \"573115728852\"\n  another-app:\n    enabled: true\n",
            minimal_yaml()
        );
        let cfg: AgentConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(cfg.extensions_config.len(), 2);
        let etb = cfg
            .extensions_config
            .get("ventas-etb")
            .expect("ventas-etb config");
        let regional = etb.get("regional").and_then(|v| v.as_str());
        assert_eq!(regional, Some("bogota"));
        let phone = etb.get("asesor_phone").and_then(|v| v.as_str());
        assert_eq!(phone, Some("573115728852"));
        let another = cfg
            .extensions_config
            .get("another-app")
            .expect("another-app");
        assert_eq!(another.get("enabled").and_then(|v| v.as_bool()), Some(true));
    }

    /// YAML lacking `auto_dream` block must
    /// deserialize to `auto_dream: None` (`#[serde(default)]`
    /// backward-compat).
    #[test]
    fn agent_config_yaml_without_auto_dream_parses() {
        let cfg: AgentConfig = serde_yaml::from_str(minimal_yaml()).unwrap();
        assert!(cfg.auto_dream.is_none());
    }

    /// YAML with explicit `auto_dream` block
    /// populates the field correctly. humantime_serde parses the
    /// `min_hours: 25h` literal.
    #[test]
    fn agent_config_yaml_with_auto_dream_parses() {
        let yaml = format!(
            "{}auto_dream:\n  enabled: true\n  min_hours: 25h\n  min_sessions: 7\n",
            minimal_yaml()
        );
        let cfg: AgentConfig = serde_yaml::from_str(&yaml).unwrap();
        let ad = cfg
            .auto_dream
            .expect("auto_dream block should populate field");
        assert!(ad.enabled);
        assert_eq!(ad.min_hours, std::time::Duration::from_secs(25 * 3600));
        assert_eq!(ad.min_sessions, 7);
    }

    /// Explicit `enabled: false` is distinguishable
    /// from absent block (Some(cfg) with cfg.enabled=false vs None).
    #[test]
    fn agent_config_yaml_with_auto_dream_disabled_explicit() {
        let yaml = format!("{}auto_dream:\n  enabled: false\n", minimal_yaml());
        let cfg: AgentConfig = serde_yaml::from_str(&yaml).unwrap();
        let ad = cfg.auto_dream.expect("auto_dream block present");
        assert!(!ad.enabled);
    }

    // ── extract_memories YAML ──

    #[test]
    fn agent_config_yaml_without_extract_memories_parses() {
        let cfg: AgentConfig = serde_yaml::from_str(minimal_yaml()).unwrap();
        assert!(cfg.extract_memories.is_none());
    }

    #[test]
    fn agent_config_yaml_with_extract_memories_parses() {
        let yaml = format!(
            "{}extract_memories:\n  enabled: true\n  turns_throttle: 2\n  max_turns: 5\n  max_consecutive_failures: 3\n",
            minimal_yaml()
        );
        let cfg: AgentConfig = serde_yaml::from_str(&yaml).unwrap();
        let em = cfg
            .extract_memories
            .expect("extract_memories block should populate field");
        assert!(em.enabled);
        assert_eq!(em.turns_throttle, 2);
        assert_eq!(em.max_turns, 5);
        assert_eq!(em.max_consecutive_failures, 3);
    }

    #[test]
    fn extract_memories_default_disables() {
        // Bare `extract_memories: {}` yields enabled=false defaults.
        let yaml = format!("{}extract_memories: {{}}\n", minimal_yaml());
        let cfg: AgentConfig = serde_yaml::from_str(&yaml).unwrap();
        let em = cfg.extract_memories.expect("block present");
        assert!(!em.enabled);
        assert_eq!(em.turns_throttle, 1);
        assert_eq!(em.max_turns, 5);
    }
}

/// Tri-state dispatch capability. The same enum is used for the
/// agent-level default and per-binding override; the resolver folds
/// them in `EffectiveBindingPolicy::dispatch_policy`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchCapability {
    /// Tracker dispatch tools are not registered for this binding.
    /// Read-only project_status / list_agents / followup_detail
    /// are unaffected (they live behind their own gate).
    #[default]
    None,
    /// Read-only tools registered (`project_status`,
    /// `project_phases_list`, `list_agents`, `agent_status`,
    /// `agent_logs_tail`, `agent_hooks_list`, `git_log_for_phase`).
    /// No write tools.
    ReadOnly,
    /// Full tracker surface — read + dispatch + chaining + control.
    Full,
}

/// Per-agent / per-binding dispatch policy. Not all fields are
/// honoured at every call site (see `DispatchGate` for
/// the enforcement matrix); the YAML carries them all so an
/// operator's intent survives across feature increments.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DispatchPolicy {
    pub mode: DispatchCapability,
    /// Cap on the number of in-flight goals this agent / binding may
    /// hold open at once. `0` = inherit the global cap from
    /// `program_phase.max_concurrent_agents`.
    pub max_concurrent_per_dispatcher: u32,
    /// Optional whitelist of phase ids this agent may dispatch.
    /// Trailing `*` glob supported (`67.*`). Empty / `["*"]` =
    /// every phase id.
    pub allowed_phase_ids: Vec<String>,
    /// Hard deny list. Wins over `allowed_phase_ids` when both
    /// match (e.g. `allowed = ["*"]`, `forbidden = ["0.*"]`).
    pub forbidden_phase_ids: Vec<String>,
}

/// Per-agent allowlist of outbound recipients. Phone numbers are matched
/// as normalized strings (digits only, country code included). Telegram
/// chat IDs are matched exactly.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundAllowlistConfig {
    #[serde(default)]
    pub whatsapp: Vec<String>,
    #[serde(default)]
    pub telegram: Vec<i64>,
}

/// Thin YAML surface for Google OAuth creds. Mirrors the shape
/// `nexo_core::agent::google_auth::GoogleAuthConfig` expects; the
/// runtime converts between the two at boot (keeps the config crate
/// independent of nexo-core).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoogleAuthAgentConfig {
    pub client_id: String,
    pub client_secret: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default = "default_google_token_file")]
    pub token_file: String,
    #[serde(default = "default_google_redirect_port")]
    pub redirect_port: u16,
}

fn default_google_token_file() -> String {
    "google_tokens.json".to_string()
}
fn default_google_redirect_port() -> u16 {
    8765
}

/// Token bucket per `(agent_id, sender_id)`. `burst` = initial pool;
/// refills at `rps` tokens/second up to the cap.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SenderRateLimitConfig {
    pub rps: f64,
    #[serde(default = "default_sender_burst")]
    pub burst: u64,
}

fn default_sender_burst() -> u64 {
    5
}

/// Matches inbound plugin events to an agent using strict
/// `(plugin, instance)` equality.
///
/// Per-binding overrides: each optional field replaces the matching
/// agent-level setting when `Some(..)`. `None` (the default) inherits the
/// agent-level value unchanged, preserving back-compat for bindings that
/// only specify `plugin` / `instance`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboundBinding {
    pub plugin: String,
    #[serde(default)]
    pub instance: Option<String>,

    /// Replace the agent-level `allowed_tools` for messages arriving via
    /// this binding. `Some(vec!["*"])` = expose every registered tool.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// Replace the agent-level outbound allowlist for this binding.
    #[serde(default)]
    pub outbound_allowlist: Option<OutboundAllowlistConfig>,
    /// Replace the agent-level skills list for this binding.
    #[serde(default)]
    pub skills: Option<Vec<String>>,
    /// Override the LLM used when answering via this binding.
    #[serde(default)]
    pub model: Option<ModelConfig>,
    /// Appended to the agent's `system_prompt` as a `# CHANNEL ADDENDUM`
    /// block. Personality lives at the agent level; the addendum only
    /// tells the LLM what is special about this particular channel.
    #[serde(default)]
    pub system_prompt_extra: Option<String>,
    /// Per-binding sender rate limit. See `SenderRateLimitOverride` for
    /// the three supported forms (`inherit` / `disable` / `{rps, burst}`).
    #[serde(default)]
    pub sender_rate_limit: SenderRateLimitOverride,
    /// Replace the agent-level `allowed_delegates` list for this binding.
    #[serde(default)]
    pub allowed_delegates: Option<Vec<String>>,
    /// Override the agent-level output language for this binding. Use
    /// when the same agent answers different channels in different
    /// languages (e.g. Spanish on a local WhatsApp, English on a
    /// support Telegram). `None` (default) inherits the agent-level
    /// `language` field.
    ///
    /// Strict-validated at YAML deserialise time;
    /// see `AgentConfig.language` for the closed-enum reference.
    #[serde(default, deserialize_with = "deserialize_locale_string")]
    pub language: Option<String>,
    /// Per-binding override of the link-understanding
    /// config. Same opaque-JSON shape as the agent-level field;
    /// `serde_json::Value::Null` (default) inherits the agent value.
    /// Use when an agent has link-understanding enabled globally but
    /// you want it OFF on a specific channel (e.g. a high-volume
    /// public WhatsApp where every URL fetch would burn quota).
    #[serde(default)]
    pub link_understanding: serde_json::Value,
    /// Per-binding override of the web-search config.
    /// `Value::Null` (default) inherits the agent-level value.
    #[serde(default)]
    pub web_search: serde_json::Value,
    /// Pairing policy. Same opaque-Value discipline as
    /// `link_understanding` and `web_search`. `Value::Null` (default)
    /// = the gate is a no-op (every message admitted). When the
    /// binding sets `{auto_challenge: true}`, unknown senders get a
    /// challenge reply and the message is dropped.
    #[serde(default)]
    pub pairing_policy: serde_json::Value,
    /// Per-binding override of `agents.dispatch_policy`.
    /// `None` (default) inherits the agent-level value; populated
    /// replaces the whole struct so an operator can be precise per
    /// channel ("agent 'asistente' is `none` everywhere except this
    /// Telegram chat where it's `full`").
    #[serde(default)]
    pub dispatch_policy: Option<DispatchPolicy>,
    /// Per-binding override of `agents.plan_mode`. `None`
    /// (default) inherits the agent-level policy; populated replaces
    /// the whole struct.
    #[serde(default)]
    pub plan_mode: Option<crate::types::plan_mode::PlanModePolicy>,
    /// Optional role tag consulted by
    /// `PlanModePolicy::compute_default_active`. Accepts
    /// `"coordinator"`, `"worker"`, `"proactive"` (case-insensitive);
    /// any other value (or omission) is treated as unset.
    #[serde(default)]
    pub role: Option<String>,
    /// Per-binding proactive config override. `None` inherits
    /// the agent-level `proactive` block. When present, replaces the whole
    /// struct for goals spawned from this binding.
    #[serde(default)]
    pub proactive: Option<crate::types::proactive::ProactiveConfig>,
    /// Per-binding REPL config override. `None` inherits
    /// the agent-level `repl` block. When present, replaces the whole
    /// struct for goals spawned from this binding.
    #[serde(default)]
    pub repl: Option<crate::types::repl::ReplConfig>,
    /// Per-binding auto-approve dial override. `None`
    /// (default) inherits the agent-level `auto_approve`.
    /// `Some(true|false)` overrides for this binding.
    #[serde(default)]
    pub auto_approve: Option<bool>,

    /// Per-binding channel-server session allowlist.
    /// Empty (default) denies every channel registration for this
    /// binding even when `agents.channels.enabled = true`. Names
    /// must match the MCP server name advertised at runtime — for
    /// plugin-served channels the convention is
    /// `plugin:<plugin>:<server>`.
    #[serde(default)]
    pub allowed_channel_servers: Vec<String>,
    /// Per-binding override of `agents.remote_triggers`.
    /// `None` (default) inherits the agent-level allowlist; `Some(vec)`
    /// replaces it entirely for this binding.
    #[serde(default)]
    pub remote_triggers: Option<Vec<crate::types::remote_triggers::RemoteTriggerEntry>>,
    /// Per-binding LSP policy override. `None` (default)
    /// inherits the agent-level `lsp` block. When present, replaces the
    /// whole struct for goals spawned from this binding (operator must
    /// write the full `LspPolicy` if they only want to flip one field).
    #[serde(default)]
    pub lsp: Option<crate::types::lsp::LspPolicy>,
    /// Per-binding team policy override. `None` (default)
    /// inherits the agent-level `team` block. Replace-whole semantics.
    #[serde(default)]
    pub team: Option<crate::types::team::TeamPolicy>,
    /// Per-binding config-tool policy override. `None`
    /// (default) inherits the agent-level `config_tool` block. Replace-whole.
    #[serde(default)]
    pub config_tool: Option<crate::types::config_tool::ConfigToolPolicy>,
    /// Per-binding tool rate-limit overrides for
    /// multi-tenant fair-use. Maps tool-name glob → spec.
    /// `None` (default) inherits the global agent-level
    /// `tool_rate_limits`. `Some(map)` FULLY REPLACES the global
    /// decision for this binding (no fall-through to global
    /// patterns). Reserved key `_default` matches anything not
    /// caught by an explicit pattern. Empty `patterns: {}` =
    /// explicit "no rate-limit applies on this binding".
    #[serde(default)]
    pub tool_rate_limits: Option<ToolRateLimitsConfig>,
}

/// Per-binding override for the sender rate limit.
///
/// YAML forms accepted:
///   `sender_rate_limit: inherit`            → inherit agent-level limit
///   `sender_rate_limit: disable`            → no limit on this binding
///   `sender_rate_limit: { rps: .., burst: .. }` → binding-specific limit
///
/// Omitting the field is equivalent to `inherit` (the `Default`).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SenderRateLimitOverride {
    Keyword(SenderRateLimitKeyword),
    Config(SenderRateLimitConfig),
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SenderRateLimitKeyword {
    Inherit,
    Disable,
}

impl Default for SenderRateLimitOverride {
    fn default() -> Self {
        SenderRateLimitOverride::Keyword(SenderRateLimitKeyword::Inherit)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolArgsValidationConfig {
    #[serde(default = "default_tool_args_validation_enabled")]
    pub enabled: bool,
}

impl Default for ToolArgsValidationConfig {
    fn default() -> Self {
        Self {
            enabled: default_tool_args_validation_enabled(),
        }
    }
}

fn default_tool_args_validation_enabled() -> bool {
    true
}

/// Per-tool rate limits. Pattern `_default` applies when no other
/// pattern matches; any `*` in the pattern is a wildcard.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolRateLimitsConfig {
    #[serde(default)]
    pub patterns: std::collections::HashMap<String, ToolRateLimitSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolRateLimitSpec {
    pub rps: f64,
    #[serde(default)]
    pub burst: u64,
    /// When `true`, fail-closed if the bucket for
    /// `(agent, binding_id, tool)` was evicted by LRU pressure.
    /// Default `false` is fail-open; tools
    /// declared "essential" (e.g. paid `marketing_send_drip` on
    /// a free tier) opt-in to fail-closed so evicted-bucket
    /// races cannot leak quota.
    #[serde(default)]
    pub essential_deny_on_miss: bool,
}

impl ToolRateLimitSpec {
    /// Effective burst capacity. When operator omits `burst`,
    /// derive from `rps` (rounded up, minimum 1).
    pub fn effective_burst(&self) -> u64 {
        if self.burst == 0 {
            self.rps.ceil().max(1.0) as u64
        } else {
            self.burst
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceGitConfig {
    #[serde(default = "default_wsg_enabled")]
    pub enabled: bool,
    #[serde(default = "default_wsg_author_name")]
    pub author_name: String,
    #[serde(default = "default_wsg_author_email")]
    pub author_email: String,
}

fn default_wsg_enabled() -> bool {
    false
}
fn default_wsg_author_name() -> String {
    "agent".to_string()
}
fn default_wsg_author_email() -> String {
    "agent@localhost".to_string()
}
fn default_skills_dir() -> String {
    "./skills".to_string()
}

/// YAML surface for dreaming. Mirrors `nexo_core::agent::DreamingConfig`
/// but lives in the config crate to keep the dependency graph acyclic.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DreamingYamlConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    #[serde(default = "default_min_score")]
    pub min_score: f32,
    #[serde(default = "default_min_recall_count")]
    pub min_recall_count: u32,
    #[serde(default = "default_min_unique_queries")]
    pub min_unique_queries: u32,
    #[serde(default = "default_max_promotions_per_sweep")]
    pub max_promotions_per_sweep: usize,
    #[serde(default)]
    pub weights: DreamingWeightsYaml,
}

fn default_interval_secs() -> u64 {
    86_400
}
fn default_min_score() -> f32 {
    0.35
}
fn default_min_recall_count() -> u32 {
    3
}
fn default_min_unique_queries() -> u32 {
    2
}
fn default_max_promotions_per_sweep() -> usize {
    20
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DreamingWeightsYaml {
    #[serde(default = "default_weight_freq")]
    pub frequency: f32,
    #[serde(default = "default_weight_rel")]
    pub relevance: f32,
    #[serde(default = "default_weight_rec")]
    pub recency: f32,
    #[serde(default = "default_weight_div")]
    pub diversity: f32,
    #[serde(default = "default_weight_con")]
    pub consolidation: f32,
}

impl Default for DreamingWeightsYaml {
    fn default() -> Self {
        Self {
            frequency: default_weight_freq(),
            relevance: default_weight_rel(),
            recency: default_weight_rec(),
            diversity: default_weight_div(),
            consolidation: default_weight_con(),
        }
    }
}

fn default_weight_freq() -> f32 {
    0.24
}
fn default_weight_rel() -> f32 {
    0.30
}
fn default_weight_rec() -> f32 {
    0.15
}
fn default_weight_div() -> f32 {
    0.15
}
fn default_weight_con() -> f32 {
    0.10
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_heartbeat_interval")]
    pub interval: String,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        HeartbeatConfig {
            enabled: false,
            interval: default_heartbeat_interval(),
        }
    }
}

fn default_heartbeat_interval() -> String {
    "5m".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRuntimeConfig {
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_queue_cap")]
    pub queue_cap: usize,
}

impl Default for AgentRuntimeConfig {
    fn default() -> Self {
        AgentRuntimeConfig {
            debounce_ms: default_debounce_ms(),
            queue_cap: default_queue_cap(),
        }
    }
}

fn default_debounce_ms() -> u64 {
    2000
}
fn default_queue_cap() -> usize {
    32
}

#[cfg(test)]
mod rate_limit_yaml_tests {
    use super::*;

    /// Yaml round-trip with new field.
    #[test]
    fn tool_rate_limit_spec_yaml_with_essential_deny_on_miss() {
        let yaml = "rps: 0.167\nburst: 10\nessential_deny_on_miss: true\n";
        let spec: ToolRateLimitSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.rps, 0.167);
        assert_eq!(spec.burst, 10);
        assert!(spec.essential_deny_on_miss);
    }

    /// Default `essential_deny_on_miss = false` is fail-open.
    #[test]
    fn tool_rate_limit_spec_default_essential_deny_on_miss_false() {
        let yaml = "rps: 1.0\nburst: 5\n";
        let spec: ToolRateLimitSpec = serde_yaml::from_str(yaml).unwrap();
        assert!(!spec.essential_deny_on_miss);
    }

    /// `effective_burst` derives from rps when burst is 0.
    #[test]
    fn tool_rate_limit_spec_effective_burst_derives_from_rps() {
        let spec = ToolRateLimitSpec {
            rps: 3.5,
            burst: 0,
            essential_deny_on_miss: false,
        };
        assert_eq!(spec.effective_burst(), 4); // ceil(3.5)
    }

    #[test]
    fn tool_rate_limits_config_round_trip_through_serde() {
        let yaml = r#"
patterns:
  marketing_send_drip:
    rps: 0.167
    burst: 10
    essential_deny_on_miss: true
  "memory_*":
    rps: 1.0
    burst: 5
"#;
        let cfg: ToolRateLimitsConfig = serde_yaml::from_str(yaml).unwrap();
        let drip = cfg.patterns.get("marketing_send_drip").unwrap();
        assert!(drip.essential_deny_on_miss);
        let mem = cfg.patterns.get("memory_*").unwrap();
        assert!(!mem.essential_deny_on_miss);
    }

    /// Yaml round-trip with per-binding override.
    /// Validates the field on `InboundBinding` deserialises and
    /// the operator-friendly nested shape works.
    #[test]
    fn inbound_binding_with_tool_rate_limits_yaml() {
        let yaml = r#"
plugin: whatsapp
instance: free_tier
tool_rate_limits:
  patterns:
    marketing_send_drip:
      rps: 0.167
      burst: 10
      essential_deny_on_miss: true
    "memory_*":
      rps: 1.0
      burst: 5
"#;
        let b: InboundBinding = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(b.plugin, "whatsapp");
        assert_eq!(b.instance.as_deref(), Some("free_tier"));
        let map = b.tool_rate_limits.as_ref().expect("override present");
        let drip = map.patterns.get("marketing_send_drip").unwrap();
        assert!(drip.essential_deny_on_miss);
        assert_eq!(drip.burst, 10);
    }

    /// `tool_rate_limits` defaults to `None` (inherit global) when
    /// the operator omits the field.
    #[test]
    fn inbound_binding_default_tool_rate_limits_is_none() {
        let yaml = "plugin: whatsapp\ninstance: enterprise\n";
        let b: InboundBinding = serde_yaml::from_str(yaml).unwrap();
        assert!(b.tool_rate_limits.is_none());
    }
}

/// Strict YAML validation
/// of `agents.<id>.language` and `InboundBinding.language` against
/// the closed-enum `nexo_tool_meta::locale::Locale`.
#[cfg(test)]
mod locale_yaml_validation_tests {
    use super::*;

    fn agent_yaml(language_block: &str) -> String {
        format!(
            r#"
id: ana
description: ""
model:
  provider: test
  model: gpt-test
plugins: []
allowed_tools: []
{language_block}
"#
        )
    }

    #[test]
    fn deserializes_valid_es_ar_locale() {
        let y = agent_yaml("language: es-AR");
        let a: AgentConfig = serde_yaml::from_str(&y).expect("valid es-AR locale must deserialize");
        assert_eq!(a.language.as_deref(), Some("es-AR"));
    }

    #[test]
    fn rejects_unknown_lang_code() {
        let y = agent_yaml(r#"language: "klingon""#);
        let err = serde_yaml::from_str::<AgentConfig>(&y).expect_err("klingon must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("invalid locale") || msg.contains("klingon"),
            "expected closed-enum rejection, got: {msg}"
        );
    }

    #[test]
    fn rejects_unknown_region() {
        let y = agent_yaml(r#"language: "es-XX""#);
        let err = serde_yaml::from_str::<AgentConfig>(&y).expect_err("es-XX must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("invalid locale") || msg.contains("XX"),
            "expected closed-enum region rejection, got: {msg}"
        );
    }

    #[test]
    fn accepts_lang_only_locale() {
        let y = agent_yaml("language: en");
        let a: AgentConfig = serde_yaml::from_str(&y).expect("language-only en must deserialize");
        assert_eq!(a.language.as_deref(), Some("en"));
    }

    #[test]
    fn accepts_null_language() {
        // Field omitted entirely (default = None).
        let y = agent_yaml("");
        let a: AgentConfig = serde_yaml::from_str(&y).expect("absent language must default None");
        assert_eq!(a.language, None);
    }

    /// Same validator gate fires on `InboundBinding.language` —
    /// per-binding overrides cannot ship an unsupported locale.
    #[test]
    fn rejects_unknown_locale_on_binding_override() {
        let y = "plugin: whatsapp\ninstance: support\nlanguage: \"klingon\"\n";
        let err =
            serde_yaml::from_str::<InboundBinding>(y).expect_err("klingon on binding rejects");
        let msg = format!("{err}");
        assert!(
            msg.contains("invalid locale") || msg.contains("klingon"),
            "expected binding-language rejection, got: {msg}"
        );
    }
}
