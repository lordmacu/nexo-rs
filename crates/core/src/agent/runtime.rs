use super::agent::Agent;
use super::behavior::AgentBehavior;
use super::context::AgentContext;
use super::effective::EffectiveBindingPolicy;
use crate::runtime_snapshot::RuntimeSnapshot;
use arc_swap::ArcSwap;
use super::peer_directory::PeerDirectory;
use super::routing::{route_topic, AgentMessage, AgentPayload, AgentRouter};
use super::sender_rate_limit::SenderRateLimiter;
use super::types::{InboundMedia, InboundMessage, RunTrigger};
use crate::heartbeat::{heartbeat_interval, heartbeat_topic, publish_heartbeat};
use crate::session::SessionManager;
use crate::telemetry::inc_messages_processed_total;
use nexo_broker::{AnyBroker, BrokerHandle};
use nexo_config::types::agents::InboundBinding;
use nexo_memory::LongTermMemory;
use dashmap::DashMap;
use serde_json::Value;
use std::mem;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinSet;
use tokio::time::{sleep_until, Instant};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use uuid::Uuid;
pub struct AgentRuntime {
    agent: Arc<Agent>,
    broker: AnyBroker,
    sessions: Arc<SessionManager>,
    memory: Option<Arc<LongTermMemory>>,
    peers: Option<Arc<PeerDirectory>>,
    router: Arc<AgentRouter>,
    // session_id → sender into that session's debounce task
    session_txs: Arc<DashMap<Uuid, mpsc::Sender<InboundMessage>>>,
    debounce_ms: Duration,
    queue_cap: usize,
    /// Per-binding sender rate limiters, keyed by
    /// `EffectiveBindingPolicy::binding_index`. Built lazily on first
    /// matching intake from the binding's effective `sender_rate_limit`;
    /// `None` in a slot means "this binding opted out of rate
    /// limiting". `None` as a key is the legacy bucket synthesised from
    /// agent-level defaults — its key space stays disjoint from real
    /// bindings (0..N).
    ///
    /// Rationale for per-binding (instead of one per agent): an agent
    /// that exposes a narrow sales surface on WhatsApp and a trusted
    /// owner-only surface on Telegram typically wants very different
    /// throttles, and keeping buckets segregated means flood on one
    /// channel cannot exhaust the quota on the other.
    sender_rate_limiters: Arc<DashMap<Option<usize>, Option<Arc<SenderRateLimiter>>>>,
    /// Pre-resolved per-binding capability policies. The `None` key is
    /// reserved for the legacy "no bindings" bucket synthesised from
    /// agent-level defaults. Policies are immutable for the lifetime
    /// of the runtime so we allocate each one exactly once at `new()`
    /// time — the hot intake path just clones an `Arc`.
    effective_policies: Arc<DashMap<Option<usize>, Arc<EffectiveBindingPolicy>>>,
    /// Phase 18 — hot-reloadable snapshot. Holds the same
    /// `effective_policies` + `tool_cache` data as the legacy fields
    /// above, plus the optional per-agent `LlmClient`. The intake
    /// hot path still reads the legacy fields in this commit; those
    /// reads migrate to `snapshot.load()` in a follow-up so the
    /// refactor stays atomic per step.
    snapshot: Arc<ArcSwap<RuntimeSnapshot>>,
    /// Base tool registry (plugins + MCP + extensions + skills). Used
    /// together with `tool_cache` to hand each session a filtered
    /// `Arc<ToolRegistry>` that only exposes the binding's allowed
    /// tools. `None` for runtimes spun up without tool wiring (tests,
    /// no-LLM behaviors). See [`AgentRuntime::with_tool_base`].
    tool_base: Option<Arc<super::tool_registry::ToolRegistry>>,
    /// Phase 17 — per-agent credential resolver attached to every
    /// AgentContext the runtime builds. `None` in tests / no-credential
    /// boot paths; consumers fall back to legacy topics in that case.
    credentials: Option<Arc<nexo_auth::AgentCredentialResolver>>,
    /// Phase 17 — per-(channel, instance) breaker registry; cloned
    /// onto every AgentContext alongside `credentials`.
    breakers: Option<Arc<nexo_auth::BreakerRegistry>>,
    /// Optional pre-persistence redactor cloned onto every
    /// AgentContext. `None` keeps transcripts un-redacted.
    redactor: Option<Arc<super::redaction::Redactor>>,
    /// Optional FTS5 transcripts index cloned onto every AgentContext.
    /// `None` keeps `session_logs` action `search` on the JSONL
    /// substring fallback.
    transcripts_index: Option<Arc<super::transcripts_index::TranscriptsIndex>>,
    /// Phase 21 — shared link extractor (HTTP client + LRU cache).
    /// `None` keeps link understanding off regardless of YAML.
    link_extractor: Option<Arc<crate::link_understanding::LinkExtractor>>,
    /// Phase 25 — shared web-search router (one per process, every
    /// runtime gets the same Arc). `None` disables the `web_search`
    /// tool regardless of YAML.
    web_search_router: Option<Arc<nexo_web_search::WebSearchRouter>>,
    /// Phase 26 — shared pairing gate. Consulted in the intake hot
    /// path before sender_rate_limit; when the resolved
    /// `EffectiveBindingPolicy::pairing.auto_challenge` is true and
    /// the sender is not in `pairing_allow_from`, the message is
    /// dropped and a code is logged for the operator to approve via
    /// `nexo pair approve`. `None` disables the gate regardless of
    /// YAML.
    pairing_gate: Option<Arc<nexo_pairing::PairingGate>>,
    /// Channel adapter registry consulted alongside `pairing_gate`. The
    /// registry maps `source_plugin` (`"whatsapp"`, `"telegram"`, …) to
    /// a `PairingChannelAdapter` so the gate can normalise sender ids
    /// and so challenge replies can be delivered through the channel-
    /// specific outbound path. `None` keeps the legacy zero-adapter
    /// path: senders pass through verbatim and challenges are published
    /// raw on `plugin.outbound.{channel}`.
    pairing_adapters: nexo_pairing::PairingAdapterRegistry,
    /// Legacy cache — still owned by the runtime for back-compat with
    /// any test construction path. Hot-reload reads the per-snapshot
    /// `tool_cache` instead; see [`RuntimeSnapshot::tool_cache`].
    tool_cache: Arc<super::tool_registry_cache::ToolRegistryCache>,
    /// Phase 18 — reload control channel. The coordinator sends
    /// `Apply(snapshot)` to atomically swap; the runtime reads the
    /// new snapshot from the next event onwards (apply-on-next).
    reload_tx: mpsc::Sender<ReloadCommand>,
    /// Receiver owned by the runtime until `start()` moves it into
    /// the select loop. `Option` because it can only be taken once.
    reload_rx: Arc<Mutex<Option<mpsc::Receiver<ReloadCommand>>>>,
    shutdown: CancellationToken,
    tasks: Arc<Mutex<JoinSet<()>>>,
}

/// Commands the reload coordinator sends to per-agent runtimes.
#[derive(Debug)]
pub enum ReloadCommand {
    /// Swap in a new snapshot. Picked up by the next event's
    /// `snapshot.load()` read — in-flight turns keep the old Arc.
    Apply(Arc<RuntimeSnapshot>),
}
impl AgentRuntime {
    pub fn new(agent: Arc<Agent>, broker: AnyBroker, sessions: Arc<SessionManager>) -> Self {
        let debounce_ms = Duration::from_millis(agent.config.config.debounce_ms);
        let queue_cap = agent.config.config.queue_cap;
        // Pre-resolve the per-binding effective policies so the intake
        // hot path doesn't allocate. The set is bounded by the number
        // of bindings (typically 1-3) plus the legacy sentinel slot
        // for agents that haven't adopted bindings yet.
        let effective_policies: DashMap<Option<usize>, Arc<EffectiveBindingPolicy>> =
            DashMap::new();
        if agent.config.inbound_bindings.is_empty() {
            effective_policies.insert(
                None,
                Arc::new(EffectiveBindingPolicy::from_agent_defaults(&agent.config)),
            );
        } else {
            for idx in 0..agent.config.inbound_bindings.len() {
                effective_policies
                    .insert(Some(idx), EffectiveBindingPolicy::resolved(&agent.config, idx));
            }
        }
        let initial_snapshot = RuntimeSnapshot::bare(Arc::clone(&agent.config), 0);
        let (reload_tx, reload_rx) = mpsc::channel(4);
        Self {
            agent,
            broker,
            sessions,
            memory: None,
            peers: None,
            router: Arc::new(AgentRouter::new()),
            session_txs: Arc::new(DashMap::new()),
            debounce_ms,
            queue_cap,
            sender_rate_limiters: Arc::new(DashMap::new()),
            effective_policies: Arc::new(effective_policies),
            snapshot: Arc::new(ArcSwap::from_pointee(initial_snapshot)),
            tool_base: None,
            credentials: None,
            breakers: None,
            redactor: None,
            transcripts_index: None,
            link_extractor: None,
            web_search_router: None,
            pairing_gate: None,
            pairing_adapters: nexo_pairing::PairingAdapterRegistry::new(),
            tool_cache: Arc::new(super::tool_registry_cache::ToolRegistryCache::new()),
            reload_tx,
            reload_rx: Arc::new(Mutex::new(Some(reload_rx))),
            shutdown: CancellationToken::new(),
            tasks: Arc::new(Mutex::new(JoinSet::new())),
        }
    }
    pub fn with_memory(mut self, memory: Arc<LongTermMemory>) -> Self {
        self.memory = Some(memory);
        self
    }
    pub fn with_redactor(mut self, redactor: Arc<super::redaction::Redactor>) -> Self {
        self.redactor = Some(redactor);
        self
    }
    pub fn with_transcripts_index(
        mut self,
        index: Arc<super::transcripts_index::TranscriptsIndex>,
    ) -> Self {
        self.transcripts_index = Some(index);
        self
    }
    /// Attach the shared link extractor. All `AgentContext`s built by
    /// this runtime inherit it so `llm_behavior` can fetch URLs in
    /// inbound messages and build the `# LINK CONTEXT` block.
    pub fn with_link_extractor(
        mut self,
        ext: Arc<crate::link_understanding::LinkExtractor>,
    ) -> Self {
        self.link_extractor = Some(ext);
        self
    }
    /// Attach the shared web-search router. Every `AgentContext` built
    /// by this runtime inherits it so the `web_search` tool can route.
    pub fn with_web_search_router(
        mut self,
        router: Arc<nexo_web_search::WebSearchRouter>,
    ) -> Self {
        self.web_search_router = Some(router);
        self
    }
    /// Attach the shared pairing gate. Consulted before the per-sender
    /// rate limiter in the intake hot path so unknown senders never
    /// reach the agent's behavior.
    pub fn with_pairing_gate(mut self, gate: Arc<nexo_pairing::PairingGate>) -> Self {
        self.pairing_gate = Some(gate);
        self
    }
    /// Attach the pairing channel-adapter registry. Adapters registered
    /// here are looked up by `source_plugin` on every inbound message
    /// when the gate is active, and used to normalise sender ids before
    /// store lookup. `None` (default) preserves legacy zero-adapter
    /// behaviour.
    pub fn with_pairing_adapters(
        mut self,
        registry: nexo_pairing::PairingAdapterRegistry,
    ) -> Self {
        self.pairing_adapters = registry;
        self
    }
    pub fn with_peers(mut self, peers: Arc<PeerDirectory>) -> Self {
        self.peers = Some(peers);
        self
    }
    /// Attach the base tool registry used by this agent so the runtime
    /// can hand each session a per-binding filtered view via its
    /// internal cache. Without this, sessions fall back to the
    /// behavior's own registry and pay a per-turn filter cost.
    pub fn with_tool_base(mut self, tools: Arc<super::tool_registry::ToolRegistry>) -> Self {
        self.tool_base = Some(tools);
        self
    }
    /// Expose the runtime's `ArcSwap<RuntimeSnapshot>` so the reload
    /// coordinator can swap a freshly-built snapshot in atomically
    /// without tearing down the runtime. Cheap `Arc` clone — callers
    /// typically stash the handle once at boot.
    pub fn snapshot_handle(&self) -> Arc<ArcSwap<RuntimeSnapshot>> {
        Arc::clone(&self.snapshot)
    }
    /// Atomic swap of the per-agent snapshot. Readers that already
    /// hold an `Arc<RuntimeSnapshot>` (session tasks mid-turn) keep
    /// their copy for the lifetime of that Arc; subsequent
    /// `snapshot.load()` calls see the new value.
    pub fn swap_snapshot(&self, new: Arc<RuntimeSnapshot>) {
        self.snapshot.store(new);
    }
    /// Clone the `ReloadCommand` sender so the coordinator can push
    /// `Apply` commands. One sender per agent runtime; the receiver is
    /// drained inside `start()`.
    pub fn reload_sender(&self) -> mpsc::Sender<ReloadCommand> {
        self.reload_tx.clone()
    }
    /// Attach the credential resolver. All `AgentContext`s built by
    /// this runtime inherit it so outbound tools can look up the
    /// agent's bound instance instead of publishing to the legacy
    /// single-account topic.
    pub fn with_credentials(
        mut self,
        credentials: Arc<nexo_auth::AgentCredentialResolver>,
    ) -> Self {
        self.credentials = Some(credentials);
        self
    }
    pub fn with_breakers(mut self, breakers: Arc<nexo_auth::BreakerRegistry>) -> Self {
        self.breakers = Some(breakers);
        self
    }
    pub fn router(&self) -> Arc<AgentRouter> {
        Arc::clone(&self.router)
    }
    pub async fn start(&self) -> anyhow::Result<()> {
        let plugin_topic = "plugin.inbound.>";
        let mut plugin_sub = self.broker.subscribe(plugin_topic).await?;
        // Phase 18 — take the reload receiver exactly once. Subsequent
        // start() calls on the same runtime would starve reload; the
        // None branch logs a warn instead of panicking to keep test
        // code that re-starts runtimes honest.
        let reload_rx = self.reload_rx.lock().await.take();
        if reload_rx.is_none() {
            tracing::warn!(
                agent_id = %self.agent.id,
                "reload receiver already taken — hot-reload disabled for this runtime start"
            );
        }
        let mut reload_rx = reload_rx;
        let snapshot = Arc::clone(&self.snapshot);
        let heartbeat_topic = heartbeat_topic(&self.agent.id);
        let mut heartbeat_sub = self.broker.subscribe(&heartbeat_topic).await?;
        let route_inbound_topic = route_topic(&self.agent.id);
        let mut route_sub = self.broker.subscribe(&route_inbound_topic).await?;
        let agent = Arc::clone(&self.agent);
        let sessions = Arc::clone(&self.sessions);
        let broker = self.broker.clone();
        let memory = self.memory.clone();
        let peers = self.peers.clone();
        let credentials = self.credentials.clone();
        let breakers = self.breakers.clone();
        let redactor = self.redactor.clone();
        let transcripts_index = self.transcripts_index.clone();
        let link_extractor = self.link_extractor.clone();
        let web_search_router = self.web_search_router.clone();
        let pairing_gate = self.pairing_gate.clone();
        let pairing_adapters = self.pairing_adapters.clone();
        let router = Arc::clone(&self.router);
        let session_txs = Arc::clone(&self.session_txs);
        let debounce_ms = self.debounce_ms;
        let queue_cap = self.queue_cap;
        let sender_rate_limiters = Arc::clone(&self.sender_rate_limiters);
        let effective_policies = Arc::clone(&self.effective_policies);
        // Phase 18 — every event reads the current snapshot so hot-
        // reload takes effect immediately on the next message without
        // touching the legacy per-runtime caches (kept around during
        // the migration so tests that construct runtimes without a
        // coordinator still work).
        let snapshot_ref = Arc::clone(&self.snapshot);
        let tool_base = self.tool_base.clone();
        let _tool_cache = Arc::clone(&self.tool_cache);
        let shutdown = self.shutdown.clone();
        let tasks = Arc::clone(&self.tasks);
        let shutdown2 = shutdown.clone();
        self.tasks.lock().await.spawn(async move {
            let mut ctx = AgentContext::new(
                agent.id.clone(),
                Arc::clone(&agent.config),
                broker.clone(),
                Arc::clone(&sessions),
            );
            if let Some(ref mem) = memory {
                ctx = ctx.with_memory(Arc::clone(mem));
            }
            if let Some(ref p) = peers {
                ctx = ctx.with_peers(Arc::clone(p));
            }
            if let Some(ref c) = credentials {
                ctx = ctx.with_credentials(Arc::clone(c));
            }
            if let Some(ref b) = breakers {
                ctx = ctx.with_breakers(Arc::clone(b));
            }
            if let Some(ref r) = redactor {
                ctx = ctx.with_redactor(Arc::clone(r));
            }
            if let Some(ref ext) = link_extractor {
                ctx = ctx.with_link_extractor(Arc::clone(ext));
            }
            if let Some(ref ws) = web_search_router {
                ctx = ctx.with_web_search_router(Arc::clone(ws));
            }
            if let Some(ref idx) = transcripts_index {
                ctx = ctx.with_transcripts_index(Arc::clone(idx));
            }
            ctx = ctx.with_router(Arc::clone(&router));
            ctx = ctx.with_context_optimization(snapshot.load().context_optimization);
            loop {
                tokio::select! {
                    biased;
                    // Phase 18 — reload command drains first so a
                    // burst of inbound events can't starve a pending
                    // config swap. `biased` keeps arm ordering stable.
                    cmd = async {
                        match reload_rx.as_mut() {
                            Some(rx) => rx.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        match cmd {
                            Some(ReloadCommand::Apply(new_snap)) => {
                                let version = new_snap.version;
                                snapshot.store(new_snap);
                                crate::telemetry::set_runtime_config_version(&agent.id, version);
                                // The aggregate counter is bumped
                                // once per reload by the coordinator;
                                // the per-agent gauge above is what
                                // dashboards correlate with sessions.
                                tracing::info!(
                                    agent_id = %agent.id,
                                    version,
                                    "config reload: snapshot applied",
                                );
                            }
                            None => {
                                tracing::debug!(agent_id = %agent.id, "reload channel closed");
                                // Channel closed just means the
                                // coordinator went away; keep serving
                                // with the current snapshot.
                                reload_rx = None;
                            }
                        }
                    }
                    event = plugin_sub.next() => {
                        let Some(event) = event else { break };
                        let session_id = event.session_id.unwrap_or_else(Uuid::new_v4);
                        let text = event.payload
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let (source_plugin, source_instance) =
                            parse_inbound_topic(&event.topic);
                        // Binding filter — empty list = legacy wildcard
                        // (accept all, matches pre-binding behavior).
                        // Populated list = strict allowlist; we also
                        // capture the matched binding index so the
                        // session task can pick up its per-binding
                        // capability overrides (tools, outbound allowlist,
                        // skills, model, prompt, rate limit, delegates).
                        // Load once per event so an in-flight reload
                        // (ReloadCommand::Apply racing against the
                        // event) can't give us a partial view: we
                        // either see the old snapshot fully or the
                        // new one fully. Matches the apply-on-next
                        // semantic — a reload that swaps while an
                        // event is being *parsed* still gets applied
                        // on the NEXT event because biased select
                        // drains reload first.
                        let snap = snapshot_ref.load_full();
                        let bindings = &snap.nexo_config.inbound_bindings;
                        let effective = if bindings.is_empty() {
                            snap.policy_for(None)
                                .or_else(|| effective_policies.get(&None).map(|e| Arc::clone(e.value())))
                                .expect("legacy effective policy is seeded at runtime::new")
                        } else {
                            match match_binding_index(
                                bindings,
                                &source_plugin,
                                source_instance.as_deref(),
                            ) {
                                Some(idx) => {
                                    tracing::trace!(
                                        agent_id = %agent.id,
                                        plugin = %source_plugin,
                                        instance = source_instance.as_deref().unwrap_or("-"),
                                        binding_index = idx,
                                        snapshot_version = snap.version,
                                        "inbound matched binding",
                                    );
                                    snap.policy_for(Some(idx))
                                        .or_else(|| effective_policies.get(&Some(idx)).map(|e| Arc::clone(e.value())))
                                        .expect("per-binding effective policy is seeded at runtime::new")
                                }
                                None => {
                                    tracing::trace!(
                                        agent_id = %agent.id,
                                        plugin = %source_plugin,
                                        instance = source_instance.as_deref().unwrap_or("-"),
                                        "inbound dropped by binding filter",
                                    );
                                    continue;
                                }
                            }
                        };
                        let sender_id = event.payload
                            .get("from")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        // Phase 26 — pairing gate. Runs before the
                        // rate limiter so unknown senders cannot
                        // exhaust their bucket. Only active when the
                        // binding's effective `pairing.auto_challenge`
                        // is true; otherwise the gate fast-paths to
                        // Admit at zero overhead. The challenge code
                        // is logged (operator approves via `nexo pair
                        // approve`); a future pass will publish it
                        // back through the channel adapter so the
                        // sender sees it in their chat.
                        if effective.pairing.auto_challenge {
                            if let (Some(gate), Some(sender)) =
                                (pairing_gate.as_ref(), sender_id.as_deref())
                            {
                                let channel = source_plugin.as_str();
                                let account = source_instance.as_deref().unwrap_or("default");
                                let adapter = pairing_adapters.get(channel);
                                match gate
                                    .should_admit(
                                        channel,
                                        account,
                                        sender,
                                        &effective.pairing,
                                        adapter
                                            .as_deref()
                                            .map(|a| a as &dyn nexo_pairing::PairingChannelAdapter),
                                    )
                                    .await
                                {
                                    Ok(nexo_pairing::Decision::Admit) => {}
                                    Ok(nexo_pairing::Decision::Challenge { code }) => {
                                        tracing::warn!(
                                            agent_id = %agent.id,
                                            channel,
                                            account,
                                            sender,
                                            code = %code,
                                            "pairing challenge issued — run `nexo pair approve {}` to admit",
                                            code,
                                        );
                                        deliver_pairing_challenge(
                                            &broker,
                                            adapter.as_deref(),
                                            channel,
                                            source_instance.as_deref(),
                                            account,
                                            sender,
                                            &code,
                                        )
                                        .await;
                                        continue;
                                    }
                                    Ok(nexo_pairing::Decision::Drop) => {
                                        tracing::trace!(
                                            agent_id = %agent.id,
                                            channel,
                                            account,
                                            sender,
                                            "pairing gate dropped (max-pending exhausted)",
                                        );
                                        continue;
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            agent_id = %agent.id,
                                            error = %e,
                                            "pairing gate storage error — admitting fail-open",
                                        );
                                    }
                                }
                            }
                        }
                        // Per-sender rate limit — applied after the
                        // binding filter so we don't waste bucket
                        // tokens on events the agent would drop anyway.
                        // A denied event is silently dropped (trace-
                        // logged) so the sender doesn't get a "rate
                        // limited" reply they could use to probe the
                        // bot. Limiter is per-binding, built lazily
                        // from the effective `sender_rate_limit`.
                        let limiter_slot = sender_rate_limiters
                            .entry(effective.binding_index)
                            .or_insert_with(|| {
                                effective
                                    .sender_rate_limit
                                    .clone()
                                    .map(|cfg| Arc::new(SenderRateLimiter::new(cfg)))
                            })
                            .value()
                            .clone();
                        if let Some(rl) = limiter_slot {
                            if !rl.try_acquire(&agent.id, sender_id.as_deref()).await {
                                tracing::trace!(
                                    agent_id = %agent.id,
                                    plugin = %source_plugin,
                                    sender = sender_id.as_deref().unwrap_or("-"),
                                    binding_index = ?effective.binding_index,
                                    "inbound dropped by sender rate limit",
                                );
                                continue;
                            }
                        }
                        let media = extract_inbound_media(&event.payload);
                        // Drop events with no text and no media — e.g. reactions,
                        // receipts, typing, poll votes reach us as empty-text
                        // InboundEvent::Message. Without this gate the LLM gets
                        // invoked on empty input and produces spontaneous "¿en
                        // qué ayudo?" replies (see startup spam bug).
                        if text.is_empty() && media.is_none() {
                            tracing::trace!(
                                agent_id = %agent.id,
                                plugin = %source_plugin,
                                "inbound dropped: no text and no media",
                            );
                            continue;
                        }
                        let mut msg = InboundMessage::new(session_id, &agent.id, text);
                        msg.source_plugin = source_plugin;
                        msg.source_instance = source_instance;
                        msg.sender_id = sender_id;
                        msg.media = media;
                        let message_id = msg.id;
                        // Atomic get-or-insert: DashMap::entry::or_insert_with
                        // guarantees only one task is spawned per session even
                        // when two threads race the first message for a new
                        // session_id. The spawned task also receives the
                        // session_txs handle so it can remove its own entry
                        // on exit — otherwise the map grows without bound as
                        // sessions come and go (one per chat, forever).
                        // Atomic get-or-insert: DashMap::entry::or_insert_with
                        // guarantees only one task is spawned per session even
                        // when two threads race the first message for a new
                        // session_id. The spawned task receives its own tx
                        // handle so it can remove exactly its own entry from
                        // the map on exit (the `same_channel` check avoids a
                        // race where a newer session replaced us).
                        let effective_for_session = Arc::clone(&effective);
                        // Pre-filtered tool registry for this binding.
                        // Pulls the cache from the active snapshot so a
                        // reload that changed allowed_tools produces a
                        // fresh filtered clone (old snapshot's cache
                        // stays with its in-flight sessions). `None`
                        // base registry (tests) → llm_behavior falls
                        // back to its own tool set.
                        let effective_tools_for_session = tool_base.as_ref().map(|base| {
                            snap.tool_cache.get_or_build(
                                &agent.id,
                                effective_for_session.binding_index,
                                base,
                                &effective_for_session.allowed_tools,
                            )
                        });
                        let entry = session_txs.entry(session_id).or_insert_with(|| {
                            let (tx, rx) = mpsc::channel(queue_cap);
                            let tx_for_task = tx.clone();
                            let mut ctx = AgentContext::new(
                                agent.id.clone(),
                                Arc::clone(&agent.config),
                                broker.clone(),
                                Arc::clone(&sessions),
                            );
                            ctx = ctx.with_effective(Arc::clone(&effective_for_session));
                            ctx = ctx.with_context_optimization(snap.context_optimization);
                            if let Some(tools) = effective_tools_for_session.clone() {
                                ctx = ctx.with_effective_tools(tools);
                            }
                            if let Some(ref mem) = memory {
                                ctx = ctx.with_memory(Arc::clone(mem));
                            }
                            if let Some(ref p) = peers {
                                ctx = ctx.with_peers(Arc::clone(p));
                            }
                            if let Some(ref c) = credentials {
                                ctx = ctx.with_credentials(Arc::clone(c));
                            }
                            if let Some(ref r) = redactor {
                                ctx = ctx.with_redactor(Arc::clone(r));
                            }
                            if let Some(ref idx) = transcripts_index {
                                ctx = ctx.with_transcripts_index(Arc::clone(idx));
                            }
                            if let Some(ref ext) = link_extractor {
                                ctx = ctx.with_link_extractor(Arc::clone(ext));
                            }
                            if let Some(ref ws) = web_search_router {
                                ctx = ctx.with_web_search_router(Arc::clone(ws));
                            }
                            let behavior = Arc::clone(&agent.behavior);
                            let cancel = shutdown.clone();
                            let session_txs_for_task = Arc::clone(&session_txs);
                            let tasks_for_spawn = Arc::clone(&tasks);
                            // Spawn without holding the tasks lock across
                            // `await` to avoid deadlock with `stop()`.
                            // Also short-circuit if shutdown has already
                            // fired: `stop()` may have taken the lock and
                            // started draining before this outer spawn
                            // got scheduled, in which case a late
                            // register would leak a joined-off task.
                            let cancel_for_outer = shutdown.clone();
                            tokio::spawn(async move {
                                if cancel_for_outer.is_cancelled() {
                                    return;
                                }
                                let mut tasks_guard = tasks_for_spawn.lock().await;
                                if cancel_for_outer.is_cancelled() {
                                    return;
                                }
                                let _jh = tasks_guard.spawn(
                                    session_debounce_task(
                                        rx,
                                        behavior,
                                        ctx,
                                        debounce_ms,
                                        cancel,
                                        session_id,
                                        session_txs_for_task,
                                        tx_for_task,
                                    ),
                                );
                            });
                            tx
                        });
                        let tx = entry.value().clone();
                        drop(entry);
                        if let Err(e) = tx.try_send(msg) {
                            tracing::warn!(
                                agent_id = %agent.id,
                                session_id = %session_id,
                                message_id = %message_id,
                                error = %e,
                                "session queue full — message dropped"
                            );
                        }
                    }
                    event = heartbeat_sub.next() => {
                        let Some(event) = event else { break };
                        tracing::debug!(
                            agent_id = %agent.id,
                            event_id = %event.id,
                            "heartbeat tick received"
                        );
                        ctx = ctx.with_context_optimization(snapshot_ref.load().context_optimization);
                        if let Err(e) = agent.behavior.on_heartbeat(&ctx).await {
                            tracing::error!(agent_id = %agent.id, error = %e, "on_heartbeat failed");
                        }
                    }
                    event = route_sub.next() => {
                        let Some(event) = event else { break };
                        let msg: AgentMessage = match serde_json::from_value(event.payload.clone()) {
                            Ok(m) => m,
                            Err(e) => {
                                tracing::warn!(agent_id = %agent.id, error = %e, "invalid route payload");
                                continue;
                            }
                        };
                        if msg.to != agent.id {
                            continue;
                        }
                        match msg.payload {
                            AgentPayload::Delegate { task, context } => {
                                // Receiver-side authorization: enforces
                                // `accept_delegates_from` so a
                                // compromised peer can't bypass the
                                // caller's `allowed_delegates` gate by
                                // publishing directly to the broker.
                                let acl = &agent.config.accept_delegates_from;
                                if !acl.is_empty()
                                    && !acl.iter().any(|p| match p.strip_suffix('*') {
                                        Some(stem) => msg.from.starts_with(stem),
                                        None => p == &msg.from,
                                    })
                                {
                                    tracing::warn!(
                                        agent_id = %agent.id,
                                        from = %msg.from,
                                        correlation_id = %msg.correlation_id,
                                        "delegate rejected: sender not in accept_delegates_from"
                                    );
                                    let response = AgentMessage {
                                        from: agent.id.clone(),
                                        to: msg.from.clone(),
                                        correlation_id: msg.correlation_id,
                                        payload: AgentPayload::Result {
                                            task_id: msg.correlation_id,
                                            output: serde_json::json!({
                                                "error": "delegate rejected by receiver ACL",
                                            }),
                                        },
                                    };
                                    let topic = route_topic(&msg.from);
                                    if let Ok(payload) = serde_json::to_value(response) {
                                        let evt = nexo_broker::Event::new(
                                            &topic,
                                            &agent.id,
                                            payload,
                                        );
                                        let _ = broker.publish(&topic, evt).await;
                                    }
                                    continue;
                                }
                                let session_id = parse_session_id_from_context(&context).unwrap_or_else(Uuid::new_v4);
                                let mut inbound = InboundMessage::new(session_id, &agent.id, task);
                                inbound.trigger = RunTrigger::Manual;
                                inbound.source_plugin = "agent".to_string();
                                inbound.sender_id = Some(msg.from.clone());
                                tracing::info!(
                                    agent_id = %agent.id,
                                    from = %msg.from,
                                    to = %msg.to,
                                    correlation_id = %msg.correlation_id,
                                    session_id = %session_id,
                                    message_id = %inbound.id,
                                    "route delegate received"
                                );
                                let output = match agent.behavior.decide(&ctx, &inbound).await {
                                    Ok(text) => serde_json::json!({ "text": text }),
                                    Err(e) => serde_json::json!({ "error": e.to_string() }),
                                };
                                let response = AgentMessage {
                                    from: agent.id.clone(),
                                    to: msg.from.clone(),
                                    correlation_id: msg.correlation_id,
                                    payload: AgentPayload::Result {
                                        task_id: msg.correlation_id,
                                        output,
                                    },
                                };
                                let topic = route_topic(&msg.from);
                                let payload = match serde_json::to_value(response) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        tracing::error!(agent_id = %agent.id, error = %e, "failed to serialize route result");
                                        continue;
                                    }
                                };
                                let evt = nexo_broker::Event::new(&topic, &agent.id, payload);
                                if let Err(e) = broker.publish(&topic, evt).await {
                                    tracing::error!(agent_id = %agent.id, error = %e, "failed to publish route result");
                                } else {
                                    tracing::info!(
                                        agent_id = %agent.id,
                                        to = %msg.from,
                                        correlation_id = %msg.correlation_id,
                                        "route result published"
                                    );
                                }
                            }
                            AgentPayload::Result { output, .. } => {
                                if let Some(router) = ctx.router.as_ref() {
                                    let resumed = router.resolve(msg.correlation_id, output);
                                    if !resumed {
                                        tracing::debug!(
                                            agent_id = %agent.id,
                                            correlation_id = %msg.correlation_id,
                                            "route result had no pending waiter"
                                        );
                                    } else {
                                        tracing::info!(
                                            agent_id = %agent.id,
                                            from = %msg.from,
                                            correlation_id = %msg.correlation_id,
                                            "route result matched pending waiter"
                                        );
                                    }
                                }
                            }
                            AgentPayload::Broadcast { event, data } => {
                                let evt = nexo_broker::Event::new(
                                    format!("agent.broadcast.{event}"),
                                    &msg.from,
                                    data,
                                );
                                if let Err(e) = agent.behavior.on_event(&ctx, evt).await {
                                    tracing::error!(agent_id = %agent.id, error = %e, "on_event failed for route broadcast");
                                }
                            }
                        }
                    }
                    _ = shutdown2.cancelled() => break,
                }
            }
        });
        if let Some(interval) = heartbeat_interval(&self.agent.config)? {
            let broker = self.broker.clone();
            let agent_id = self.agent.id.clone();
            let shutdown = self.shutdown.clone();
            self.tasks.lock().await.spawn(async move {
                // Delay first tick by `interval` so the agent doesn't fire
                // on_heartbeat immediately on boot (which causes proactive
                // messages / reminders to spam on startup).
                let mut ticker = tokio::time::interval_at(
                    tokio::time::Instant::now() + interval,
                    interval,
                );
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = ticker.tick() => {
                            if let Err(e) = publish_heartbeat(&broker, &agent_id).await {
                                tracing::error!(agent_id = %agent_id, error = %e, "failed to publish heartbeat");
                            }
                        }
                    }
                }
            });
        }
        Ok(())
    }
    pub async fn stop(&self) {
        // Stop intake/tickers first, then close per-session queues so workers
        // can flush pending buffered messages and exit gracefully.
        self.shutdown.cancel();
        self.session_txs.clear();
        let mut tasks = self.tasks.lock().await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            tokio::select! {
                result = tasks.join_next() => {
                    if result.is_none() { break; }
                }
                _ = sleep_until(deadline) => {
                    tasks.abort_all();
                    break;
                }
            }
        }
    }
}
/// Per-session idle TTL: after this long with no incoming message, the
/// debounce task exits and is removed from `session_txs`. Prevents the
/// per-agent map from growing unbounded when traffic churns through
/// many short-lived sessions (every chat gets its own session_id).
const SESSION_IDLE_TTL: Duration = Duration::from_secs(600);
#[allow(clippy::too_many_arguments)]
async fn session_debounce_task(
    mut rx: mpsc::Receiver<InboundMessage>,
    behavior: Arc<dyn AgentBehavior>,
    ctx: AgentContext,
    debounce_ms: Duration,
    shutdown: CancellationToken,
    session_id: Uuid,
    session_txs: Arc<DashMap<Uuid, mpsc::Sender<InboundMessage>>>,
    my_tx: mpsc::Sender<InboundMessage>,
) {
    let mut buffer: Vec<InboundMessage> = Vec::new();
    let mut deadline: Option<Instant> = None;
    // Rolling idle deadline: reset on every recv, fire when reached.
    let mut idle_deadline = Instant::now() + SESSION_IDLE_TTL;
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                // Drain what is already queued and flush before stopping.
                while let Ok(m) = rx.try_recv() {
                    buffer.push(m);
                }
                if !buffer.is_empty() {
                    flush(&behavior, &ctx, mem::take(&mut buffer)).await;
                }
                break;
            },
            msg = rx.recv() => {
                match msg {
                    Some(m) => {
                        buffer.push(m);
                        idle_deadline = Instant::now() + SESSION_IDLE_TTL;
                        if debounce_ms.is_zero() {
                            // flush immediately — no timer needed
                            flush(&behavior, &ctx, mem::take(&mut buffer)).await;
                            deadline = None;
                        } else {
                            deadline = Some(Instant::now() + debounce_ms);
                        }
                    }
                    None => {
                        // sender dropped — flush remaining
                        if !buffer.is_empty() {
                            flush(&behavior, &ctx, mem::take(&mut buffer)).await;
                        }
                        break;
                    }
                }
            }
            _ = async {
                match deadline {
                    Some(d) => sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => {
                let items = mem::take(&mut buffer);
                deadline = None;
                flush(&behavior, &ctx, items).await;
            }
            _ = sleep_until(idle_deadline) => {
                // No activity for `SESSION_IDLE_TTL`. Exit so the
                // task doesn't linger indefinitely. The session_txs
                // cleanup below removes our entry; a future message
                // on this session respawns a fresh task.
                tracing::debug!(
                    %session_id,
                    ttl_secs = SESSION_IDLE_TTL.as_secs(),
                    "session debounce task idle — exiting"
                );
                break;
            }
        }
    }
    // Cleanup: remove our entry so the DashMap doesn't accumulate dead
    // sessions. Use `remove_if` with `same_channel` to avoid the race
    // where a fresh message raced in after we decided to exit — in
    // that case `or_insert_with` already replaced us, and we must not
    // evict the newcomer's sender.
    session_txs.remove_if(&session_id, |_, current_tx| current_tx.same_channel(&my_tx));
}
async fn flush(behavior: &Arc<dyn AgentBehavior>, ctx: &AgentContext, items: Vec<InboundMessage>) {
    for msg in items {
        inc_messages_processed_total(&ctx.agent_id);
        let span = tracing::info_span!(
            "agent.message",
            agent_id = %ctx.agent_id,
            session_id = %msg.session_id,
            message_id = %msg.id,
            trigger = ?msg.trigger,
            source_plugin = %msg.source_plugin
        );
        // Capture a snapshot of the message before we move it so that
        // a handler panic / error path can DLQ it without losing data.
        let dlq_payload = serde_json::json!({
            "agent_id": ctx.agent_id,
            "session_id": msg.session_id,
            "message_id": msg.id,
            "text": msg.text,
            "source_plugin": msg.source_plugin,
            "source_instance": msg.source_instance,
            "sender_id": msg.sender_id,
        });
        if let Err(e) = behavior.on_message(ctx, msg).instrument(span).await {
            tracing::error!(
                agent_id = %ctx.agent_id,
                error = %e,
                "on_message failed — publishing to DLQ topic for ops review"
            );
            // Best-effort DLQ: publish to a well-known topic so ops
            // can attach alerting / retry tooling. Never blocks the
            // loop — a broker hiccup here is logged and we move on.
            let dlq_topic = format!("agent.dlq.{}", ctx.agent_id);
            let mut ev = nexo_broker::Event::new(
                &dlq_topic,
                &ctx.agent_id,
                serde_json::json!({
                    "error": e.to_string(),
                    "message": dlq_payload,
                }),
            );
            ev.session_id = dlq_payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok());
            if let Err(pe) = ctx.broker.publish(&dlq_topic, ev).await {
                tracing::warn!(
                    agent_id = %ctx.agent_id,
                    error = %pe,
                    "DLQ publish failed — message unrecoverable"
                );
            }
        }
    }
}
fn parse_session_id_from_context(context: &Value) -> Option<Uuid> {
    context
        .get("session_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}
/// Pull a media reference from an inbound plugin payload. Plugins flatten
/// `media_kind` + `media_path` at the top level (see telegram's
/// `InboundEvent::to_payload`) so this helper is wire-format agnostic.
fn extract_inbound_media(payload: &Value) -> Option<InboundMedia> {
    let kind = payload
        .get("media_kind")
        .and_then(|v| v.as_str())?
        .to_string();
    let path = payload
        .get("media_path")
        .and_then(|v| v.as_str())?
        .to_string();
    let mime_type = payload
        .pointer("/media/mime_type")
        .or_else(|| payload.pointer("/media/0/mime_type"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(InboundMedia {
        kind,
        path,
        mime_type,
    })
}
/// Split `plugin.inbound.<plugin>[.<instance>]` into its parts.
/// Returns `("", None)` if the topic doesn't have the expected prefix
/// — caller's binding check treats that as "unknown source", which
/// only passes the filter when bindings are empty.
fn parse_inbound_topic(topic: &str) -> (String, Option<String>) {
    let Some(rest) = topic.strip_prefix("plugin.inbound.") else {
        return (String::new(), None);
    };
    match rest.split_once('.') {
        Some((plugin, instance)) if !instance.is_empty() => {
            (plugin.to_string(), Some(instance.to_string()))
        }
        _ => (rest.to_string(), None),
    }
}
/// Find the first binding index that matches `(plugin, instance)`. A
/// binding with `instance=None` matches any instance of its plugin —
/// including events with no instance at all. Used by the runtime
/// inbound-subscriber loop to both accept/reject events and select
/// which binding's overrides govern the session.
///
/// Returns `None` when no binding matches. Note: when an agent has no
/// bindings at all the caller interprets that as the legacy wildcard
/// ("accept every inbound"); this helper only speaks to the populated
/// case.
fn match_binding_index(
    bindings: &[InboundBinding],
    plugin: &str,
    instance: Option<&str>,
) -> Option<usize> {
    bindings.iter().position(|b| {
        if b.plugin != plugin {
            return false;
        }
        match (&b.instance, instance) {
            (None, _) => true,
            (Some(want), Some(got)) => want == got,
            (Some(_), None) => false,
        }
    })
}

/// Back-compat boolean wrapper around [`match_binding_index`]. Kept for
/// the unit tests that assert the accept/reject semantics; production
/// callers use `match_binding_index` so the index can be fed into
/// `EffectiveBindingPolicy::resolve`.
#[cfg(test)]
fn binding_matches(bindings: &[InboundBinding], plugin: &str, instance: Option<&str>) -> bool {
    match_binding_index(bindings, plugin, instance).is_some()
}

/// Phase 26.x — deliver the pairing challenge to the sender. When a
/// channel adapter is registered we use it for both sender-id
/// normalisation and channel-correct formatting (e.g. Telegram
/// MarkdownV2). For unregistered channels we fall back to the legacy
/// hardcoded broker publish so the operator still gets a log line and
/// the challenge text on `plugin.outbound.{whatsapp,telegram}`.
async fn deliver_pairing_challenge(
    broker: &AnyBroker,
    adapter: Option<&dyn nexo_pairing::PairingChannelAdapter>,
    channel: &str,
    instance: Option<&str>,
    account: &str,
    sender: &str,
    code: &str,
) {
    if let Some(adapter) = adapter {
        let to = adapter
            .normalize_sender(sender)
            .unwrap_or_else(|| sender.to_string());
        let text = adapter.format_challenge_text(code);
        match adapter.send_reply(account, &to, &text).await {
            Ok(()) => crate::telemetry::inc_pairing_inbound_challenged(
                channel,
                "delivered_via_adapter",
            ),
            Err(e) => {
                tracing::warn!(error = %e, %channel, "pairing adapter send_reply failed");
                crate::telemetry::inc_pairing_inbound_challenged(channel, "publish_failed");
            }
        }
        return;
    }

    // Fallback: legacy hardcoded broker publish for channels with no
    // registered adapter. Mirrors the pre-26.x payload shape so any
    // existing dispatcher still recognises the message.
    let topic_base = match channel {
        "whatsapp" => "plugin.outbound.whatsapp",
        "telegram" => "plugin.outbound.telegram",
        _ => {
            crate::telemetry::inc_pairing_inbound_challenged(
                channel,
                "no_adapter_no_broker_topic",
            );
            return;
        }
    };
    let topic = match instance {
        Some(inst) if !inst.is_empty() => format!("{topic_base}.{inst}"),
        _ => topic_base.to_string(),
    };
    let text = format!(
        "🔐 Pairing required.\nAsk the operator to run:\n  nexo pair approve {code}",
    );
    let payload = serde_json::json!({
        "kind": "text",
        "to": sender,
        "text": text,
    });
    let evt = nexo_broker::Event::new(&topic, "core.pairing", payload);
    match broker.publish(&topic, evt).await {
        Ok(_) => {
            crate::telemetry::inc_pairing_inbound_challenged(channel, "delivered_via_broker");
        }
        Err(e) => {
            tracing::warn!(error = %e, %topic, "pairing challenge outbound publish failed");
            crate::telemetry::inc_pairing_inbound_challenged(channel, "publish_failed");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_topic_extracts_plugin_and_optional_instance() {
        assert_eq!(
            parse_inbound_topic("plugin.inbound.telegram"),
            ("telegram".into(), None)
        );
        assert_eq!(
            parse_inbound_topic("plugin.inbound.telegram.sales"),
            ("telegram".into(), Some("sales".into()))
        );
        // Nested instances collapse — everything after the 2nd dot is
        // treated as the instance name so bot_names can contain `.`.
        assert_eq!(
            parse_inbound_topic("plugin.inbound.telegram.bot.v2"),
            ("telegram".into(), Some("bot.v2".into()))
        );
        // Non-inbound topics → neutral sentinel; binding filter rejects
        // them unless bindings are empty.
        assert_eq!(parse_inbound_topic("something.else"), (String::new(), None));
        assert_eq!(
            parse_inbound_topic("plugin.inbound."),
            (String::new(), None)
        );
    }
    #[test]
    fn match_binding_index_returns_first_winner_for_overlapping_rules() {
        // Two bindings overlap: the wildcard `(telegram, None)` matches
        // every telegram event, but there's also a specific
        // `(telegram, Some("sales"))` at a higher index. The runtime
        // must return the FIRST match in declaration order — callers
        // that want the specific binding to win should list it before
        // the wildcard. Locking down the rule here so a future refactor
        // can't silently reorder.
        let bindings = vec![
            InboundBinding {
                plugin: "telegram".into(),
                instance: None,
                ..Default::default()
            },
            InboundBinding {
                plugin: "telegram".into(),
                instance: Some("sales".into()),
                ..Default::default()
            },
        ];
        assert_eq!(
            match_binding_index(&bindings, "telegram", Some("sales")),
            Some(0),
            "first-match semantics: wildcard at index 0 wins over specific at index 1"
        );
        // Reversed order: specific wins.
        let bindings = vec![
            InboundBinding {
                plugin: "telegram".into(),
                instance: Some("sales".into()),
                ..Default::default()
            },
            InboundBinding {
                plugin: "telegram".into(),
                instance: None,
                ..Default::default()
            },
        ];
        assert_eq!(
            match_binding_index(&bindings, "telegram", Some("sales")),
            Some(0)
        );
        // No match → None.
        assert_eq!(match_binding_index(&bindings, "whatsapp", None), None);
    }

    #[test]
    fn binding_matches_covers_plugin_wide_and_exact_instance() {
        let all_telegram = vec![InboundBinding {
            plugin: "telegram".into(),
            instance: None,
            ..Default::default()
        }];
        assert!(binding_matches(&all_telegram, "telegram", None));
        assert!(binding_matches(&all_telegram, "telegram", Some("anyone")));
        assert!(!binding_matches(&all_telegram, "whatsapp", None));
        let only_sales = vec![InboundBinding {
            plugin: "telegram".into(),
            instance: Some("sales".into()),
            ..Default::default()
        }];
        assert!(binding_matches(&only_sales, "telegram", Some("sales")));
        assert!(!binding_matches(&only_sales, "telegram", Some("boss")));
        // Binding asked for a specific instance but the topic didn't
        // have one — strict no-match (avoids leaks from legacy topics).
        assert!(!binding_matches(&only_sales, "telegram", None));
        // Multiple bindings: OR-semantic.
        let mixed = vec![
            InboundBinding {
                plugin: "telegram".into(),
                instance: Some("sales".into()),
                ..Default::default()
            },
            InboundBinding {
                plugin: "whatsapp".into(),
                instance: None,
                ..Default::default()
            },
        ];
        assert!(binding_matches(&mixed, "telegram", Some("sales")));
        assert!(binding_matches(&mixed, "whatsapp", Some("whatever")));
        assert!(!binding_matches(&mixed, "telegram", Some("boss")));
    }
    #[test]
    fn same_channel_distinguishes_senders_for_cleanup_race() {
        // The on-exit cleanup uses Sender::same_channel to avoid
        // evicting a newer entry that raced in after we decided to
        // shut down. Verify the primitive actually distinguishes.
        use tokio::sync::mpsc;
        let (a_tx, _a_rx) = mpsc::channel::<i32>(1);
        let (b_tx, _b_rx) = mpsc::channel::<i32>(1);
        assert!(a_tx.same_channel(&a_tx.clone()));
        assert!(!a_tx.same_channel(&b_tx));
    }
}
