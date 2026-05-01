use super::behavior::{AgentBehavior, AgentTurnControl};
use super::context::AgentContext;
use super::skills::{render_system_blocks as render_skill_blocks, SkillLoader};
use super::tool_registry::ToolRegistry;
use super::transcripts::{TranscriptEntry, TranscriptRole, TranscriptWriter};
use super::types::{InboundMessage, MessagePriority, RunTrigger};
use super::workspace::{SessionScope, WorkspaceLoader};
use crate::session::types::{Interaction, Role};
use crate::telemetry::{
    inc_llm_requests_total, observe_cache_usage, observe_llm_latency_ms,
    observe_prompt_tokens_drift, observe_prompt_tokens_estimated,
};
use async_trait::async_trait;
use chrono::Utc;
use nexo_broker::{BrokerHandle, Event};
use nexo_llm::{
    collect_stream, Attachment, CachePolicy, ChatMessage, ChatRequest, ChatRole, LlmClient,
    ResponseContent,
};
use nexo_driver_types::{GoalId, MemoryExtractor};
use nexo_memory::EmailFollowupEntry;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
/// Decide whether a session is a private DM (main) or a shared surface.
/// `MEMORY.md` loads only for `Main` — shared scopes strip it at load time.
fn session_scope_for(msg: &InboundMessage) -> SessionScope {
    // Agent-to-agent delegation arrives with source_plugin="agent": the peer
    // agent is never the human, so MEMORY.md must stay out.
    if msg.source_plugin == "agent" {
        return SessionScope::Shared;
    }
    SessionScope::Main
}

const MAX_TRACKED_CACHE_BREAK_SESSIONS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheBreakRequestContext {
    provider: String,
    model: String,
    system_hash: u64,
}

impl CacheBreakRequestContext {
    fn from_request(provider: &str, model: &str, req: &ChatRequest) -> Self {
        Self {
            provider: provider.to_string(),
            model: model.to_string(),
            system_hash: prompt_shape_hash(req),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheBreakSnapshot {
    req: CacheBreakRequestContext,
    cache_read_input_tokens: u32,
    cache_creation_input_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheBreakEvent {
    previous_provider: String,
    new_provider: String,
    previous_model: String,
    new_model: String,
    previous_cache_read_input_tokens: u32,
    cache_read_input_tokens: u32,
    cache_creation_input_tokens: u32,
    drop_pct: u32,
    provider_changed: bool,
    model_changed: bool,
    system_prompt_changed: bool,
    suspected_breaker: String,
}

#[derive(Debug, Default)]
struct CacheBreakTracker {
    by_session: HashMap<String, CacheBreakSnapshot>,
}

impl CacheBreakTracker {
    fn observe(
        &mut self,
        session_id: &str,
        current: CacheBreakSnapshot,
    ) -> Option<CacheBreakEvent> {
        if !self.by_session.contains_key(session_id)
            && self.by_session.len() >= MAX_TRACKED_CACHE_BREAK_SESSIONS
        {
            if let Some(oldest_key) = self.by_session.keys().next().cloned() {
                self.by_session.remove(&oldest_key);
            }
        }
        let previous = self
            .by_session
            .insert(session_id.to_string(), current.clone())?;
        let prev_read = previous.cache_read_input_tokens;
        if prev_read == 0 {
            return None;
        }
        // Generic cache-break trigger across providers/models:
        // cache-read dropped by >50% turn-over-turn.
        if u64::from(current.cache_read_input_tokens).saturating_mul(2) >= u64::from(prev_read) {
            return None;
        }
        let provider_changed = previous.req.provider != current.req.provider;
        let model_changed = previous.req.model != current.req.model;
        let system_prompt_changed = previous.req.system_hash != current.req.system_hash;
        let mut breakers: Vec<&str> = Vec::new();
        if provider_changed {
            breakers.push("provider_swap");
        }
        if model_changed {
            breakers.push("model_swap");
        }
        if system_prompt_changed {
            breakers.push("system_prompt_mutation");
        }
        let suspected_breaker = if breakers.is_empty() {
            "unknown".to_string()
        } else {
            breakers.join(",")
        };
        let drop_pct = ((u64::from(prev_read.saturating_sub(current.cache_read_input_tokens))
            * 100)
            / u64::from(prev_read)) as u32;
        Some(CacheBreakEvent {
            previous_provider: previous.req.provider,
            new_provider: current.req.provider,
            previous_model: previous.req.model,
            new_model: current.req.model,
            previous_cache_read_input_tokens: prev_read,
            cache_read_input_tokens: current.cache_read_input_tokens,
            cache_creation_input_tokens: current.cache_creation_input_tokens,
            drop_pct,
            provider_changed,
            model_changed,
            system_prompt_changed,
            suspected_breaker,
        })
    }
}

fn cache_policy_tag(policy: CachePolicy) -> u8 {
    match policy {
        CachePolicy::None => 0,
        CachePolicy::Ephemeral5m => 1,
        CachePolicy::Ephemeral1h => 2,
    }
}

fn prompt_shape_hash(req: &ChatRequest) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(system) = req.system_prompt.as_deref() {
        "system_prompt".hash(&mut h);
        system.hash(&mut h);
    }
    for block in &req.system_blocks {
        "system_block".hash(&mut h);
        block.label.hash(&mut h);
        block.text.hash(&mut h);
        cache_policy_tag(block.cache).hash(&mut h);
    }
    h.finish()
}
pub struct LlmAgentBehavior {
    llm: Arc<dyn LlmClient>,
    tools: Arc<ToolRegistry>,
    hooks: Option<Arc<super::hook_registry::HookRegistry>>,
    max_tool_iterations: usize,
    rate_limiter: Option<Arc<super::rate_limit::ToolRateLimiter>>,
    schema_validator: Option<Arc<super::schema_validator::ToolArgsValidator>>,
    /// Sidecar policy: which tools are cacheable / parallel-safe.
    /// `ToolPolicy::disabled()` is the back-compat default — nothing
    /// cached, nothing parallel, identical behavior to pre-policy.
    tool_policy: Arc<super::tool_policy::ToolPolicy>,
    /// Cached relevance filter. Built once when `with_tool_policy` is
    /// called (tool set is stable over process lifetime). `None`
    /// means relevance filtering is disabled — every call passes the
    /// full catalog. Held under `RwLock` so a future hot-reload API
    /// can swap the index without rebuilding the behavior struct.
    tool_filter: Arc<tokio::sync::RwLock<Option<super::tool_filter::ToolFilter>>>,
    /// Hot path for workspace bundle reads. When `Some`, run_turn
    /// fetches via the cache (in-memory + notify invalidation); when
    /// `None`, falls back to a fresh `WorkspaceLoader` every turn
    /// (legacy behavior, kept for tests and bootstrap paths).
    workspace_cache: Option<Arc<super::workspace_cache::WorkspaceCache>>,
    /// Phase A.2 — when true, system prompt is emitted as
    /// `Vec<PromptBlock>` with `cache_control` breakpoints, and the
    /// tool catalog is marked cacheable. When false, the legacy flat
    /// `system_prompt: String` path runs (no provider-level caching).
    prompt_cache_enabled: bool,
    /// Phase C — pre-flight token counter. When `Some`, every request
    /// is sized before send; the estimated count is emitted as
    /// `llm_prompt_tokens_estimated` and post-response we record drift
    /// vs the provider's reported total. When `None`, counting is
    /// skipped entirely (zero overhead).
    token_counter: Option<Arc<dyn nexo_llm::TokenCounter>>,
    /// Phase B — online history compaction. All three must be wired
    /// together (compactor + store + runtime config). When any is
    /// missing, the compaction path is silently skipped — the agent
    /// loop falls back to the legacy "send the whole history" mode.
    compactor: Option<Arc<super::compaction::LlmCompactor>>,
    compaction_store: Option<Arc<nexo_memory::CompactionStore>>,
    compaction_runtime: CompactionRuntime,
    /// Phase 77.2 — runtime circuit-breaker state (Sync-safe).
    compaction_failures: std::sync::atomic::AtomicU32,
    compaction_last_turn: std::sync::Mutex<Option<u32>>,
    cache_break_tracker: Mutex<CacheBreakTracker>,
    /// Phase M4 — post-turn memory-extraction hook. When set,
    /// every successful `run_turn` ticks the extractor and (when
    /// `memory_dir` is also set + `reply_text` is `Some`) fires
    /// `extract(...)` against the conversation transcript.
    /// `Arc<dyn MemoryExtractor>` is provider-agnostic — the
    /// concrete `ExtractMemories` impl from `nexo-driver-loop` is
    /// the one we ship today, but any impl works.
    memory_extractor: Option<Arc<dyn MemoryExtractor>>,
    /// Phase M4 — destination root for extracted memories. Set
    /// together with `memory_extractor` via
    /// `with_memory_extractor`. `None` keeps `tick()` firing
    /// (cadence stays sane) but skips the actual `extract`
    /// call so we never write outside an explicit dir.
    memory_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RunTurnOutcome {
    Reply(Option<String>),
    Sleep { duration_ms: u64, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolExecutionResult {
    result: String,
    tool_err: Option<String>,
    outcome: &'static str,
    duration_ms: u64,
    sleep: Option<SleepSignal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SleepSignal {
    duration_ms: u64,
    reason: String,
}

/// Phase B — flattened compaction config that the agent loop reads on
/// every turn. Lives alongside the behavior so a hot-reload can swap
/// the whole struct via `Arc::make_mut` later (Phase F).
#[derive(Debug, Clone)]
pub struct CompactionRuntime {
    pub enabled: bool,
    /// Trigger threshold in tokens. When the pre-flight estimate
    /// crosses this, run compaction before the request.
    pub compact_at_tokens: u32,
    /// Minimum tail to preserve verbatim, in chars (≈4 chars/token).
    /// `find_safe_boundary` walks from the end until reaching this.
    pub tail_keep_chars: usize,
    /// Per-tool-result hard cap, in chars. Above this, the body is
    /// replaced by a `[truncated NNN bytes]` marker pre-send.
    pub tool_result_max_chars: usize,
    /// Phase 77.1 microcompact threshold. Tool results above this
    /// byte size are summarized before sending the next LLM request.
    pub micro_threshold_bytes: usize,
    /// Maximum summary body retained for one microcompacted tool result.
    pub micro_summary_max_chars: usize,
    /// Optional model override for microcompact. Empty = current turn model.
    pub micro_model: String,
    /// Lock TTL for `CompactionStore::try_acquire_lock`. Above this
    /// after a crash, the next acquire wins automatically.
    pub lock_ttl_seconds: u32,
    /// Override of the summary model. Empty = reuse the agent's main
    /// model.
    pub summarizer_model: String,
    // ── Phase 77.2 autoCompact ─────────────────────────────────────
    /// Token-pct trigger (0.0 disables token trigger).
    pub auto_token_pct: f32,
    /// Age trigger in minutes (0 disables age trigger).
    pub auto_max_age_minutes: u64,
    /// Safety margin below effective context window.
    pub auto_buffer_tokens: u64,
    /// Minimum turns between consecutive auto-compactions.
    pub auto_min_turns_between: u32,
    /// Consecutive failures that trip the circuit breaker.
    pub auto_max_consecutive_failures: u32,
}

impl Default for CompactionRuntime {
    fn default() -> Self {
        Self {
            enabled: false,
            compact_at_tokens: 75_000,
            tail_keep_chars: 80_000,       // ≈20K tokens
            tool_result_max_chars: 60_000, // ≈15K tokens; per-turn pre-send only
            micro_threshold_bytes: 16 * 1024,
            micro_summary_max_chars: 2048,
            micro_model: String::new(),
            lock_ttl_seconds: 300,
            summarizer_model: String::new(),
            auto_token_pct: 0.80,
            auto_max_age_minutes: 120,
            auto_buffer_tokens: 13_000,
            auto_min_turns_between: 5,
            auto_max_consecutive_failures: 3,
        }
    }
}
impl LlmAgentBehavior {
    pub fn new(llm: Arc<dyn LlmClient>, tools: Arc<ToolRegistry>) -> Self {
        Self {
            llm,
            tools,
            hooks: None,
            max_tool_iterations: 10,
            rate_limiter: None,
            schema_validator: None,
            tool_policy: super::tool_policy::ToolPolicy::disabled(),
            tool_filter: Arc::new(tokio::sync::RwLock::new(None)),
            workspace_cache: None,
            prompt_cache_enabled: false,
            token_counter: None,
            compactor: None,
            compaction_store: None,
            compaction_runtime: CompactionRuntime::default(),
            compaction_failures: std::sync::atomic::AtomicU32::new(0),
            compaction_last_turn: std::sync::Mutex::new(None),
            cache_break_tracker: Mutex::new(CacheBreakTracker::default()),
            memory_extractor: None,
            memory_dir: None,
        }
    }

    /// Phase M4 — wire post-turn memory extraction. When set,
    /// every successful `run_turn` ticks the extractor and fires
    /// extraction against `memory_dir`. Mirrors driver-loop's
    /// per-turn wire (`orchestrator.rs:702-726`); both engines
    /// share the same `Arc<ExtractMemories>` so cadence + circuit
    /// breaker + in-progress mutex stay coherent across paths.
    ///
    /// Provider-agnostic: `Arc<dyn MemoryExtractor>` keeps any
    /// concrete impl pluggable (today `ExtractMemories` from
    /// `nexo-driver-loop`).
    pub fn with_memory_extractor(
        mut self,
        extractor: Arc<dyn MemoryExtractor>,
        memory_dir: PathBuf,
    ) -> Self {
        self.memory_extractor = Some(extractor);
        self.memory_dir = Some(memory_dir);
        self
    }

    fn maybe_log_cache_break(
        &self,
        agent_id: &str,
        session_id: &str,
        req_ctx: CacheBreakRequestContext,
        cache_read_input_tokens: u32,
        cache_creation_input_tokens: u32,
    ) {
        let current = CacheBreakSnapshot {
            req: req_ctx,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        };
        let event = {
            let mut tracker = match self.cache_break_tracker.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            tracker.observe(session_id, current)
        };
        if let Some(event) = event {
            tracing::warn!(
                target: "llm.cache_break",
                agent_id = agent_id,
                session_id = session_id,
                previous_provider = %event.previous_provider,
                new_provider = %event.new_provider,
                previous_model = %event.previous_model,
                new_model = %event.new_model,
                previous_cache_read_input_tokens = event.previous_cache_read_input_tokens,
                cache_read_input_tokens = event.cache_read_input_tokens,
                cache_creation_input_tokens = event.cache_creation_input_tokens,
                drop_pct = event.drop_pct,
                provider_changed = event.provider_changed,
                model_changed = event.model_changed,
                system_prompt_changed = event.system_prompt_changed,
                suspected_breaker = %event.suspected_breaker,
                "llm.cache_break"
            );
        }
    }
    /// Phase B — wire the online compactor. All three handles must be
    /// supplied together; passing `enabled: true` in `runtime` without
    /// the wiring is a no-op (logged on first turn so the gap is
    /// visible). `summarizer` is the LLM client used to produce the
    /// summary itself — most operators reuse the agent's main model;
    /// pass a dedicated cheaper client to save spend.
    pub fn with_compaction(
        mut self,
        summarizer: Arc<dyn LlmClient>,
        store: Arc<nexo_memory::CompactionStore>,
        runtime: CompactionRuntime,
    ) -> Self {
        self.compactor = Some(Arc::new(super::compaction::LlmCompactor::new(summarizer)));
        self.compaction_store = Some(store);
        self.compaction_runtime = runtime;
        self
    }
    /// Phase C — attach a `TokenCounter`. Boot time pick this from
    /// `nexo_llm::token_counter::build()` based on
    /// `llm.context_optimization.token_counter.backend`. When omitted,
    /// pre-flight sizing is skipped (zero metrics, zero overhead).
    pub fn with_token_counter(mut self, counter: Arc<dyn nexo_llm::TokenCounter>) -> Self {
        self.token_counter = Some(counter);
        self
    }
    /// Attach the shared workspace cache. When set, `run_turn` reads
    /// the workspace bundle via `WorkspaceCache::get` (warm Arc, no
    /// disk I/O on the hot path); when omitted, falls back to a fresh
    /// `WorkspaceLoader` every turn (legacy / test path).
    pub fn with_workspace_cache(
        mut self,
        cache: Arc<super::workspace_cache::WorkspaceCache>,
    ) -> Self {
        self.workspace_cache = Some(cache);
        self
    }
    /// Phase A.2 — opt the agent into provider-level prompt caching.
    /// Driven from `llm.context_optimization.prompt_cache.enabled` (or
    /// the per-agent override added in Phase F). Defaults to false so
    /// the legacy non-cached path stays the safe fallback.
    pub fn with_prompt_cache(mut self, enabled: bool) -> Self {
        self.prompt_cache_enabled = enabled;
        self
    }
    /// Attach a tool-execution policy. Controls caching + parallel
    /// execution of tool calls. Defaults to a no-op policy.
    ///
    /// Pre-builds the relevance filter (if enabled) so the per-turn
    /// hot path stays O(1) instead of re-tokenizing the full tool
    /// catalog on every message.
    pub fn with_tool_policy(mut self, p: Arc<super::tool_policy::ToolPolicy>) -> Self {
        let rel = p.relevance_config().clone();
        if rel.enabled {
            let tool_defs = self.tools.to_tool_defs();
            let filter = super::tool_filter::ToolFilter::build(rel, &tool_defs);
            self.tool_filter = Arc::new(tokio::sync::RwLock::new(Some(filter)));
        }
        self.tool_policy = p;
        self
    }
    /// Rebuild the relevance filter index — call after the tool set
    /// changes (extension hot-reload, runtime registration). Idempotent.
    pub async fn rebuild_tool_filter(&self) {
        let rel = self.tool_policy.relevance_config().clone();
        if !rel.enabled {
            *self.tool_filter.write().await = None;
            return;
        }
        let tool_defs = self.tools.to_tool_defs();
        let filter = super::tool_filter::ToolFilter::build(rel, &tool_defs);
        *self.tool_filter.write().await = Some(filter);
    }
    pub fn with_max_iterations(mut self, n: usize) -> Self {
        self.max_tool_iterations = n;
        self
    }
    /// Attach an extension hook registry. Without this, hook fire sites are
    /// no-ops and behavior is identical to pre-11.6 operation.
    pub fn with_hooks(mut self, hooks: Arc<super::hook_registry::HookRegistry>) -> Self {
        self.hooks = Some(hooks);
        self
    }
    /// Phase 9.2 follow-up — attach per-tool rate limiter. Denied calls
    /// surface as `outcome="rate_limited"` and are not routed to the
    /// handler.
    pub fn with_rate_limiter(mut self, rl: Arc<super::rate_limit::ToolRateLimiter>) -> Self {
        self.rate_limiter = Some(rl);
        self
    }
    /// Phase 9.2 follow-up — attach the JSON Schema args validator.
    /// Denied calls surface as `outcome="invalid_args"` with the path
    /// of the offending field(s) in the result, so the LLM can retry.
    pub fn with_schema_validator(
        mut self,
        v: Arc<super::schema_validator::ToolArgsValidator>,
    ) -> Self {
        self.schema_validator = Some(v);
        self
    }
    /// Execute a single tool call end-to-end: hooks → rate limit →
    /// schema → cache lookup → handler → cache store. Caller picks the
    /// concurrency pattern (serial vs `join_all`).
    ///
    /// Returns a structured result so control-flow tools (notably
    /// `Sleep`) can stop the LLM loop without brittle string parsing.
    /// Telemetry + `after_tool_call` hook are fired by the caller so
    /// those observations stay in LLM-emitted order even when we
    /// parallelise.
    async fn execute_one_call(
        &self,
        call: &nexo_llm::ToolCall,
        msg: &InboundMessage,
        ctx: &AgentContext,
    ) -> ToolExecutionResult {
        let args = inject_runtime_tool_args(&call.name, call.arguments.clone(), msg);
        tracing::debug!(
            agent_id = %ctx.agent_id,
            session_id = %msg.session_id,
            message_id = %msg.id,
            tool = %call.name,
            tool_call_id = %call.id,
            "tool call dispatch"
        );
        // Phase 79.1 — centralised plan-mode gate. Runs before any
        // other check so the refusal message stays consistent with
        // the structured `PlanModeRefusal` shape regardless of which
        // downstream gate would have matched. The Bash classifier
        // verdict is `None` until Phase 77.8 ships — `gate_tool_call`
        // treats `Bash + None` as fail-safe blocking, which matches
        // the spec ("default to blocking if classifier returns
        // Unknown").
        {
            let state = ctx.plan_mode.read().await;
            if let Some(refusal) = crate::plan_mode::gate_tool_call(&state, &call.name, None) {
                let body = serde_json::json!({
                    "is_error": true,
                    "kind": "plan_mode_refusal",
                    "refusal": refusal,
                });
                let err = format!("plan_mode: refused {} ({:?})", call.name, refusal.tool_kind);
                tracing::info!(
                    agent_id = %ctx.agent_id,
                    tool = %call.name,
                    "plan_mode gate refused tool call"
                );
                return ToolExecutionResult {
                    result: body.to_string(),
                    tool_err: Some(err),
                    outcome: "plan_mode_refused",
                    duration_ms: 0,
                    sleep: None,
                };
            }
        }
        // Defense-in-depth: enforce the per-binding `allowed_tools`
        // list at execution time. The tool was already hidden from
        // the LLM's tool_defs for this binding (see filter below at
        // the turn-entry point), so a matching call usually means the
        // model is hallucinating the name — returning a clear error
        // keeps the turn bounded instead of either executing the
        // forbidden tool or letting the model retry the same call.
        let effective_tools = ctx.effective_policy();
        if !effective_tools.tool_allowed(&call.name) {
            let msg_str = format!(
                "tool `{}` is not available on this binding (agent `{}`)",
                call.name, ctx.agent_id
            );
            return ToolExecutionResult {
                result: msg_str.clone(),
                tool_err: Some(msg_str),
                outcome: "not_allowed",
                duration_ms: 0,
                sleep: None,
            };
        }
        // Phase 11.6 — before_tool_call hook.
        let mut skip_call = None;
        if let Some(hooks) = &self.hooks {
            let ev = serde_json::json!({
                "agent_id": ctx.agent_id,
                "session_id": msg.session_id.to_string(),
                "tool_name": call.name,
                "arguments": args,
            });
            if let super::hook_registry::HookOutcome::Aborted { plugin_id, reason } =
                hooks.fire("before_tool_call", ev).await
            {
                skip_call = Some(format!(
                    "tool `{}` blocked by extension `{}`: {}",
                    call.name,
                    plugin_id,
                    reason.unwrap_or_else(|| "(no reason)".into())
                ));
            }
        }
        let started_tool = std::time::Instant::now();
        let call_ctx = ctx.clone().with_session_id(msg.session_id);
        // Phase 82.7 — rate-limit lookup is binding-aware. The
        // limiter resolves per-binding overrides
        // (`ctx.effective.tool_rate_limits`) before the global
        // pattern set; bucket cardinality is per
        // `(agent, binding_id, tool)` so a single binding can't
        // starve other bindings on the same agent.
        let binding_id_owned = ctx.binding.as_ref().and_then(|b| b.binding_id.clone());
        let per_binding_override = ctx
            .effective
            .as_ref()
            .and_then(|p| p.tool_rate_limits.clone());
        let rate_allowed = match &self.rate_limiter {
            Some(rl) if skip_call.is_none() => {
                rl.try_acquire_with_binding(
                    &ctx.agent_id,
                    binding_id_owned.as_deref(),
                    &call.name,
                    per_binding_override.as_ref(),
                )
                .await
            }
            _ => true,
        };
        if !rate_allowed {
            // Phase 82.7 — Phase 72 turn-log marker on denial so
            // operator audit queries can identify which
            // `(binding, tool)` pairs hit caps most. The marker
            // is wire-shape stable; downstream billing pipelines
            // parse the format documented on `format_rate_limit_hit`.
            //
            // Resolved rps lookup: we redo the resolve here purely
            // for the marker — the limiter consumed the bucket
            // already and we don't need its f64 internally. Falls
            // back to 0.0 when the configured pattern is gone (race
            // with hot-reload); the marker still carries enough
            // signal to identify the binding and tool.
            let rps_for_marker = per_binding_override
                .as_ref()
                .and_then(|over| {
                    over.patterns
                        .iter()
                        .find(|(p, _)| {
                            super::rate_limit::glob_matches(p, &call.name)
                        })
                        .or_else(|| over.patterns.get_key_value("_default"))
                        .map(|(_, spec)| spec.rps)
                })
                .unwrap_or(0.0);
            tracing::info!(
                agent_id = %ctx.agent_id,
                marker = %nexo_tool_meta::format_rate_limit_hit(
                    &call.name,
                    binding_id_owned.as_deref(),
                    rps_for_marker,
                ),
                "tool call rate-limited"
            );
        }
        let schema_error: Option<String> = match &self.schema_validator {
            Some(v) if skip_call.is_none() && rate_allowed => {
                if let Some((def, _)) = self.tools.get(&call.name) {
                    match v.validate(&def, &args) {
                        Ok(()) => None,
                        Err(errs) => Some(errs.join("; ")),
                    }
                } else {
                    None
                }
            }
            _ => None,
        };
        let cache_hit: Option<serde_json::Value> =
            if skip_call.is_none() && rate_allowed && schema_error.is_none() {
                self.tool_policy.cache_get(&ctx.agent_id, &call.name, &args)
            } else {
                None
            };
        let (result, tool_err, outcome, sleep) = match (skip_call, schema_error) {
            (Some(msg_str), _) => (
                msg_str,
                Some("blocked-by-hook".to_string()),
                "blocked",
                None,
            ),
            (None, _) if !rate_allowed => {
                let msg_str = format!(
                    "rate limited: exceeded configured rps for tool '{}'",
                    call.name
                );
                (msg_str.clone(), Some(msg_str), "rate_limited", None)
            }
            (None, Some(errs)) => {
                let msg = format!("invalid arguments: {errs}");
                (msg.clone(), Some(msg), "invalid_args", None)
            }
            (None, None) => {
                if let Some(v) = cache_hit {
                    tracing::debug!(
                        agent_id = %ctx.agent_id,
                        tool = %call.name,
                        "tool cache hit"
                    );
                    let sleep = sleep_signal_from_value(&v);
                    (stringify_tool_result(&v), None, "cache_hit", sleep)
                } else {
                    match self.tools.get(&call.name) {
                        Some((_, handler)) => {
                            // Apply per-call timeout from policy — a slow
                            // tool call is cancelled rather than blocking
                            // the parallel batch indefinitely.
                            let to = std::time::Duration::from_secs(
                                self.tool_policy.parallel_config().call_timeout_secs,
                            );
                            match tokio::time::timeout(to, handler.call(&call_ctx, args.clone()))
                                .await
                            {
                                Ok(Ok(v)) => {
                                    self.tool_policy.cache_put(
                                        &ctx.agent_id,
                                        &call.name,
                                        &args,
                                        v.clone(),
                                    );
                                    let sleep = sleep_signal_from_value(&v);
                                    (stringify_tool_result(&v), None, "ok", sleep)
                                }
                                Ok(Err(e)) => {
                                    (format!("error: {e}"), Some(e.to_string()), "error", None)
                                }
                                Err(_) => {
                                    let msg = format!(
                                        "timeout after {}s for tool '{}'",
                                        to.as_secs(),
                                        call.name
                                    );
                                    (msg.clone(), Some(msg), "timeout", None)
                                }
                            }
                        }
                        None => (
                            format!("unknown tool: {}", call.name),
                            Some(format!("unknown tool: {}", call.name)),
                            "unknown",
                            None,
                        ),
                    }
                }
            }
        };
        let duration_ms = started_tool.elapsed().as_millis() as u64;
        // Surface every tool invocation at INFO so operators can
        // diagnose "why did the agent not call X" or "why did X fail
        // silently" without flipping the whole crate to debug.
        let preview: String = result.chars().take(160).collect::<String>();
        tracing::info!(
            agent_id = %ctx.agent_id,
            tool = %call.name,
            outcome,
            duration_ms,
            error = tool_err.as_deref().unwrap_or(""),
            result_preview = %preview,
            "tool executed"
        );
        ToolExecutionResult {
            result,
            tool_err,
            outcome,
            duration_ms,
            sleep,
        }
    }
    async fn run_turn(
        &self,
        ctx: &AgentContext,
        msg: InboundMessage,
        publish_reply: bool,
    ) -> anyhow::Result<RunTurnOutcome> {
        tracing::info!(
            agent_id = %ctx.agent_id,
            session_id = %msg.session_id,
            message_id = %msg.id,
            trigger = ?msg.trigger,
            source_plugin = %msg.source_plugin,
            publish_reply,
            "agent turn started"
        );
        // Phase 11.6 — before_message hook. Extensions can short-circuit the
        // turn (e.g. content filter, rate-limiter, observability gate).
        if let Some(hooks) = &self.hooks {
            let event = serde_json::json!({
                "agent_id": ctx.agent_id,
                "session_id": msg.session_id.to_string(),
                "text": msg.text,
                "source": msg.source_plugin,
            });
            if let super::hook_registry::HookOutcome::Aborted { plugin_id, reason } =
                hooks.fire("before_message", event).await
            {
                tracing::warn!(
                    agent_id = %ctx.agent_id,
                    session_id = %msg.session_id,
                    message_id = %msg.id,
                    ext = %plugin_id,
                    reason = ?reason,
                    "before_message hook aborted the turn",
                );
                return Ok(RunTurnOutcome::Reply(None));
            }
        }
        let mut session = ctx.sessions.get_or_create(msg.session_id, &ctx.agent_id);
        // If session is new (empty history) and long-term memory is available,
        // prepend recent interactions from disk so the agent remembers past conversations.
        let mut prefix_messages: Vec<ChatMessage> = Vec::new();
        // Build the initial system message from three sources, in priority
        // order: workspace bundle (IDENTITY/SOUL/USER/AGENTS/recent notes/MEMORY),
        // then optional local skills, then inline `system_prompt`. All parts
        // are merged into one system ChatMessage to keep prompt caching stable.
        // Phase A.2 — collect the system prompt into named sections so
        // we can hand them to `prompt_assembly::build_blocks` with
        // explicit `CachePolicy` per block. Empty sections fall out
        // and never occupy a cache breakpoint.
        let mut workspace_section: Option<String> = None;
        let mut skills_section: Option<String> = None;
        let mut binding_glue_parts: Vec<String> = Vec::new();
        let mut channel_meta_parts: Vec<String> = Vec::new();

        let workspace_path = ctx.config.workspace.trim();
        if !workspace_path.is_empty() {
            let scope = session_scope_for(&msg);
            // Hot path: prefer the shared cache (Arc, no disk I/O).
            // Legacy fallback: fresh loader every turn — kept so tests
            // and bootstrap that don't wire a cache still work.
            let bundle_result = if let Some(cache) = self.workspace_cache.as_ref() {
                cache
                    .get(
                        std::path::Path::new(workspace_path),
                        scope,
                        &ctx.config.extra_docs,
                    )
                    .await
                    .map(Some)
            } else {
                WorkspaceLoader::new(workspace_path)
                    .load_with_extras(scope, &ctx.config.extra_docs)
                    .await
                    .map(|b| Some(std::sync::Arc::new(b)))
            };
            match bundle_result {
                Ok(Some(bundle)) => {
                    if let Some(blocks) = bundle.render_system_blocks() {
                        workspace_section = Some(blocks);
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    agent_id = %ctx.agent_id,
                    workspace = workspace_path,
                    error = %e,
                    "workspace load failed — falling back to system_prompt only"
                ),
            }
        }
        // Per-binding skills: pull the list from the effective policy so
        // a narrow binding can boot with zero skills loaded while a
        // wider binding on the same agent injects the full catalogue.
        // skills_dir stays agent-level because skills are physical files
        // shared across every binding.
        let effective = ctx.effective_policy();
        if !effective.skills.is_empty() {
            let skills_dir = ctx.config.skills_dir.trim();
            if skills_dir.is_empty() {
                tracing::warn!(
                    agent_id = %ctx.agent_id,
                    "skills configured but skills_dir is empty; skipping skill injection"
                );
            } else {
                let loader =
                    SkillLoader::new(skills_dir).with_overrides(ctx.config.skill_overrides.clone());
                let loaded = loader.load_many(&effective.skills).await;
                if let Some(blocks) = render_skill_blocks(&loaded) {
                    skills_section = Some(blocks);
                }
            }
        }
        // Peer directory — auto-rendered `# PEERS` block listing other
        // agents in the process. The LLM learns who it can delegate to
        // without the user having to hand-write `AGENTS.md`.
        if let Some(peers) = ctx.peers.as_ref() {
            if let Some(block) = peers.render_for(&ctx.agent_id, &effective.allowed_delegates) {
                binding_glue_parts.push(block);
            }
        }
        // Per-binding system prompt: agent-level base with an optional
        // `# CHANNEL ADDENDUM` block appended by EffectiveBindingPolicy.
        // Legacy bindingless code paths see the plain agent prompt via
        // from_agent_defaults.
        let system_prompt = effective.system_prompt.trim();
        if !system_prompt.is_empty() {
            binding_glue_parts.push(system_prompt.to_string());
        }
        // Per-binding output language directive. Workspace docs stay in
        // English (so recall, dreaming, and dev tooling read them
        // unchanged); this block tells the model to reply in the
        // configured language instead. Resolved with binding > agent
        // > none precedence inside EffectiveBindingPolicy.
        if let Some(lang) = effective.language.as_deref() {
            binding_glue_parts.push(format!(
                "# OUTPUT LANGUAGE\n\nRespond to the user in {lang}. \
                 Workspace docs (IDENTITY, SOUL, MEMORY, USER, AGENTS) and \
                 tool descriptions are in English — read them as-is, but \
                 your turn-final reply to the user must be in {lang}."
            ));
        }
        // Phase 21 — link understanding. When the agent has it
        // enabled and the user message contains URLs, fetch each one
        // and inject a `# LINK CONTEXT` block so the LLM has grounded
        // facts to reason over. Lives in `channel_meta_parts` so it
        // sits in the per-turn (non-cached) section of the prompt —
        // every turn fetches fresh and the cache is keyed on URL,
        // not on the prompt blob.
        if effective.link_understanding.enabled {
            if let Some(extractor) = ctx.link_extractor.as_ref() {
                let urls = crate::link_understanding::detect_urls(
                    &msg.text,
                    effective.link_understanding.max_links_per_turn,
                );
                if !urls.is_empty() {
                    let cfg = effective.link_understanding.clone();
                    let extractor = Arc::clone(extractor);
                    let mut summaries = Vec::with_capacity(urls.len());
                    for u in urls {
                        if let Some(s) = extractor.fetch(&u, &cfg).await {
                            summaries.push(s);
                        }
                    }
                    let block = crate::link_understanding::render_block(&summaries);
                    if !block.is_empty() {
                        channel_meta_parts.push(block);
                    }
                }
            }
        }

        // Inbound metadata — give the LLM the current sender so it
        // doesn't have to ask ("¿cuál es tu teléfono?") when the
        // channel already carries it (WhatsApp JID, Telegram user id,
        // email address). The runtime injects this every turn so even
        // mid-conversation it's always current. Lives in its own
        // (short-TTL) block because it varies per turn.
        if let Some(sender) = msg.sender_id.as_deref() {
            if !sender.is_empty() {
                channel_meta_parts.push(format!(
                    "# CONTEXTO DEL CANAL\n\nRemitente ({}): {}\n\nUsá este identificador como \"número del cliente\" cuando un prompt hable de capturar el teléfono.",
                    msg.source_plugin,
                    sender
                ));
            }
        }
        // Phase 79.1 — inject the canonical plan-mode hint while
        // plan mode is on. Frozen string keeps the prompt cache warm.
        if let Some(hint) = crate::plan_mode::plan_mode_system_hint(&*ctx.plan_mode.read().await) {
            channel_meta_parts.push(hint.to_string());
        }
        // Phase 77.20 — inject proactive + coordinator hints once (frozen
        // strings → prompt-cache-friendly, same pattern as plan_mode).
        if let Some(hint) =
            crate::agent::proactive_hint::proactive_system_hint(ctx.proactive_enabled)
        {
            channel_meta_parts.push(hint.to_string());
        }
        if let Some(hint) =
            crate::agent::proactive_hint::coordinator_system_hint(ctx.binding_role.as_deref())
        {
            channel_meta_parts.push(hint.to_string());
        }
        // Phase 80.15 — assistant-mode addendum. Append the resolved
        // text (operator override or bundled default) when the
        // boot-immutable flag is on. Same prompt-cache rules as the
        // proactive/coordinator hints — the addendum is stable across
        // turns so the cache stays warm.
        let assistant_addendum_appended = ctx.assistant.should_append_addendum();
        if assistant_addendum_appended {
            channel_meta_parts.push((*ctx.assistant.addendum).clone());
        }
        // Phase 80.8 — brief-mode "talking to the user" section.
        // Skipped when the assistant-mode addendum already covers
        // the same instruction (avoid duplicating the directive).
        if let Some(section) = crate::agent::send_user_message_tool::brief_system_section(
            ctx.config.brief.as_ref(),
            assistant_addendum_appended,
        ) {
            channel_meta_parts.push(section.to_string());
        }
        let prompt_inputs = super::prompt_assembly::PromptInputs {
            workspace: workspace_section,
            skills: skills_section,
            binding_glue: if binding_glue_parts.is_empty() {
                None
            } else {
                Some(binding_glue_parts.join("\n\n"))
            },
            channel_meta: if channel_meta_parts.is_empty() {
                None
            } else {
                Some(channel_meta_parts.join("\n\n"))
            },
        };
        let mut system_blocks = super::prompt_assembly::build_blocks(prompt_inputs);
        // Phase 79.2 — inject a stub block listing deferred tools by name
        // + description so the model can discover them via ToolSearch.
        let registry_for_deferred = ctx.effective_tools.as_ref().unwrap_or(&self.tools);
        if let Some(summary) = registry_for_deferred.deferred_tools_summary() {
            system_blocks.push(nexo_llm::PromptBlock::plain("deferred_tools", summary));
        }
        // Legacy flat string for providers that don't honor
        // `system_blocks` (and as a back-compat path when prompt_cache
        // is disabled). Cheap to build — `flatten_blocks` walks the
        // same Vec we just assembled.
        let flat_system = nexo_llm::flatten_blocks(&system_blocks);
        if !flat_system.is_empty() {
            prefix_messages.push(ChatMessage::system(flat_system));
        }
        if session.history.is_empty() {
            if let Some(ref memory) = ctx.memory {
                if let Ok(past) = memory.load_interactions(msg.session_id, 20).await {
                    for i in &past {
                        match i.role.as_str() {
                            "user" => prefix_messages.push(ChatMessage::user(&i.content)),
                            "assistant" => prefix_messages.push(ChatMessage::assistant(&i.content)),
                            _ => {}
                        }
                    }
                }
            }
        }
        session.push(Interaction::new(Role::User, &msg.text));

        // Phase B + 77.2 — pre-flight compaction trigger. Only runs when the
        // compactor is wired AND enabled in runtime config. Estimates
        // the would-be request size (system blocks + history); when
        // it exceeds `compact_at_tokens` OR the session is older than
        // `auto_max_age_minutes`, runs the summarizer on
        // `history[..tail_start]`, persists an audit row, and replaces
        // the head with a stored summary. The summary then gets
        // injected into `messages` below as a user/assistant pair so
        // role alternation stays valid for Anthropic.
        // Phase F follow-up — gate on BOTH the boot-wired flag AND the
        // current snapshot's resolved enable. A hot-reload that flips
        // `compaction: false` takes effect on this turn without
        // rebuilding the behavior. Legacy paths without a snapshot
        // (tests, heartbeat bootstrap) treat the live flag as `true`
        // so the boot-wired enable stays the only gate.
        let live_compaction = ctx
            .context_optimization
            .map(|co| co.compaction)
            .unwrap_or(true);
        if let (true, true, Some(compactor), Some(compaction_store)) = (
            self.compaction_runtime.enabled,
            live_compaction,
            self.compactor.as_ref(),
            self.compaction_store.as_ref(),
        ) {
            let est = if let Some(counter) = self.token_counter.as_ref() {
                let blocks_n = counter.count_blocks(&system_blocks).await.unwrap_or(0);
                let hist_msgs: Vec<ChatMessage> = session
                    .history
                    .iter()
                    .filter_map(|i| match i.role {
                        Role::User => Some(ChatMessage::user(&i.content)),
                        Role::Assistant => Some(ChatMessage::assistant(&i.content)),
                        Role::Tool => None,
                    })
                    .collect();
                let msg_n = counter
                    .count_messages(&effective.model.model, &hist_msgs)
                    .await
                    .unwrap_or(0);
                blocks_n.saturating_add(msg_n)
            } else {
                0
            };

            // ── Phase 77.2 autoCompact triggers ───────────────────
            let token_trigger =
                est >= self.compaction_runtime.compact_at_tokens;
            let age_minutes = chrono::Utc::now()
                .signed_duration_since(session.created_at)
                .num_minutes()
                .max(0) as u64;
            let age_trigger = self.compaction_runtime.auto_max_age_minutes > 0
                && age_minutes >= self.compaction_runtime.auto_max_age_minutes;

            // Circuit breaker: skip when too many consecutive failures.
            let failures = self
                .compaction_failures
                .load(std::sync::atomic::Ordering::Relaxed);
            let breaker_tripped = self.compaction_runtime.auto_max_consecutive_failures > 0
                && failures >= self.compaction_runtime.auto_max_consecutive_failures;

            // Anti-storm: respect min turns between compactions.
            let current_turns = session.history.len() as u32;
            let last_turn: Option<u32> = *self.compaction_last_turn.lock().unwrap();
            let min_gap_ok = match last_turn {
                Some(last) => {
                    current_turns.saturating_sub(last)
                        >= self.compaction_runtime.auto_min_turns_between
                }
                None => true,
            };

            let should_compact = (token_trigger || age_trigger)
                && !breaker_tripped
                && min_gap_ok;

            if should_compact {
                if let Some(boundary) = super::compaction::find_safe_boundary(
                    &session.history,
                    self.compaction_runtime.tail_keep_chars,
                ) {
                    let store = compaction_store;
                    let acquired = store
                        .try_acquire_lock(
                            session.id,
                            &format!("pid:{}", std::process::id()),
                            self.compaction_runtime.lock_ttl_seconds,
                        )
                        .await
                        .unwrap_or(false);
                    if acquired {
                        let started = std::time::Instant::now();
                        let model = if self.compaction_runtime.summarizer_model.is_empty() {
                            effective.model.model.clone()
                        } else {
                            self.compaction_runtime.summarizer_model.clone()
                        };
                        let budget = super::compaction::CompactionBudget {
                            target_tokens: self.compaction_runtime.compact_at_tokens,
                            tail_keep_tokens: (self.compaction_runtime.tail_keep_chars / 4) as u32,
                            model: model.clone(),
                        };
                        let result = compactor.compact(&session.history, boundary, &budget).await;
                        let elapsed_ms = started.elapsed().as_millis() as u64;
                        match result {
                            Ok(r) => {
                                let row = nexo_memory::CompactionRow {
                                    session_id: session.id.to_string(),
                                    compacted_at: chrono::Utc::now().timestamp_millis(),
                                    head_turn_count: r.head_turns_summarized as i64,
                                    tail_start_index: r.tail_start_index as i64,
                                    summary: r.summary.clone(),
                                    model_used: model,
                                    input_tokens: r.input_tokens as i64,
                                    output_tokens: r.output_tokens as i64,
                                };
                                if let Err(e) = store.insert(&row).await {
                                    tracing::warn!(
                                        error = %e,
                                        session_id = %session.id,
                                        "compaction succeeded but persist failed; \
                                         applying anyway and continuing"
                                    );
                                }
                                session.apply_compaction(r.summary, r.tail_start_index);
                                // Phase 77.2 — reset circuit breaker on success.
                                self.compaction_failures
                                    .store(0, std::sync::atomic::Ordering::Relaxed);
                                *self.compaction_last_turn.lock().unwrap() =
                                    Some(session.history.len() as u32);
                                crate::telemetry::observe_compaction(
                                    &ctx.agent_id,
                                    "ok",
                                    elapsed_ms,
                                );
                                tracing::info!(
                                    session_id = %session.id,
                                    head_turns = r.head_turns_summarized,
                                    duration_ms = elapsed_ms,
                                    trigger = if token_trigger { "token" } else { "age" },
                                    age_minutes = age_minutes,
                                    "compaction applied"
                                );
                            }
                            Err(e) => {
                                // Phase 77.2 — increment circuit breaker.
                                let new_failures = self
                                    .compaction_failures
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                    .saturating_add(1);
                                crate::telemetry::observe_compaction(
                                    &ctx.agent_id,
                                    "failed",
                                    elapsed_ms,
                                );
                                tracing::warn!(
                                    error = %e,
                                    session_id = %session.id,
                                    consecutive_failures = new_failures,
                                    "compaction failed — continuing with original history"
                                );
                            }
                        }
                        let _ = store.release_lock(session.id).await;
                    } else {
                        crate::telemetry::observe_compaction(&ctx.agent_id, "lock_held", 0);
                        tracing::debug!(
                            session_id = %session.id,
                            "compaction lock held by another holder; skipping"
                        );
                    }
                } else {
                    crate::telemetry::observe_compaction(&ctx.agent_id, "no_boundary", 0);
                }
            }
        }

        // Build message list: historical prefix + compacted summary
        // (when present) + current session turns.
        let mut messages: Vec<ChatMessage> = prefix_messages;
        if let Some(summary) = session.compacted_summary.as_ref() {
            // Inject as user/assistant pair so Anthropic's strict
            // alternation rule never sees user-user. The synthetic
            // ack tells the model the summary is authoritative
            // context, not a fresh user request.
            messages.push(ChatMessage::user(format!(
                "<COMPACTED SUMMARY OF EARLIER TURNS>\n{}\n</COMPACTED SUMMARY>",
                summary
            )));
            messages.push(ChatMessage::assistant(
                "Got it — continuing from the summary above.",
            ));
        }
        messages.extend(session.history.iter().filter_map(|i| match i.role {
            Role::User => Some(ChatMessage::user(&i.content)),
            Role::Assistant => Some(ChatMessage::assistant(&i.content)),
            Role::Tool => None,
        }));
        // Attach inbound media to the latest user turn. Gemini consumes
        // image/audio/video parts inline; providers that do not support
        // a media kind simply skip it while keeping the text turn.
        if let Some(media) = msg.media.as_ref() {
            if let Some(att) = build_media_attachment(media) {
                if let Some(last_user) = messages
                    .iter_mut()
                    .rev()
                    .find(|m| matches!(m.role, ChatRole::User))
                {
                    last_user.attachments.push(att);
                }
            }
        }
        // Per-binding model override: ctx.effective carries the model
        // string resolved by EffectiveBindingPolicy. Agent-level config
        // is only consulted via the `effective_policy()` fallback when
        // the context was built outside of a matched binding (heartbeat
        // bootstrap, tests). The provider stays at whatever the agent
        // was booted with — boot validation rejects bindings that try
        // to change `model.provider` because the LLM client is wired
        // once per agent. Switching only the model name works because
        // providers ship multiple model variants behind a single API.
        let effective_policy = ctx.effective_policy();
        let model = effective_policy.model.model.clone();
        // Prefer the pre-filtered per-binding registry attached by
        // AgentRuntime (see `with_tool_base`). Falls back to the
        // behavior's base registry + a per-turn filter when the
        // runtime wasn't given a tool base (legacy tests, no-LLM
        // behaviors). Both paths produce the same visible surface;
        // the cached path skips the clone-per-turn.
        let tool_defs: Vec<_> = match ctx.effective_tools.as_ref() {
            Some(pre) => pre.to_tool_defs_non_deferred(),
            None => self
                .tools
                .to_tool_defs_non_deferred()
                .into_iter()
                .filter(|d| effective_policy.tool_allowed(&d.name))
                .collect(),
        };
        // Phase 3 optimisation: the relevance filter index is built
        // once at agent boot (see `with_tool_policy`). We just borrow
        // the prebuilt index here and score against a query built
        // from the current user message plus the last few turns of
        // conversation history so multi-turn threads ("and in
        // Medellín?") don't lose weather tools because the literal
        // message is short.
        let filtered_tools = {
            let filter_guard = self.tool_filter.read().await;
            match filter_guard.as_ref() {
                Some(filter) if filter.enabled() => {
                    let mut query = String::with_capacity(msg.text.len() + 256);
                    query.push_str(&msg.text);
                    // Tail of conversation for context; cap lookback
                    // so a long session doesn't push the query into
                    // irrelevant domains.
                    const CTX_LOOKBACK: usize = 3;
                    for i in session.history.iter().rev().take(CTX_LOOKBACK) {
                        query.push(' ');
                        query.push_str(&i.content);
                    }
                    let picked = filter.filter(&query, &tool_defs);
                    tracing::info!(
                        agent_id = %ctx.agent_id,
                        session_id = %msg.session_id,
                        full = tool_defs.len(),
                        kept = picked.len(),
                        "tool relevance filter applied"
                    );
                    picked
                }
                _ => tool_defs.clone(),
            }
        };
        let mut reply_text: Option<String> = None;
        let mut sleep_signal: Option<SleepSignal> = None;
        for iteration in 0..self.max_tool_iterations {
            // Phase 77.1 — microcompact oversized tool results in the
            // request clone only. The canonical in-memory messages keep
            // the full body and the tool_call_id/name correlation stays
            // intact in the compacted clone.
            let mut messages_for_send = messages.clone();
            if live_compaction && self.compaction_runtime.micro_threshold_bytes > 0 {
                let stats = if self.compaction_runtime.micro_model.is_empty() {
                    super::compaction::clear_large_compactable_tool_results(
                        &mut messages_for_send,
                        self.compaction_runtime.micro_threshold_bytes,
                    )
                } else {
                    let budget = super::compaction::MicroCompactBudget {
                        threshold_bytes: self.compaction_runtime.micro_threshold_bytes,
                        summary_max_chars: self.compaction_runtime.micro_summary_max_chars,
                        model: self.compaction_runtime.micro_model.clone(),
                    };
                    let stats = super::compaction::microcompact_large_tool_results(
                        &mut messages_for_send,
                        self.llm.as_ref(),
                        &budget,
                    )
                    .await;
                    if stats.failed > 0 {
                        super::compaction::clear_large_compactable_tool_results(
                            &mut messages_for_send,
                            self.compaction_runtime.micro_threshold_bytes,
                        )
                    } else {
                        stats
                    }
                };
                if stats.compacted > 0 {
                    crate::telemetry::observe_compaction(
                        &ctx.agent_id,
                        "tool_result_microcompact",
                        0,
                    );
                    tracing::info!(
                        agent_id = %ctx.agent_id,
                        compacted = stats.compacted,
                        original_bytes = stats.original_bytes,
                        compacted_bytes = stats.compacted_bytes,
                        "microcompacted tool results before LLM request"
                    );
                }
            }
            if self.compaction_runtime.tool_result_max_chars > 0 {
                let truncated = super::compaction::truncate_large_tool_results(
                    &mut messages_for_send,
                    self.compaction_runtime.tool_result_max_chars,
                );
                if truncated > 0 {
                    crate::telemetry::observe_compaction(&ctx.agent_id, "tool_result_truncated", 0);
                }
            }
            let mut req = ChatRequest::new(&model, messages_for_send);
            req.tools = filtered_tools.clone();
            // Phase A.2 — wire the structured prompt + tool catalog
            // caching opt-in. Provider clients that don't honor the
            // fields fall back to flat `system_prompt`; the fields
            // are otherwise inert.
            let live_prompt_cache = ctx
                .context_optimization
                .map(|co| co.prompt_cache)
                .unwrap_or(true);
            if self.prompt_cache_enabled && live_prompt_cache {
                req.system_blocks = system_blocks.clone();
                req.cache_tools = !filtered_tools.is_empty();
            }
            tracing::debug!(
                agent_id = %ctx.agent_id,
                session_id = %msg.session_id,
                message_id = %msg.id,
                iteration,
                "llm chat request"
            );
            let provider = self.llm.provider();
            let model_label = self.llm.model_id();
            inc_llm_requests_total(&ctx.agent_id, provider, model_label);
            // Phase C — pre-flight token sizing. Counted on
            // (system_blocks + messages); count_tokens-backed
            // counters cache the stable prefix so 95%+ of the bytes
            // are a memory hit. Emits the estimate as a gauge; drift
            // vs actual lands in the histogram below after the
            // response.
            let estimated_tokens: u32 = if let Some(counter) = self.token_counter.as_ref() {
                let blocks_total = match counter.count_blocks(&system_blocks).await {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::debug!(error = %e, "pre-flight count_blocks failed");
                        0
                    }
                };
                let messages_total = match counter.count_messages(&model, &messages).await {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::debug!(error = %e, "pre-flight count_messages failed");
                        0
                    }
                };
                let total = blocks_total.saturating_add(messages_total);
                observe_prompt_tokens_estimated(
                    &ctx.agent_id,
                    provider,
                    model_label,
                    total,
                    counter.is_exact(),
                );
                total
            } else {
                0
            };
            let cache_break_req_ctx =
                CacheBreakRequestContext::from_request(provider, model_label, &req);
            let started_at = std::time::Instant::now();
            // Phase 3 follow-up: consume the streaming API in the
            // main loop so provider-native SSE paths are exercised
            // end-to-end (chat() remains the fallback in the trait).
            let response = collect_stream(self.llm.stream(req).await?).await?;
            observe_llm_latency_ms(
                &ctx.agent_id,
                provider,
                model_label,
                started_at.elapsed().as_millis() as u64,
            );
            // Phase A.2 — emit cache hit/miss metrics whenever the
            // provider returned `CacheUsage`. Off-by-default providers
            // pass `None` here, so dashboards only see real activity.
            if let Some(cu) = response.cache_usage.as_ref() {
                observe_cache_usage(&ctx.agent_id, provider, model_label, cu);
            }
            let cache_read_input_tokens = response
                .cache_usage
                .as_ref()
                .map(|u| u.cache_read_input_tokens)
                .unwrap_or(0);
            let cache_creation_input_tokens = response
                .cache_usage
                .as_ref()
                .map(|u| u.cache_creation_input_tokens)
                .unwrap_or(0);
            self.maybe_log_cache_break(
                &ctx.agent_id,
                &msg.session_id.to_string(),
                cache_break_req_ctx,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            );
            // Phase C — drift observation. Only meaningful when we
            // actually estimated and the provider actually reported a
            // total. `prompt_tokens` on Anthropic already folds cache
            // read+creation into the total, so the comparison stays
            // apples-to-apples regardless of cache hit status.
            if estimated_tokens > 0 && response.usage.prompt_tokens > 0 {
                observe_prompt_tokens_drift(
                    &ctx.agent_id,
                    provider,
                    model_label,
                    estimated_tokens,
                    response.usage.prompt_tokens,
                );
            }
            match response.content {
                ResponseContent::Text(text) => {
                    reply_text = Some(text.clone());
                    messages.push(ChatMessage::assistant(&text));
                    break;
                }
                ResponseContent::ToolCalls(calls) => {
                    let tool_names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
                    tracing::info!(
                        agent_id = %ctx.agent_id,
                        session_id = %msg.session_id,
                        message_id = %msg.id,
                        tool_calls = calls.len(),
                        tool_names = ?tool_names,
                        iteration,
                        "llm requested tool calls"
                    );
                    // Preserve the full tool_call metadata (id + name +
                    // arguments) so the next turn can emit matching
                    // `tool_use` blocks on the Anthropic wire. A pure
                    // text "[tool:foo]" summary loses the id and makes
                    // MiniMax reject the follow-up tool_result.
                    messages.push(ChatMessage::assistant_tool_calls(
                        calls.clone(),
                        String::new(),
                    ));
                    // Partition calls: parallel-safe batch runs
                    // concurrently (bounded by `parallel.max_in_flight`
                    // to protect downstream endpoints), the rest stays
                    // sequential (side-effect tools). Results merge
                    // back in the original LLM-emitted order so
                    // tool_use_id correlation stays consistent on the
                    // Anthropic wire.
                    use futures::stream::{FuturesUnordered, StreamExt};
                    use std::collections::HashMap;
                    use std::pin::Pin;
                    type BoxedCallFut<'a> = Pin<
                        Box<
                            dyn std::future::Future<Output = (usize, ToolExecutionResult)>
                                + Send
                                + 'a,
                        >,
                    >;
                    let (par_idx, seq_idx): (Vec<usize>, Vec<usize>) = (0..calls.len())
                        .partition(|i| self.tool_policy.is_parallel_safe(&calls[*i].name));
                    let par_cap = self.tool_policy.parallel_config().max_in_flight;
                    let mut in_flight: FuturesUnordered<BoxedCallFut<'_>> = FuturesUnordered::new();
                    let mut results_by_idx: HashMap<usize, ToolExecutionResult> = HashMap::new();
                    let mut par_queue = par_idx.into_iter();
                    let msg_ref: &InboundMessage = &msg;
                    let calls_ref: &[nexo_llm::ToolCall] = &calls;
                    // Prime the in-flight window.
                    while in_flight.len() < par_cap.max(1) {
                        match par_queue.next() {
                            Some(i) => {
                                let c = &calls_ref[i];
                                let fut: BoxedCallFut<'_> = Box::pin(async move {
                                    (i, self.execute_one_call(c, msg_ref, ctx).await)
                                });
                                in_flight.push(fut);
                            }
                            None => break,
                        }
                    }
                    while let Some((i, r)) = in_flight.next().await {
                        results_by_idx.insert(i, r);
                        if let Some(next_i) = par_queue.next() {
                            let c = &calls_ref[next_i];
                            let fut: BoxedCallFut<'_> = Box::pin(async move {
                                (next_i, self.execute_one_call(c, msg_ref, ctx).await)
                            });
                            in_flight.push(fut);
                        }
                    }
                    for i in seq_idx {
                        let c = &calls[i];
                        let r = self.execute_one_call(c, &msg, ctx).await;
                        results_by_idx.insert(i, r);
                    }
                    // Push tool_result messages in original order —
                    // also run `after_tool_call` hook + telemetry here
                    // so observers see calls in the order the LLM
                    // emitted them.
                    for (i, call) in calls.iter().enumerate() {
                        // Defensive: if a future bug leaves an index
                        // unscheduled (par/seq partition miss), synthesize
                        // an error tool_result so the agent loop keeps
                        // running. Panicking here kills the whole agent
                        // over one missing dispatch — not worth it.
                        let tool_result = results_by_idx.remove(&i).unwrap_or_else(|| {
                            tracing::error!(
                                session_id = %msg.session_id,
                                tool = %call.name,
                                index = i,
                                "tool call dispatch slot missing — emitting synthetic error"
                            );
                            ToolExecutionResult {
                                result: serde_json::json!({
                                    "error": "internal: tool dispatch slot missing",
                                })
                                .to_string(),
                                tool_err: Some("tool dispatch slot missing".to_string()),
                                outcome: "error",
                                duration_ms: 0,
                                sleep: None,
                            }
                        });
                        crate::telemetry::inc_tool_calls_total(
                            &ctx.agent_id,
                            &call.name,
                            tool_result.outcome,
                        );
                        crate::telemetry::observe_tool_latency_ms(
                            &ctx.agent_id,
                            &call.name,
                            tool_result.duration_ms,
                        );
                        if let Some(hooks) = &self.hooks {
                            let ev = serde_json::json!({
                                "agent_id": ctx.agent_id,
                                "session_id": msg.session_id.to_string(),
                                "tool_name": call.name,
                                "duration_ms": tool_result.duration_ms,
                                "result": tool_result.result.clone(),
                                "error": tool_result.tool_err.clone(),
                            });
                            if let crate::agent::HookOutcome::Aborted { plugin_id, reason } =
                                hooks.fire("after_tool_call", ev).await
                            {
                                tracing::warn!(
                                    plugin = %plugin_id,
                                    reason = ?reason,
                                    hook = "after_tool_call",
                                    "extension hook aborted the chain"
                                );
                            }
                        }
                        if let Some(sleep) = tool_result.sleep.clone() {
                            sleep_signal = Some(sleep);
                        }
                        messages.push(ChatMessage::tool_result(
                            &call.id,
                            &call.name,
                            tool_result.result,
                        ));
                    }
                    if sleep_signal.is_some() {
                        tracing::info!(
                            agent_id = %ctx.agent_id,
                            session_id = %msg.session_id,
                            message_id = %msg.id,
                            "sleep tool requested proactive wake; stopping llm loop"
                        );
                        break;
                    }
                    if iteration + 1 >= self.max_tool_iterations {
                        tracing::warn!(
                            session_id = %msg.session_id,
                            "max tool iterations reached without text response"
                        );
                        break;
                    }
                }
            }
        }
        if let Some(ref text) = reply_text {
            session.push(Interaction::new(Role::Assistant, text));
        }
        ctx.sessions.update(session);
        // Persist user + assistant turns to long-term memory if available
        if let Some(ref memory) = ctx.memory {
            let _ = memory
                .save_interaction(msg.session_id, &ctx.agent_id, "user", &msg.text)
                .await;
            if let Some(ref text) = reply_text {
                let _ = memory
                    .save_interaction(msg.session_id, &ctx.agent_id, "assistant", text)
                    .await;
            }
        }
        // Persist turn to the session transcript (Phase 10.4) when the
        // operator has configured a transcripts_dir. Failures are logged
        // but never break the reply — transcripts are auxiliary state.
        let transcripts_dir = ctx.config.transcripts_dir.trim();
        if !transcripts_dir.is_empty() {
            let redactor = ctx
                .redactor
                .clone()
                .unwrap_or_else(|| std::sync::Arc::new(super::redaction::Redactor::disabled()));
            let writer = TranscriptWriter::with_extras(
                transcripts_dir,
                &ctx.agent_id,
                redactor,
                ctx.transcripts_index.clone(),
            );
            let user_entry = TranscriptEntry {
                timestamp: Utc::now(),
                role: TranscriptRole::User,
                content: msg.text.clone(),
                message_id: Some(msg.id),
                source_plugin: msg.source_plugin.clone(),
                sender_id: msg.sender_id.clone(),
            };
            if let Err(e) = writer.append_entry(msg.session_id, user_entry).await {
                tracing::warn!(
                    agent_id = %ctx.agent_id,
                    session_id = %msg.session_id,
                    error = %e,
                    "transcript append (user) failed"
                );
            }
            if let Some(ref text) = reply_text {
                let assistant_entry = TranscriptEntry {
                    timestamp: Utc::now(),
                    role: TranscriptRole::Assistant,
                    content: text.clone(),
                    message_id: None,
                    source_plugin: msg.source_plugin.clone(),
                    sender_id: None,
                };
                if let Err(e) = writer.append_entry(msg.session_id, assistant_entry).await {
                    tracing::warn!(
                        agent_id = %ctx.agent_id,
                        session_id = %msg.session_id,
                        error = %e,
                        "transcript append (assistant) failed"
                    );
                }
            }
        }
        if publish_reply {
            if let Some(text) = reply_text.clone() {
                let plugin = if msg.source_plugin.is_empty() {
                    "default"
                } else {
                    &msg.source_plugin
                };
                // When the inbound came from a labelled plugin instance
                // (e.g. `plugin.inbound.telegram.sales`), the matching
                // bot subscribes to `plugin.outbound.telegram.sales` —
                // publish there so only the originating bot replies. A
                // missing/empty instance falls back to the legacy topic.
                let topic = match msg.source_instance.as_deref() {
                    Some(inst) if !inst.is_empty() => {
                        format!("plugin.outbound.{}.{}", plugin, inst)
                    }
                    _ => format!("plugin.outbound.{}", plugin),
                };
                let payload = serde_json::json!({
                    "to": msg.sender_id,
                    "text": text,
                    "session_id": msg.session_id,
                });
                let mut event = Event::new(&topic, &ctx.agent_id, payload);
                event.session_id = Some(msg.session_id);
                ctx.broker.publish(&topic, event).await?;
                tracing::info!(
                    agent_id = %ctx.agent_id,
                    session_id = %msg.session_id,
                    message_id = %msg.id,
                    topic = %topic,
                    "agent reply published"
                );
            }
        }
        // Phase 11.6 — after_message hook (advisory). Only fire when we
        // actually produced a reply; silent turns don't trigger it.
        if let (Some(hooks), Some(text_out)) = (&self.hooks, reply_text.as_ref()) {
            let ev = serde_json::json!({
                "agent_id": ctx.agent_id,
                "session_id": msg.session_id.to_string(),
                "text_in": msg.text,
                "text_out": text_out,
            });
            if let crate::agent::HookOutcome::Aborted { plugin_id, reason } =
                hooks.fire("after_message", ev).await
            {
                tracing::warn!(
                    plugin = %plugin_id,
                    reason = ?reason,
                    hook = "after_message",
                    "extension hook aborted the chain"
                );
            }
        }
        tracing::info!(
            agent_id = %ctx.agent_id,
            session_id = %msg.session_id,
            message_id = %msg.id,
            produced_reply = reply_text.is_some(),
            sleep_requested = sleep_signal.is_some(),
            "agent turn finished"
        );

        // Phase M4 — post-turn memory extraction. Mirrors driver-loop's
        // wire at `orchestrator.rs:702-726`; both engines share the
        // same `Arc<dyn MemoryExtractor>` so cadence + circuit breaker
        // + in-progress mutex stay coherent across paths. `tick()`
        // runs every turn (cadence stays sane even when extract gates
        // skip); `extract(...)` only fires when `memory_dir` is set
        // AND `reply_text` carries the assistant turn text. Provider-
        // agnostic — the trait operates on transcript text, no LLM
        // provider assumption. `turn_index = 0` is an MVP sentinel
        // (regular AgentRuntime does not yet track per-session turn
        // counters; defer M4.c).
        if let Some(extractor) = &self.memory_extractor {
            extractor.tick();
            if let (Some(dir), Some(text)) = (&self.memory_dir, reply_text.as_ref()) {
                let goal_id = GoalId(msg.session_id);
                Arc::clone(extractor).extract(goal_id, 0, text.clone(), dir.clone());
            }
        }

        if let Some(sleep) = sleep_signal {
            Ok(RunTurnOutcome::Sleep {
                duration_ms: sleep.duration_ms,
                reason: sleep.reason,
            })
        } else {
            Ok(RunTurnOutcome::Reply(reply_text))
        }
    }
}
#[async_trait]
impl AgentBehavior for LlmAgentBehavior {
    async fn on_heartbeat(&self, ctx: &AgentContext) -> anyhow::Result<()> {
        tracing::debug!(agent_id = %ctx.agent_id, "heartbeat tick");
        let Some(memory) = ctx.memory.as_ref() else {
            return Ok(());
        };
        let due = memory
            .claim_due_reminders(&ctx.agent_id, Utc::now(), 32)
            .await?;
        for reminder in due {
            let topic = format!("plugin.outbound.{}", reminder.plugin);
            let payload = serde_json::json!({
                "to": reminder.recipient,
                "text": reminder.message,
                "session_id": reminder.session_id,
            });
            let mut event = Event::new(&topic, &ctx.agent_id, payload);
            event.session_id = Some(reminder.session_id);
            if let Err(e) = ctx.broker.publish(&topic, event).await {
                let _ = memory.release_reminder_claim(reminder.id).await;
                return Err(e.into());
            }
            let marked = memory.mark_reminder_delivered(reminder.id).await?;
            if marked {
                tracing::info!(
                    agent_id = %ctx.agent_id,
                    reminder_id = %reminder.id,
                    plugin = %reminder.plugin,
                    "delivered due reminder"
                );
            }
        }
        let due_followups = memory
            .claim_due_email_followups(&ctx.agent_id, Utc::now(), 16)
            .await?;
        for followup in due_followups {
            let attempt_number = followup.attempts.saturating_add(1);
            let flow_id = followup.flow_id;
            let mut tick = InboundMessage::new(
                followup.session_id,
                &ctx.agent_id,
                build_followup_tick_prompt(&followup, attempt_number),
            );
            tick.trigger = RunTrigger::Tick;
            tick.source_plugin = "followup".to_string();
            tick.source_instance = followup.source_instance.clone();
            tick.priority = MessagePriority::Later;
            // Phase 82.5 — followup ticks are scheduler-driven
            // (no end-user) → InternalSystem.
            tick.inbound = Some(
                nexo_tool_meta::InboundMessageMeta::internal_system()
                    .with_ts(Utc::now()),
            );

            match self.run_turn(ctx, tick, false).await {
                Ok(_) => {
                    let exhausted = attempt_number >= followup.max_attempts;
                    let next_check = if exhausted {
                        None
                    } else {
                        Some(Utc::now() + secs_to_chrono(followup.check_every_secs))
                    };
                    let applied = memory
                        .advance_email_followup_attempt(flow_id, next_check, None)
                        .await?;
                    if exhausted && applied {
                        tracing::info!(
                            agent_id = %ctx.agent_id,
                            flow_id = %flow_id,
                            attempts = followup.max_attempts,
                            "email follow-up exhausted max attempts"
                        );
                    } else if !applied {
                        tracing::debug!(
                            agent_id = %ctx.agent_id,
                            flow_id = %flow_id,
                            "email follow-up no longer active after autonomous turn"
                        );
                    }
                }
                Err(e) => {
                    let next_check = Utc::now() + secs_to_chrono(followup.check_every_secs);
                    let _ = memory
                        .requeue_email_followup_after_error(flow_id, next_check, &e.to_string())
                        .await;
                    tracing::warn!(
                        agent_id = %ctx.agent_id,
                        flow_id = %flow_id,
                        error = %e,
                        "email follow-up turn failed; re-queued"
                    );
                }
            }
        }
        Ok(())
    }
    async fn on_message_control(
        &self,
        ctx: &AgentContext,
        msg: InboundMessage,
    ) -> anyhow::Result<AgentTurnControl> {
        match self.run_turn(ctx, msg, true).await? {
            RunTurnOutcome::Reply(_) => Ok(AgentTurnControl::Done),
            RunTurnOutcome::Sleep {
                duration_ms,
                reason,
            } => Ok(AgentTurnControl::Sleep {
                duration_ms,
                reason,
            }),
        }
    }
    async fn on_message(&self, ctx: &AgentContext, msg: InboundMessage) -> anyhow::Result<()> {
        self.run_turn(ctx, msg, true).await?;
        Ok(())
    }
    async fn decide(&self, ctx: &AgentContext, msg: &InboundMessage) -> anyhow::Result<String> {
        match self.run_turn(ctx, msg.clone(), false).await? {
            RunTurnOutcome::Reply(reply) => Ok(reply.unwrap_or_default()),
            RunTurnOutcome::Sleep { .. } => Ok(String::new()),
        }
    }
    async fn on_event(&self, _ctx: &AgentContext, _event: Event) -> anyhow::Result<()> {
        Ok(())
    }
}
/// Turn an `InboundMedia` into an `Attachment` ready for the LLM wire.
/// Image / audio / video attachments ride on the provider wire directly
/// (Gemini accepts all three inline; Anthropic accepts images today —
/// non-image blocks are ignored by the Anthropic builder). Documents and
/// anything else flow through dedicated skills (whisper / pdf-extract /
/// video-frames) which read `media.path` out of band.
fn build_media_attachment(media: &super::types::InboundMedia) -> Option<Attachment> {
    let kind_hint = media.kind.as_str();
    let mime_hint = media.mime_type.as_deref();
    let (att_kind, mime) = if kind_hint == "photo"
        || kind_hint == "sticker"
        || mime_hint.map(|m| m.starts_with("image/")).unwrap_or(false)
    {
        (
            "image",
            mime_hint
                .map(str::to_string)
                .unwrap_or_else(|| guess_mime(&media.path, "image/jpeg")),
        )
    } else if kind_hint == "voice"
        || kind_hint == "audio"
        || mime_hint.map(|m| m.starts_with("audio/")).unwrap_or(false)
    {
        (
            "audio",
            mime_hint
                .map(str::to_string)
                .unwrap_or_else(|| guess_mime(&media.path, "audio/ogg")),
        )
    } else if kind_hint == "video"
        || kind_hint == "video_note"
        || kind_hint == "animation"
        || mime_hint.map(|m| m.starts_with("video/")).unwrap_or(false)
    {
        (
            "video",
            mime_hint
                .map(str::to_string)
                .unwrap_or_else(|| guess_mime(&media.path, "video/mp4")),
        )
    } else {
        return None;
    };
    let mut att = Attachment {
        kind: att_kind.to_string(),
        mime_type: mime,
        data: nexo_llm::AttachmentData::Path {
            path: media.path.clone(),
        },
    };
    if let Err(e) = att.materialize() {
        tracing::warn!(path = %media.path, kind = att_kind, error = %e, "failed to materialize inbound media; skipping");
        return None;
    }
    Some(att)
}

fn secs_to_chrono(raw_secs: u64) -> chrono::Duration {
    let secs = raw_secs.max(60).min(i64::MAX as u64) as i64;
    chrono::Duration::seconds(secs)
}

fn build_followup_tick_prompt(flow: &EmailFollowupEntry, attempt_number: u32) -> String {
    let instance = flow
        .source_instance
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    format!(
        "[followup_tick]\nflow_id: {}\nthread_root_id: {}\ninstance: {}\nrecipient: {}\nattempt: {}/{}\ninstruction: {}\n\nTarea: revisa el hilo en email instance={}, si el cliente ya respondió o el caso está resuelto llama cancel_followup {{ flow_id }}, si no respondió envía follow-up manteniendo threading.",
        flow.flow_id,
        flow.thread_root_id,
        instance,
        flow.recipient,
        attempt_number,
        flow.max_attempts,
        flow.instruction,
        instance,
    )
}
/// Best-effort MIME guess from extension, falling back to `default`.
fn guess_mime(path: &str, default: &str) -> String {
    let lower = path.to_ascii_lowercase();
    let ext = std::path::Path::new(&lower)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    match ext {
        // images
        "png" => "image/png".into(),
        "webp" => "image/webp".into(),
        "gif" => "image/gif".into(),
        "jpg" | "jpeg" => "image/jpeg".into(),
        // audio
        "oga" | "ogg" | "opus" => "audio/ogg".into(),
        "mp3" => "audio/mpeg".into(),
        "m4a" => "audio/mp4".into(),
        "wav" => "audio/wav".into(),
        "flac" => "audio/flac".into(),
        // video
        "mp4" | "m4v" => "video/mp4".into(),
        "webm" => "video/webm".into(),
        "mov" => "video/quicktime".into(),
        _ => default.to_string(),
    }
}
/// Tool handlers return `serde_json::Value`. Calling `.to_string()` on
/// a `Value::String` leaks the JSON quoting (`"hello"` instead of
/// `hello`). The rest of the pipeline expects plain text, so strip the
/// quotes for the string case and serialize everything else normally.
fn stringify_tool_result(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn sleep_signal_from_value(value: &serde_json::Value) -> Option<SleepSignal> {
    if !super::sleep_tool::is_sleep_result(value) {
        return None;
    }
    Some(SleepSignal {
        duration_ms: super::sleep_tool::extract_sleep_ms(value)?,
        reason: value
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("sleep requested")
            .to_string(),
    })
}

fn inject_runtime_tool_args(
    tool_name: &str,
    mut args: serde_json::Value,
    msg: &InboundMessage,
) -> serde_json::Value {
    if tool_name != "schedule_reminder" && tool_name != "delegate" {
        return args;
    }
    let Some(map) = args.as_object_mut() else {
        return args;
    };
    map.entry("session_id".to_string())
        .or_insert_with(|| serde_json::json!(msg.session_id.to_string()));
    map.entry("source_plugin".to_string())
        .or_insert_with(|| serde_json::json!(msg.source_plugin));
    map.entry("recipient".to_string())
        .or_insert_with(|| serde_json::json!(msg.sender_id));
    if tool_name == "delegate" {
        let ctx = map
            .entry("context".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(ctx_map) = ctx.as_object_mut() {
            ctx_map
                .entry("session_id".to_string())
                .or_insert_with(|| serde_json::json!(msg.session_id.to_string()));
            ctx_map
                .entry("source_plugin".to_string())
                .or_insert_with(|| serde_json::json!(msg.source_plugin));
            ctx_map
                .entry("sender_id".to_string())
                .or_insert_with(|| serde_json::json!(msg.sender_id));
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::super::types::InboundMedia;
    use super::*;
    use nexo_llm::AttachmentData;

    fn temp_media_file(name: &str, bytes: &[u8]) -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new()
            .prefix("media-")
            .suffix(name)
            .tempfile()
            .expect("create temp file");
        std::fs::write(file.path(), bytes).expect("write media bytes");
        file
    }

    #[test]
    fn build_media_attachment_voice_materializes_as_audio() {
        let file = temp_media_file(".ogg", b"ogg-bytes");
        let media = InboundMedia {
            kind: "voice".into(),
            path: file.path().display().to_string(),
            mime_type: None,
        };
        let att = build_media_attachment(&media).expect("voice media should attach");
        assert_eq!(att.kind, "audio");
        assert_eq!(att.mime_type, "audio/ogg");
        match att.data {
            AttachmentData::Base64 { base64 } => assert!(!base64.is_empty()),
            other => panic!("expected Base64 attachment, got {other:?}"),
        }
    }

    #[test]
    fn build_media_attachment_video_uses_kind_and_guessed_mime() {
        let file = temp_media_file(".WEBM", b"webm-bytes");
        let media = InboundMedia {
            kind: "video_note".into(),
            path: file.path().display().to_string(),
            mime_type: None,
        };
        let att = build_media_attachment(&media).expect("video_note media should attach");
        assert_eq!(att.kind, "video");
        assert_eq!(att.mime_type, "video/webm");
    }

    #[test]
    fn build_media_attachment_ignores_unsupported_kind() {
        let file = temp_media_file(".pdf", b"%PDF");
        let media = InboundMedia {
            kind: "document".into(),
            path: file.path().display().to_string(),
            mime_type: Some("application/pdf".into()),
        };
        assert!(build_media_attachment(&media).is_none());
    }

    #[test]
    fn sleep_signal_maps_sentinel_without_parsing_stringified_result() {
        let signal = sleep_signal_from_value(&serde_json::json!({
            "__nexo_sleep__": true,
            "duration_ms": 270_000,
            "reason": "waiting for work"
        }))
        .expect("sleep sentinel should map");

        assert_eq!(
            signal,
            SleepSignal {
                duration_ms: 270_000,
                reason: "waiting for work".into()
            }
        );
        assert!(sleep_signal_from_value(&serde_json::json!({"text": "normal"})).is_none());
    }

    fn req_for_cache_break(system: &str) -> ChatRequest {
        let mut req = ChatRequest::new("claude-sonnet-4-5", vec![ChatMessage::user("hola")]);
        req.system_prompt = Some(system.to_string());
        req
    }

    #[test]
    fn cache_break_tracker_hit_run_is_noop() {
        let mut tracker = CacheBreakTracker::default();
        let first = CacheBreakSnapshot {
            req: CacheBreakRequestContext::from_request(
                "anthropic",
                "claude-sonnet-4-5",
                &req_for_cache_break("stable"),
            ),
            cache_read_input_tokens: 8_000,
            cache_creation_input_tokens: 0,
        };
        let second = CacheBreakSnapshot {
            req: CacheBreakRequestContext::from_request(
                "anthropic",
                "claude-sonnet-4-5",
                &req_for_cache_break("stable"),
            ),
            cache_read_input_tokens: 7_500,
            cache_creation_input_tokens: 0,
        };
        assert!(tracker.observe("sess-1", first).is_none());
        assert!(tracker.observe("sess-1", second).is_none());
    }

    // ── Phase M4 — MemoryExtractor wire ──

    /// Minimal mock that records `tick` + `extract` calls so
    /// tests can assert the post-turn wire fired.
    struct MockExtractor {
        tick_count: std::sync::atomic::AtomicU32,
        extract_count: std::sync::atomic::AtomicU32,
        last_extract: std::sync::Mutex<Option<(GoalId, u32, String, std::path::PathBuf)>>,
    }

    impl Default for MockExtractor {
        fn default() -> Self {
            Self {
                tick_count: std::sync::atomic::AtomicU32::new(0),
                extract_count: std::sync::atomic::AtomicU32::new(0),
                last_extract: std::sync::Mutex::new(None),
            }
        }
    }

    impl MemoryExtractor for MockExtractor {
        fn tick(&self) {
            self.tick_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        fn extract(
            self: Arc<Self>,
            goal_id: GoalId,
            turn_index: u32,
            messages_text: String,
            memory_dir: std::path::PathBuf,
        ) {
            self.extract_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.last_extract.lock().unwrap() = Some((goal_id, turn_index, messages_text, memory_dir));
        }
    }

    fn dummy_behavior() -> LlmAgentBehavior {
        struct DummyClient;
        #[async_trait]
        impl LlmClient for DummyClient {
            async fn chat(
                &self,
                _req: ChatRequest,
            ) -> anyhow::Result<nexo_llm::ChatResponse> {
                anyhow::bail!("dummy client — unused in M4 tests")
            }
            fn model_id(&self) -> &str {
                "dummy"
            }
        }
        let llm: Arc<dyn LlmClient> = Arc::new(DummyClient);
        let tools = Arc::new(crate::agent::ToolRegistry::default());
        LlmAgentBehavior::new(llm, tools)
    }

    #[test]
    fn with_memory_extractor_populates_both_fields() {
        let mock: Arc<dyn MemoryExtractor> = Arc::new(MockExtractor::default());
        let dir = std::path::PathBuf::from("/tmp/nexo-test/memory");
        let b = dummy_behavior().with_memory_extractor(Arc::clone(&mock), dir.clone());
        assert!(b.memory_extractor.is_some());
        assert_eq!(b.memory_dir.as_deref(), Some(dir.as_path()));
    }

    #[test]
    fn default_behavior_has_no_memory_extractor() {
        let b = dummy_behavior();
        assert!(b.memory_extractor.is_none());
        assert!(b.memory_dir.is_none());
    }

    #[test]
    fn memory_extractor_records_tick_and_extract_calls() {
        // Simulate the post-turn wire by calling tick + extract
        // directly. Verifies the trait Arc<dyn> dispatch path
        // compiles AND the side effects land where the wire
        // expects them.
        let mock = Arc::new(MockExtractor::default());
        let extractor: Arc<dyn MemoryExtractor> = Arc::clone(&mock) as Arc<dyn MemoryExtractor>;
        extractor.tick();
        Arc::clone(&extractor).extract(
            GoalId(uuid::Uuid::nil()),
            0,
            "transcript".into(),
            std::path::PathBuf::from("/tmp/nexo-test/memory"),
        );
        assert_eq!(
            mock.tick_count.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            mock.extract_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let last = mock.last_extract.lock().unwrap().clone().unwrap();
        assert_eq!(last.1, 0);
        assert_eq!(last.2, "transcript");
    }

    #[test]
    fn cache_break_tracker_break_run_flags_system_mutation() {
        let mut tracker = CacheBreakTracker::default();
        let first = CacheBreakSnapshot {
            req: CacheBreakRequestContext::from_request(
                "anthropic",
                "claude-sonnet-4-5",
                &req_for_cache_break("stable"),
            ),
            cache_read_input_tokens: 8_000,
            cache_creation_input_tokens: 0,
        };
        let second = CacheBreakSnapshot {
            req: CacheBreakRequestContext::from_request(
                "anthropic",
                "claude-sonnet-4-5",
                &req_for_cache_break("mutated"),
            ),
            cache_read_input_tokens: 3_000,
            cache_creation_input_tokens: 200,
        };
        assert!(tracker.observe("sess-1", first).is_none());
        let ev = tracker
            .observe("sess-1", second)
            .expect("expected cache-break event");
        assert!(ev.system_prompt_changed);
        assert!(ev.suspected_breaker.contains("system_prompt_mutation"));
        assert_eq!(ev.previous_cache_read_input_tokens, 8_000);
        assert_eq!(ev.cache_read_input_tokens, 3_000);
    }
}
