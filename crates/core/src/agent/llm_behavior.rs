use std::sync::Arc;
use agent_broker::{BrokerHandle, Event};
use agent_llm::{Attachment, ChatMessage, ChatRequest, ChatRole, LlmClient, ResponseContent};
use async_trait::async_trait;
use chrono::Utc;
use super::behavior::AgentBehavior;
use super::context::AgentContext;
use super::skills::{render_system_blocks as render_skill_blocks, SkillLoader};
use super::tool_registry::ToolRegistry;
use super::transcripts::{TranscriptEntry, TranscriptRole, TranscriptWriter};
use super::types::InboundMessage;
use super::workspace::{SessionScope, WorkspaceLoader};
use crate::session::types::{Interaction, Role};
use crate::telemetry::{inc_llm_requests_total, observe_llm_latency_ms};
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
        }
    }
    /// Attach a tool-execution policy. Controls caching + parallel
    /// execution of tool calls. Defaults to a no-op policy.
    ///
    /// Pre-builds the relevance filter (if enabled) so the per-turn
    /// hot path stays O(1) instead of re-tokenizing the full tool
    /// catalog on every message.
    pub fn with_tool_policy(
        mut self,
        p: Arc<super::tool_policy::ToolPolicy>,
    ) -> Self {
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
    pub fn with_rate_limiter(
        mut self,
        rl: Arc<super::rate_limit::ToolRateLimiter>,
    ) -> Self {
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
    /// concurrency pattern (serial vs `join_all`). Returns
    /// `(result_text, tool_err, outcome_label, duration_ms)`; telemetry
    /// + `after_tool_call` hook are fired by the caller so those
    /// observations stay in LLM-emitted order even when we parallelise.
    async fn execute_one_call(
        &self,
        call: &agent_llm::ToolCall,
        msg: &InboundMessage,
        ctx: &AgentContext,
    ) -> (String, Option<String>, &'static str, u64) {
        let args = inject_runtime_tool_args(&call.name, call.arguments.clone(), msg);
        tracing::debug!(
            agent_id = %ctx.agent_id,
            session_id = %msg.session_id,
            message_id = %msg.id,
            tool = %call.name,
            tool_call_id = %call.id,
            "tool call dispatch"
        );
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
        let rate_allowed = match &self.rate_limiter {
            Some(rl) if skip_call.is_none() => {
                rl.try_acquire(&ctx.agent_id, &call.name).await
            }
            _ => true,
        };
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
        let (result, tool_err, outcome) = match (skip_call, schema_error) {
            (Some(msg_str), _) => {
                (msg_str, Some("blocked-by-hook".to_string()), "blocked")
            }
            (None, _) if !rate_allowed => {
                let msg_str = format!(
                    "rate limited: exceeded configured rps for tool '{}'",
                    call.name
                );
                (msg_str.clone(), Some(msg_str), "rate_limited")
            }
            (None, Some(errs)) => {
                let msg = format!("invalid arguments: {errs}");
                (msg.clone(), Some(msg), "invalid_args")
            }
            (None, None) => {
                if let Some(v) = cache_hit {
                    tracing::debug!(
                        agent_id = %ctx.agent_id,
                        tool = %call.name,
                        "tool cache hit"
                    );
                    (stringify_tool_result(&v), None, "cache_hit")
                } else {
                    match self.tools.get(&call.name) {
                        Some((_, handler)) => {
                            // Apply per-call timeout from policy — a slow
                            // tool call is cancelled rather than blocking
                            // the parallel batch indefinitely.
                            let to = std::time::Duration::from_secs(
                                self.tool_policy.parallel_config().call_timeout_secs,
                            );
                            match tokio::time::timeout(to, handler.call(&call_ctx, args.clone())).await {
                                Ok(Ok(v)) => {
                                    self.tool_policy.cache_put(
                                        &ctx.agent_id,
                                        &call.name,
                                        &args,
                                        v.clone(),
                                    );
                                    (stringify_tool_result(&v), None, "ok")
                                }
                                Ok(Err(e)) => {
                                    (format!("error: {e}"), Some(e.to_string()), "error")
                                }
                                Err(_) => {
                                    let msg = format!(
                                        "timeout after {}s for tool '{}'",
                                        to.as_secs(),
                                        call.name
                                    );
                                    (msg.clone(), Some(msg), "timeout")
                                }
                            }
                        }
                        None => (
                            format!("unknown tool: {}", call.name),
                            Some(format!("unknown tool: {}", call.name)),
                            "unknown",
                        ),
                    }
                }
            }
        };
        let duration_ms = started_tool.elapsed().as_millis() as u64;
        (result, tool_err, outcome, duration_ms)
    }
    async fn run_turn(
        &self,
        ctx: &AgentContext,
        msg: InboundMessage,
        publish_reply: bool,
    ) -> anyhow::Result<Option<String>> {
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
                return Ok(None);
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
        let mut system_parts: Vec<String> = Vec::new();
        let workspace_path = ctx.config.workspace.trim();
        if !workspace_path.is_empty() {
            let scope = session_scope_for(&msg);
            match WorkspaceLoader::new(workspace_path)
                .load_with_extras(scope, &ctx.config.extra_docs)
                .await
            {
                Ok(bundle) => {
                    if let Some(blocks) = bundle.render_system_blocks() {
                        system_parts.push(blocks);
                    }
                }
                Err(e) => tracing::warn!(
                    agent_id = %ctx.agent_id,
                    workspace = workspace_path,
                    error = %e,
                    "workspace load failed — falling back to system_prompt only"
                ),
            }
        }
        if !ctx.config.skills.is_empty() {
            let skills_dir = ctx.config.skills_dir.trim();
            if skills_dir.is_empty() {
                tracing::warn!(
                    agent_id = %ctx.agent_id,
                    "skills configured but skills_dir is empty; skipping skill injection"
                );
            } else {
                let loader = SkillLoader::new(skills_dir);
                let loaded = loader.load_many(&ctx.config.skills).await;
                if let Some(blocks) = render_skill_blocks(&loaded) {
                    system_parts.push(blocks);
                }
            }
        }
        // Peer directory — auto-rendered `# PEERS` block listing other
        // agents in the process. The LLM learns who it can delegate to
        // without the user having to hand-write `AGENTS.md`.
        if let Some(peers) = ctx.peers.as_ref() {
            if let Some(block) =
                peers.render_for(&ctx.agent_id, &ctx.config.allowed_delegates)
            {
                system_parts.push(block);
            }
        }
        let system_prompt = ctx.config.system_prompt.trim();
        if !system_prompt.is_empty() {
            system_parts.push(system_prompt.to_string());
        }
        if !system_parts.is_empty() {
            prefix_messages.push(ChatMessage::system(system_parts.join("\n\n")));
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
        // Build message list: historical prefix + current session turns
        let mut messages: Vec<ChatMessage> = prefix_messages;
        messages.extend(session.history.iter().filter_map(|i| match i.role {
            Role::User => Some(ChatMessage::user(&i.content)),
            Role::Assistant => Some(ChatMessage::assistant(&i.content)),
            Role::Tool => None,
        }));
        // Attach inbound media to the latest user turn so vision-capable
        // providers (Anthropic / Gemini) can see the photo / doc the
        // sender shared. Non-image media is still useful for skills
        // downstream — but only image attachments ride on the LLM wire.
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
        let model = ctx.config.model.model.clone();
        let tool_defs = self.tools.to_tool_defs();
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
        for iteration in 0..self.max_tool_iterations {
            let mut req = ChatRequest::new(&model, messages.clone());
            req.tools = filtered_tools.clone();
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
            let started_at = std::time::Instant::now();
            let response = self.llm.chat(req).await?;
            observe_llm_latency_ms(
                &ctx.agent_id,
                provider,
                model_label,
                started_at.elapsed().as_millis() as u64,
            );
            match response.content {
                ResponseContent::Text(text) => {
                    reply_text = Some(text.clone());
                    messages.push(ChatMessage::assistant(&text));
                    break;
                }
                ResponseContent::ToolCalls(calls) => {
                    tracing::info!(
                        agent_id = %ctx.agent_id,
                        session_id = %msg.session_id,
                        message_id = %msg.id,
                        tool_calls = calls.len(),
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
                            dyn std::future::Future<
                                    Output = (
                                        usize,
                                        (String, Option<String>, &'static str, u64),
                                    ),
                                > + Send
                                + 'a,
                        >,
                    >;
                    let (par_idx, seq_idx): (Vec<usize>, Vec<usize>) = (0..calls.len())
                        .partition(|i| self.tool_policy.is_parallel_safe(&calls[*i].name));
                    let par_cap = self.tool_policy.parallel_config().max_in_flight;
                    let mut in_flight: FuturesUnordered<BoxedCallFut<'_>> =
                        FuturesUnordered::new();
                    let mut results_by_idx: HashMap<
                        usize,
                        (String, Option<String>, &'static str, u64),
                    > = HashMap::new();
                    let mut par_queue = par_idx.into_iter();
                    let msg_ref: &InboundMessage = &msg;
                    let calls_ref: &[agent_llm::ToolCall] = &calls;
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
                        let (result, tool_err, outcome, duration_ms) =
                            results_by_idx.remove(&i).unwrap_or_else(|| {
                                tracing::error!(
                                    session_id = %msg.session_id,
                                    tool = %call.name,
                                    index = i,
                                    "tool call dispatch slot missing — emitting synthetic error"
                                );
                                (
                                    serde_json::json!({
                                        "error": "internal: tool dispatch slot missing",
                                    })
                                    .to_string(),
                                    Some("tool dispatch slot missing".to_string()),
                                    "error",
                                    0,
                                )
                            });
                        crate::telemetry::inc_tool_calls_total(&ctx.agent_id, &call.name, outcome);
                        crate::telemetry::observe_tool_latency_ms(
                            &ctx.agent_id,
                            &call.name,
                            duration_ms,
                        );
                        if let Some(hooks) = &self.hooks {
                            let ev = serde_json::json!({
                                "agent_id": ctx.agent_id,
                                "session_id": msg.session_id.to_string(),
                                "tool_name": call.name,
                                "duration_ms": duration_ms,
                                "result": result,
                                "error": tool_err,
                            });
                            let _ = hooks.fire("after_tool_call", ev).await;
                        }
                        messages.push(ChatMessage::tool_result(&call.id, &call.name, result));
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
            let writer = TranscriptWriter::new(transcripts_dir, &ctx.agent_id);
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
            let _ = hooks.fire("after_message", ev).await;
        }
        tracing::info!(
            agent_id = %ctx.agent_id,
            session_id = %msg.session_id,
            message_id = %msg.id,
            produced_reply = reply_text.is_some(),
            "agent turn finished"
        );
        Ok(reply_text)
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
        Ok(())
    }
    async fn on_message(&self, ctx: &AgentContext, msg: InboundMessage) -> anyhow::Result<()> {
        self.run_turn(ctx, msg, true).await?;
        Ok(())
    }
    async fn decide(&self, ctx: &AgentContext, msg: &InboundMessage) -> anyhow::Result<String> {
        let reply = self.run_turn(ctx, msg.clone(), false).await?;
        Ok(reply.unwrap_or_default())
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
    } else if kind_hint == "voice" || kind_hint == "audio"
        || mime_hint.map(|m| m.starts_with("audio/")).unwrap_or(false)
    {
        (
            "audio",
            mime_hint
                .map(str::to_string)
                .unwrap_or_else(|| guess_mime(&media.path, "audio/ogg")),
        )
    } else if kind_hint == "video" || kind_hint == "video_note" || kind_hint == "animation"
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
        data: agent_llm::AttachmentData::Path { path: media.path.clone() },
    };
    if let Err(e) = att.materialize() {
        tracing::warn!(path = %media.path, kind = att_kind, error = %e, "failed to materialize inbound media; skipping");
        return None;
    }
    Some(att)
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
