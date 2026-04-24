use std::mem;
use std::sync::Arc;
use std::time::Duration;
use dashmap::DashMap;
use serde_json::Value;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinSet;
use tokio::time::{sleep_until, Instant};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use uuid::Uuid;
use agent_broker::{AnyBroker, BrokerHandle};
use agent_memory::LongTermMemory;
use super::agent::Agent;
use super::behavior::AgentBehavior;
use super::context::AgentContext;
use super::peer_directory::PeerDirectory;
use super::routing::{route_topic, AgentMessage, AgentPayload, AgentRouter};
use super::sender_rate_limit::SenderRateLimiter;
use super::types::{InboundMedia, InboundMessage, RunTrigger};
use agent_config::types::agents::InboundBinding;
use crate::heartbeat::{heartbeat_interval, heartbeat_topic, publish_heartbeat};
use crate::session::SessionManager;
use crate::telemetry::inc_messages_processed_total;
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
    /// Per-sender inbound throttle — denies messages whose sender_id
    /// exceeds its bucket. `None` = unlimited (back-compat). Built
    /// from `agent.config.sender_rate_limit` when present.
    sender_rate_limiter: Option<Arc<SenderRateLimiter>>,
    shutdown: CancellationToken,
    tasks: Arc<Mutex<JoinSet<()>>>,
}
impl AgentRuntime {
    pub fn new(agent: Arc<Agent>, broker: AnyBroker, sessions: Arc<SessionManager>) -> Self {
        let debounce_ms = Duration::from_millis(agent.config.config.debounce_ms);
        let queue_cap = agent.config.config.queue_cap;
        // Auto-wire the sender rate limiter from config so `new()`
        // already honors `sender_rate_limit`. Callers don't need a
        // separate builder call for the common path.
        let sender_rate_limiter = agent
            .config
            .sender_rate_limit
            .clone()
            .map(|cfg| Arc::new(SenderRateLimiter::new(cfg)));
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
            sender_rate_limiter,
            shutdown: CancellationToken::new(),
            tasks: Arc::new(Mutex::new(JoinSet::new())),
        }
    }
    pub fn with_memory(mut self, memory: Arc<LongTermMemory>) -> Self {
        self.memory = Some(memory);
        self
    }
    pub fn with_peers(mut self, peers: Arc<PeerDirectory>) -> Self {
        self.peers = Some(peers);
        self
    }
    pub fn router(&self) -> Arc<AgentRouter> {
        Arc::clone(&self.router)
    }
    pub async fn start(&self) -> anyhow::Result<()> {
        let plugin_topic = "plugin.inbound.>";
        let mut plugin_sub = self.broker.subscribe(plugin_topic).await?;
        let heartbeat_topic = heartbeat_topic(&self.agent.id);
        let mut heartbeat_sub = self.broker.subscribe(&heartbeat_topic).await?;
        let route_inbound_topic = route_topic(&self.agent.id);
        let mut route_sub = self.broker.subscribe(&route_inbound_topic).await?;
        let agent = Arc::clone(&self.agent);
        let sessions = Arc::clone(&self.sessions);
        let broker = self.broker.clone();
        let memory = self.memory.clone();
        let peers = self.peers.clone();
        let router = Arc::clone(&self.router);
        let session_txs = Arc::clone(&self.session_txs);
        let debounce_ms = self.debounce_ms;
        let queue_cap = self.queue_cap;
        let sender_rate_limiter = self.sender_rate_limiter.clone();
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
            ctx = ctx.with_router(Arc::clone(&router));
            loop {
                tokio::select! {
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
                        // Populated list = strict allowlist.
                        let bindings = &agent.config.inbound_bindings;
                        if !bindings.is_empty()
                            && !binding_matches(bindings, &source_plugin, source_instance.as_deref())
                        {
                            tracing::trace!(
                                agent_id = %agent.id,
                                plugin = %source_plugin,
                                instance = source_instance.as_deref().unwrap_or("-"),
                                "inbound dropped by binding filter",
                            );
                            continue;
                        }
                        let sender_id = event.payload
                            .get("from")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        // Per-sender rate limit — applied after binding
                        // filter so we don't waste bucket tokens on
                        // events the agent would drop anyway. A denied
                        // event is silently dropped (trace-logged) so
                        // the sender doesn't get a "rate limited" reply
                        // they could use to probe the bot.
                        if let Some(rl) = &sender_rate_limiter {
                            if !rl.try_acquire(&agent.id, sender_id.as_deref()).await {
                                tracing::trace!(
                                    agent_id = %agent.id,
                                    plugin = %source_plugin,
                                    sender = sender_id.as_deref().unwrap_or("-"),
                                    "inbound dropped by sender rate limit",
                                );
                                continue;
                            }
                        }
                        let media = extract_inbound_media(&event.payload);
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
                        let entry = session_txs.entry(session_id).or_insert_with(|| {
                            let (tx, rx) = mpsc::channel(queue_cap);
                            let tx_for_task = tx.clone();
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
                                        let evt = agent_broker::Event::new(
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
                                let evt = agent_broker::Event::new(&topic, &agent.id, payload);
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
                                let evt = agent_broker::Event::new(
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
                let mut ticker = tokio::time::interval(interval);
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
                loop {
                    match rx.try_recv() {
                        Ok(m) => buffer.push(m),
                        Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                    }
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
            let mut ev = agent_broker::Event::new(
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
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(InboundMedia { kind, path, mime_type })
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
/// Check whether `(plugin, instance)` matches at least one binding.
/// A binding with `instance=None` matches any instance of its plugin —
/// including events with no instance at all. Used by the runtime
/// inbound-subscriber loop.
fn binding_matches(
    bindings: &[InboundBinding],
    plugin: &str,
    instance: Option<&str>,
) -> bool {
    bindings.iter().any(|b| {
        if b.plugin != plugin {
            return false;
        }
        match (&b.instance, instance) {
            (None, _) => true,                      // plugin-wide binding
            (Some(want), Some(got)) => want == got,
            (Some(_), None) => false,               // binding asked for instance, topic had none
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_topic_extracts_plugin_and_optional_instance() {
        assert_eq!(parse_inbound_topic("plugin.inbound.telegram"),
            ("telegram".into(), None));
        assert_eq!(parse_inbound_topic("plugin.inbound.telegram.sales"),
            ("telegram".into(), Some("sales".into())));
        // Nested instances collapse — everything after the 2nd dot is
        // treated as the instance name so bot_names can contain `.`.
        assert_eq!(parse_inbound_topic("plugin.inbound.telegram.bot.v2"),
            ("telegram".into(), Some("bot.v2".into())));
        // Non-inbound topics → neutral sentinel; binding filter rejects
        // them unless bindings are empty.
        assert_eq!(parse_inbound_topic("something.else"),
            (String::new(), None));
        assert_eq!(parse_inbound_topic("plugin.inbound."),
            (String::new(), None));
    }
    #[test]
    fn binding_matches_covers_plugin_wide_and_exact_instance() {
        let all_telegram = vec![InboundBinding {
            plugin: "telegram".into(),
            instance: None,
        }];
        assert!(binding_matches(&all_telegram, "telegram", None));
        assert!(binding_matches(&all_telegram, "telegram", Some("anyone")));
        assert!(!binding_matches(&all_telegram, "whatsapp", None));
        let only_sales = vec![InboundBinding {
            plugin: "telegram".into(),
            instance: Some("sales".into()),
        }];
        assert!(binding_matches(&only_sales, "telegram", Some("sales")));
        assert!(!binding_matches(&only_sales, "telegram", Some("boss")));
        // Binding asked for a specific instance but the topic didn't
        // have one — strict no-match (avoids leaks from legacy topics).
        assert!(!binding_matches(&only_sales, "telegram", None));
        // Multiple bindings: OR-semantic.
        let mixed = vec![
            InboundBinding { plugin: "telegram".into(), instance: Some("sales".into()) },
            InboundBinding { plugin: "whatsapp".into(), instance: None },
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
