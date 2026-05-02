//! Phase 83.8.7 — `TranscriptStream` SDK helper.
//!
//! Wraps a tokio `mpsc::Receiver<AgentEventKind>` (typically fed
//! by the `nexo/notify/agent_event` firehose handler the SDK
//! runtime exposes on stdin) with a multi-tenant defense-in-depth
//! filter so a microapp serving multiple clients only ever sees
//! events for the agents it is allowed to read.
//!
//! The filter is applied on `next()` rather than at subscription
//! time — drops the event before it crosses the SDK/microapp
//! boundary instead of relying on every microapp author to remember
//! the check.

use std::collections::HashSet;
use std::sync::Arc;

use nexo_tool_meta::admin::agent_events::AgentEventKind;
use tokio::sync::mpsc;

/// Multi-tenant transcript-event stream — wraps the firehose
/// receiver with an optional `allowed agent_ids` set.
pub struct TranscriptStream {
    rx: mpsc::Receiver<AgentEventKind>,
    allowed: Option<Arc<HashSet<String>>>,
}

impl std::fmt::Debug for TranscriptStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscriptStream")
            .field("allowed_count", &self.allowed.as_ref().map(|s| s.len()))
            .finish()
    }
}

impl TranscriptStream {
    /// Build a stream over `rx`. No filter is attached; call
    /// [`Self::filter_by_agent`] to scope the stream to one
    /// tenant.
    pub fn new(rx: mpsc::Receiver<AgentEventKind>) -> Self {
        Self { rx, allowed: None }
    }

    /// Attach (or replace) the agent-id filter. Subsequent calls
    /// to [`Self::next`] only return events whose `agent_id`
    /// belongs to `allowed`. An empty set drops every event —
    /// `[]` is the explicit "no agents allowed" form, distinct
    /// from "no filter" (`None`, the default).
    pub fn filter_by_agent(mut self, allowed: HashSet<String>) -> Self {
        self.allowed = Some(Arc::new(allowed));
        self
    }

    /// Slice convenience — turns `&[String]` into the `HashSet`
    /// the filter requires. Use when the caller has the agent
    /// ids in vec form already (typical microapp shape: list of
    /// agents owned by a client tenant).
    pub fn filter_by_agent_slice(self, allowed: &[String]) -> Self {
        self.filter_by_agent(allowed.iter().cloned().collect())
    }

    /// Whether a filter is currently attached. Visibility-only —
    /// callers normally do not branch on this.
    pub fn has_filter(&self) -> bool {
        self.allowed.is_some()
    }

    /// Receive the next event whose `agent_id` matches the
    /// attached filter (or any event when no filter is set).
    /// Drops mismatching events silently and pulls the next one
    /// from the channel until one passes or the channel closes.
    pub async fn next(&mut self) -> Option<AgentEventKind> {
        loop {
            let event = self.rx.recv().await?;
            if Self::accept(&self.allowed, &event) {
                return Some(event);
            }
            // Otherwise loop and pull again.
        }
    }

    fn accept(allowed: &Option<Arc<HashSet<String>>>, event: &AgentEventKind) -> bool {
        let Some(allowed) = allowed else {
            return true;
        };
        match event {
            AgentEventKind::TranscriptAppended { agent_id, .. } => allowed.contains(agent_id),
            // Defense-in-depth: any future variant that does not
            // expose an `agent_id` is dropped under a non-empty
            // filter so multi-tenant isolation is preserved by
            // default.
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::admin::agent_events::TranscriptRole;
    use uuid::Uuid;

    fn ev(agent_id: &str) -> AgentEventKind {
        AgentEventKind::TranscriptAppended {
            agent_id: agent_id.into(),
            session_id: Uuid::new_v4(),
            seq: 0,
            role: TranscriptRole::User,
            body: "hello".into(),
            sent_at_ms: 0,
            sender_id: None,
            source_plugin: "whatsapp".into(),
        }
    }

    fn channel() -> (mpsc::Sender<AgentEventKind>, TranscriptStream) {
        let (tx, rx) = mpsc::channel(8);
        (tx, TranscriptStream::new(rx))
    }

    #[tokio::test]
    async fn no_filter_passes_every_event() {
        let (tx, mut stream) = channel();
        for a in ["a", "b", "c"] {
            tx.send(ev(a)).await.unwrap();
        }
        drop(tx);
        let mut seen = Vec::new();
        while let Some(e) = stream.next().await {
            if let AgentEventKind::TranscriptAppended { agent_id, .. } = e {
                seen.push(agent_id);
            }
        }
        assert_eq!(seen, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn filter_by_agent_drops_mismatches() {
        let (tx, stream) = channel();
        let allowed: HashSet<String> = ["a".into(), "c".into()].into_iter().collect();
        let mut stream = stream.filter_by_agent(allowed);
        for a in ["a", "b", "c", "d"] {
            tx.send(ev(a)).await.unwrap();
        }
        drop(tx);
        let mut seen = Vec::new();
        while let Some(e) = stream.next().await {
            if let AgentEventKind::TranscriptAppended { agent_id, .. } = e {
                seen.push(agent_id);
            }
        }
        assert_eq!(seen, vec!["a", "c"]);
    }

    #[tokio::test]
    async fn filter_by_agent_slice_round_trips() {
        let (tx, stream) = channel();
        let mut stream = stream.filter_by_agent_slice(&["x".into()]);
        tx.send(ev("y")).await.unwrap();
        tx.send(ev("x")).await.unwrap();
        drop(tx);
        let first = stream.next().await.unwrap();
        if let AgentEventKind::TranscriptAppended { agent_id, .. } = first {
            assert_eq!(agent_id, "x");
        } else {
            panic!("unexpected variant");
        }
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn empty_allowed_set_drops_everything() {
        let (tx, stream) = channel();
        let mut stream = stream.filter_by_agent(HashSet::new());
        tx.send(ev("a")).await.unwrap();
        drop(tx);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn closed_channel_returns_none() {
        let (tx, mut stream) = channel();
        drop(tx);
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn has_filter_reports_attachment_state() {
        let (_tx, stream) = channel();
        assert!(!stream.has_filter());
        let stream = stream.filter_by_agent(HashSet::new());
        assert!(stream.has_filter());
    }
}
