use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    pub agents: Vec<AgentConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub id: String,
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
    /// Optional directory for per-session JSONL transcripts. Kept separate
    /// from `workspace` because workspaces are typically git-committed while
    /// transcripts contain PII. Empty = transcripts disabled.
    #[serde(default)]
    pub transcripts_dir: String,
    /// Dreaming sweep config — runs the memory consolidation pass on an
    /// interval when `dreaming.enabled` is true.
    #[serde(default)]
    pub dreaming: DreamingYamlConfig,
    /// Phase 10.9 — wrap the workspace directory in a local git repo for
    /// forensics, rollback, and LLM-inspectable history. Off by default.
    #[serde(default)]
    pub workspace_git: WorkspaceGitConfig,
    /// Phase 9.2 follow-up — per-tool rate limits. `None` = no limits.
    #[serde(default)]
    pub tool_rate_limits: Option<ToolRateLimitsConfig>,
    /// Phase 9.2 follow-up — opt-out JSON Schema validation of tool
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
    /// Which plugin topics this agent accepts inbound from. Empty =
    /// legacy wildcard (`plugin.inbound.>`, receive everything — matches
    /// pre-binding behavior). Populated = strict allowlist.
    ///
    /// Topic parse rules:
    ///   `plugin.inbound.<plugin>`                → (plugin, None)
    ///   `plugin.inbound.<plugin>.<instance>`     → (plugin, Some(inst))
    /// A binding with `instance=None` matches any instance of `plugin`.
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

/// Matches inbound plugin events to an agent. A binding is "plugin X"
/// (any instance) or "plugin X, instance Y" (exact).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboundBinding {
    pub plugin: String,
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolArgsValidationConfig {
    #[serde(default = "default_tool_args_validation_enabled")]
    pub enabled: bool,
}

impl Default for ToolArgsValidationConfig {
    fn default() -> Self {
        Self { enabled: default_tool_args_validation_enabled() }
    }
}

fn default_tool_args_validation_enabled() -> bool {
    true
}

/// Per-tool rate limits. Pattern `_default` applies when no other
/// pattern matches; any `*` in the pattern is a wildcard.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRateLimitsConfig {
    #[serde(default)]
    pub patterns: std::collections::HashMap<String, ToolRateLimitSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRateLimitSpec {
    pub rps: f64,
    #[serde(default)]
    pub burst: u64,
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

/// YAML surface for dreaming. Mirrors `agent_core::agent::DreamingConfig`
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
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
