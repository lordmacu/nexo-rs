//! Phase 80.11.b — receive side for the agent inbox.
//!
//! Subscribes once to `agent.inbox.>` and routes incoming
//! `InboxMessage`s into per-goal in-memory buffers. The runtime
//! per-turn loop drains a goal's buffer at turn start, renders
//! the messages as a `<peer-message from="...">` system block,
//! and prepends them to `channel_meta_parts` so the LLM sees the
//! messages on its next turn.
//!
//! Mirrors the `TeamMessageRouter` (Phase 79.6) pattern: single
//! broker subscriber + per-key dashmap of in-memory consumers,
//! cancelled by a `CancellationToken`. Buffer-on-demand: messages
//! addressed to a goal that hasn't registered yet still queue in
//! a fresh buffer so the consumer can drain when it eventually
//! starts (race-safe under fast-spawn-then-immediate-send).
//!
//! Provider-agnostic — pure NATS subject + JSON payload + in-memory
//! VecDeque. The receive loop runs adjacent to the LLM round-trip,
//! independent of which provider drives the agent.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use nexo_broker::{BrokerHandle, Event};
use nexo_driver_types::GoalId;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::inbox::{InboxMessage, INBOX_SUBJECT_PREFIX};

/// Maximum messages buffered per goal before FIFO eviction kicks
/// in. Long-idle goals shouldn't accumulate unbounded backlog.
pub const MAX_QUEUE: usize = 64;

/// Per-goal FIFO buffer. Push from the router task, drain from the
/// runtime per-turn loop. Mutex held only for the microsecond
/// push/drain windows.
pub struct InboxBuffer {
    queue: Mutex<VecDeque<InboxMessage>>,
}

impl InboxBuffer {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(8)),
        }
    }

    /// Push a message. When the queue is at `MAX_QUEUE` capacity,
    /// the oldest message is evicted FIFO and a warn line is
    /// logged. Returns `true` when an eviction happened.
    pub fn push(&self, msg: InboxMessage) -> bool {
        let mut q = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        let evicted = if q.len() >= MAX_QUEUE {
            q.pop_front().is_some()
        } else {
            false
        };
        q.push_back(msg);
        if evicted {
            tracing::warn!(
                target: "agent::inbox_router",
                cap = MAX_QUEUE,
                "inbox buffer at cap; evicted oldest message"
            );
        }
        evicted
    }

    /// Atomically empty the queue and return its contents in
    /// chronological order (oldest first). Cheap when the queue is
    /// empty (returns an empty `Vec`).
    pub fn drain(&self) -> Vec<InboxMessage> {
        let mut q = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        q.drain(..).collect()
    }

    /// Current queue length without draining. Used by the
    /// `agent_status`-style read tools.
    pub fn len(&self) -> usize {
        self.queue.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Inbox router: single broker subscriber + per-goal dashmap of
/// `InboxBuffer`. Operator boots one per process and registers
/// each goal at spawn time.
pub struct InboxRouter<B: BrokerHandle + ?Sized> {
    broker: Arc<B>,
    buffers: DashMap<GoalId, Arc<InboxBuffer>>,
}

impl<B: BrokerHandle + ?Sized + 'static> InboxRouter<B> {
    pub fn new(broker: Arc<B>) -> Arc<Self> {
        Arc::new(Self {
            broker,
            buffers: DashMap::new(),
        })
    }

    /// Spawn the broker subscriber on `agent.inbox.>`. Cancelled by
    /// dropping `cancel`. Returns the `JoinHandle` so the caller
    /// can `await` graceful shutdown.
    pub fn spawn(self: &Arc<Self>, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let pattern = format!("{}.>", INBOX_SUBJECT_PREFIX);
            let mut sub = match me.broker.subscribe(&pattern).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        target: "agent::inbox_router",
                        error = %e,
                        "could not subscribe to {pattern} — peer messages offline"
                    );
                    return;
                }
            };
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    next = sub.next() => {
                        match next {
                            Some(ev) => me.dispatch_inbound(ev),
                            None => break,
                        }
                    }
                }
            }
        })
    }

    /// Register or fetch the buffer for a goal. Idempotent — a
    /// re-register returns the existing buffer so previously queued
    /// (buffer-on-demand) messages survive.
    pub fn register(&self, goal_id: GoalId) -> Arc<InboxBuffer> {
        Arc::clone(
            self.buffers
                .entry(goal_id)
                .or_insert_with(|| Arc::new(InboxBuffer::new()))
                .value(),
        )
    }

    /// Drop a goal's buffer (call on goal terminal state).
    pub fn forget(&self, goal_id: GoalId) {
        self.buffers.remove(&goal_id);
    }

    /// Number of registered (or buffer-on-demand) goal buffers.
    pub fn buffer_count(&self) -> usize {
        self.buffers.len()
    }

    /// Parse the subject suffix as a UUID, look up or create the
    /// buffer, push the message. Drops malformed subjects /
    /// payloads with a debug log.
    fn dispatch_inbound(&self, ev: Event) {
        // Subject shape: `agent.inbox.<goal_id_uuid>`.
        let goal_id = match parse_goal_from_subject(&ev.topic) {
            Some(g) => g,
            None => {
                tracing::debug!(
                    target: "agent::inbox_router",
                    topic = %ev.topic,
                    "[inbox] dropping subject without parsable goal_id"
                );
                return;
            }
        };
        let msg: InboxMessage = match serde_json::from_value(ev.payload) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(
                    target: "agent::inbox_router",
                    topic = %ev.topic,
                    error = %e,
                    "[inbox] dropping malformed payload"
                );
                return;
            }
        };
        // Buffer-on-demand: `entry.or_insert_with` creates a fresh
        // buffer if the goal hasn't registered yet, so a fast-spawn-
        // then-immediate-peer-send pattern doesn't lose the message.
        let buf = Arc::clone(
            self.buffers
                .entry(goal_id)
                .or_insert_with(|| Arc::new(InboxBuffer::new()))
                .value(),
        );
        buf.push(msg);
    }
}

fn parse_goal_from_subject(subject: &str) -> Option<GoalId> {
    let prefix = format!("{}.", INBOX_SUBJECT_PREFIX);
    let suffix = subject.strip_prefix(&prefix)?;
    Uuid::parse_str(suffix).ok().map(GoalId)
}

/// Pure-fn renderer. Returns `None` when the slice is empty so
/// callers can `if let Some(block) = render_peer_messages_block(&msgs)`
/// inside a `channel_meta_parts.push(block)` chain.
pub fn render_peer_messages_block(messages: &[InboxMessage]) -> Option<String> {
    if messages.is_empty() {
        return None;
    }
    let mut out = String::from("# PEER MESSAGES\n\n");
    for m in messages {
        let corr = match m.correlation_id {
            Some(c) => format!(r#" correlation_id="{c}""#),
            None => String::new(),
        };
        out.push_str(&format!(
            "<peer-message from=\"{}\" sent_at=\"{}\"{}>\n{}\n</peer-message>\n",
            m.from_agent_id,
            m.sent_at.to_rfc3339(),
            corr,
            m.body,
        ));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use nexo_broker::AnyBroker;
    use nexo_broker::BrokerHandle;

    fn mk_msg(from: &str, body: &str) -> InboxMessage {
        InboxMessage {
            from_agent_id: from.into(),
            from_goal_id: GoalId(Uuid::new_v4()),
            to_agent_id: "kate".into(),
            body: body.into(),
            sent_at: Utc::now(),
            correlation_id: None,
        }
    }

    #[test]
    fn buffer_push_drain_round_trip() {
        let buf = InboxBuffer::new();
        assert!(buf.is_empty());
        buf.push(mk_msg("a", "first"));
        buf.push(mk_msg("b", "second"));
        assert_eq!(buf.len(), 2);
        let drained = buf.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].body, "first");
        assert_eq!(drained[1].body, "second");
        assert!(buf.is_empty());
    }

    #[test]
    fn buffer_drain_empty_returns_empty_vec() {
        let buf = InboxBuffer::new();
        let drained = buf.drain();
        assert!(drained.is_empty());
    }

    #[test]
    fn buffer_evicts_oldest_at_cap() {
        let buf = InboxBuffer::new();
        for i in 0..MAX_QUEUE {
            buf.push(mk_msg(&format!("a{i}"), "msg"));
        }
        assert_eq!(buf.len(), MAX_QUEUE);
        // Push one more — eviction.
        let evicted = buf.push(mk_msg("over", "msg"));
        assert!(evicted);
        assert_eq!(buf.len(), MAX_QUEUE);
        let drained = buf.drain();
        // Oldest (a0) should have been evicted; newest "over" present.
        assert_eq!(drained.len(), MAX_QUEUE);
        assert_ne!(drained[0].from_agent_id, "a0");
        assert_eq!(drained.last().unwrap().from_agent_id, "over");
    }

    #[test]
    fn parse_goal_from_subject_valid() {
        let goal = GoalId(Uuid::new_v4());
        let subj = format!("{}.{}", INBOX_SUBJECT_PREFIX, goal.0);
        assert_eq!(parse_goal_from_subject(&subj), Some(goal));
    }

    #[test]
    fn parse_goal_from_subject_rejects_unknown_prefix() {
        let subj = "team.broadcast.foo";
        assert_eq!(parse_goal_from_subject(subj), None);
    }

    #[test]
    fn parse_goal_from_subject_rejects_non_uuid_suffix() {
        let subj = format!("{}.not-a-uuid", INBOX_SUBJECT_PREFIX);
        assert_eq!(parse_goal_from_subject(&subj), None);
    }

    #[test]
    fn render_empty_returns_none() {
        assert!(render_peer_messages_block(&[]).is_none());
    }

    #[test]
    fn render_single_message_includes_from_and_body() {
        let msgs = vec![mk_msg("researcher", "task #1 ready")];
        let block = render_peer_messages_block(&msgs).unwrap();
        assert!(block.starts_with("# PEER MESSAGES"));
        assert!(block.contains(r#"from="researcher""#));
        assert!(block.contains("task #1 ready"));
        assert!(!block.contains("correlation_id"));
    }

    #[test]
    fn render_with_correlation_id_includes_attribute() {
        let mut m = mk_msg("a", "b");
        m.correlation_id = Some(Uuid::new_v4());
        let corr = m.correlation_id.unwrap();
        let block = render_peer_messages_block(&[m]).unwrap();
        assert!(block.contains(&format!(r#"correlation_id="{corr}""#)));
    }

    #[test]
    fn render_preserves_chronological_order() {
        let msgs = vec![
            mk_msg("a", "first"),
            mk_msg("b", "second"),
            mk_msg("c", "third"),
        ];
        let block = render_peer_messages_block(&msgs).unwrap();
        let pos_first = block.find("first").unwrap();
        let pos_second = block.find("second").unwrap();
        let pos_third = block.find("third").unwrap();
        assert!(pos_first < pos_second);
        assert!(pos_second < pos_third);
    }

    #[tokio::test]
    async fn router_register_idempotent_returns_same_buffer() {
        let broker = Arc::new(AnyBroker::local());
        let router = InboxRouter::new(broker);
        let goal = GoalId(Uuid::new_v4());
        let buf1 = router.register(goal);
        let buf2 = router.register(goal);
        // Push via buf1, drain via buf2 — same buffer instance.
        buf1.push(mk_msg("a", "ping"));
        let drained = buf2.drain();
        assert_eq!(drained.len(), 1);
    }

    #[tokio::test]
    async fn router_dispatch_inbound_pushes_to_buffer() {
        let broker = Arc::new(AnyBroker::local());
        let router = InboxRouter::new(broker);
        let goal = GoalId(Uuid::new_v4());
        let buf = router.register(goal);

        let msg = mk_msg("researcher", "task #1");
        let payload = serde_json::to_value(&msg).unwrap();
        let topic = format!("{}.{}", INBOX_SUBJECT_PREFIX, goal.0);
        let event = Event::new(&topic, "test", payload);
        router.dispatch_inbound(event);

        let drained = buf.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].body, "task #1");
    }

    #[tokio::test]
    async fn router_buffer_on_demand_for_unregistered_goal() {
        let broker = Arc::new(AnyBroker::local());
        let router = InboxRouter::new(broker);
        let goal = GoalId(Uuid::new_v4());

        // Send BEFORE register — message buffered on-demand.
        let msg = mk_msg("researcher", "early bird");
        let payload = serde_json::to_value(&msg).unwrap();
        let topic = format!("{}.{}", INBOX_SUBJECT_PREFIX, goal.0);
        router.dispatch_inbound(Event::new(&topic, "test", payload));

        assert_eq!(router.buffer_count(), 1);
        // Now register — should see the buffered message.
        let buf = router.register(goal);
        let drained = buf.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].body, "early bird");
    }

    #[tokio::test]
    async fn router_drops_malformed_subject() {
        let broker = Arc::new(AnyBroker::local());
        let router = InboxRouter::new(broker);
        let event = Event::new("agent.inbox.not-a-uuid", "test", serde_json::json!({}));
        router.dispatch_inbound(event); // does not panic
        assert_eq!(router.buffer_count(), 0);
    }

    #[tokio::test]
    async fn router_drops_malformed_payload() {
        let broker = Arc::new(AnyBroker::local());
        let router = InboxRouter::new(broker);
        let goal = GoalId(Uuid::new_v4());
        let topic = format!("{}.{}", INBOX_SUBJECT_PREFIX, goal.0);
        let event = Event::new(&topic, "test", serde_json::json!({"garbage": true}));
        router.dispatch_inbound(event);
        assert_eq!(router.buffer_count(), 0);
    }

    #[tokio::test]
    async fn router_forget_drops_buffer() {
        let broker = Arc::new(AnyBroker::local());
        let router = InboxRouter::new(broker);
        let goal = GoalId(Uuid::new_v4());
        let _ = router.register(goal);
        assert_eq!(router.buffer_count(), 1);
        router.forget(goal);
        assert_eq!(router.buffer_count(), 0);
    }

    #[tokio::test]
    async fn router_spawn_subscribes_and_routes_end_to_end() {
        // End-to-end via the broker's pubsub: spawn the router,
        // publish to the goal's inbox subject, drain the buffer.
        let broker = Arc::new(AnyBroker::local());
        let router = InboxRouter::new(broker.clone());
        let cancel = CancellationToken::new();
        let _handle = router.spawn(cancel.clone());
        // Give the subscriber a moment to register.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let goal = GoalId(Uuid::new_v4());
        let buf = router.register(goal);

        let msg = mk_msg("researcher", "via broker");
        let payload = serde_json::to_value(&msg).unwrap();
        let topic = format!("{}.{}", INBOX_SUBJECT_PREFIX, goal.0);
        broker.publish(&topic, Event::new(&topic, "test", payload))
            .await
            .unwrap();

        // Allow the subscriber loop to process.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let drained = buf.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].body, "via broker");
        cancel.cancel();
    }
}
