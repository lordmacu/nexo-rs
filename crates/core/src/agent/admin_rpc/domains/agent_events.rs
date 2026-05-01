//! Phase 82.11 — `nexo/admin/agent_events/*` handlers.
//!
//! Backfill + search surface for the agent event firehose. The
//! live notification stream
//! (`nexo/notify/agent_event`) is wired separately at boot —
//! microapps that hold `transcripts_subscribe` /
//! `agent_events_subscribe_all` capabilities receive frames
//! automatically without an RPC subscribe call.
//!
//! Backed by a [`TranscriptReader`] trait so this crate stays
//! cycle-free vs `nexo-setup` (which holds the concrete
//! [`crate::agent::transcripts::TranscriptWriter`] +
//! [`crate::agent::transcripts_index::TranscriptsIndex`]
//! adapters).

use async_trait::async_trait;
use serde_json::Value;

use nexo_tool_meta::admin::agent_events::{
    AgentEventKind, AgentEventsListFilter, AgentEventsListResponse, AgentEventsReadParams,
    AgentEventsReadResponse, AgentEventsSearchParams, AgentEventsSearchResponse, SearchHit,
};

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Default `since_ms` when the caller omits the field. 30 days
/// matches the spec.
pub const DEFAULT_LIST_WINDOW_MS: u64 = 30 * 24 * 3_600 * 1000;
/// Default + cap for `list` results.
pub const DEFAULT_LIST_LIMIT: usize = 500;
const MAX_LIST_LIMIT: usize = 1000;
/// Default + cap for `read` results.
pub const DEFAULT_READ_LIMIT: usize = 200;
const MAX_READ_LIMIT: usize = 1000;
/// Default + cap for `search` results.
pub const DEFAULT_SEARCH_LIMIT: usize = 50;
const MAX_SEARCH_LIMIT: usize = 500;

/// Read-side surface the firehose admin handlers consume.
/// Production wires
/// `nexo_setup::admin_adapters::TranscriptReaderFs` against the
/// existing `TranscriptWriter` + `TranscriptsIndex`. Tests
/// inject mocks.
#[async_trait]
pub trait TranscriptReader: Send + Sync {
    /// List most-recent events for `agent_id` since `since_ms`,
    /// newest first, capped at `limit`.
    async fn list_recent_events(
        &self,
        filter: &AgentEventsListFilter,
    ) -> anyhow::Result<Vec<AgentEventKind>>;
    /// Read events for a single scope (`session_id` for
    /// `TranscriptAppended`) ordered ascending by `seq`.
    async fn read_session_events(
        &self,
        params: &AgentEventsReadParams,
    ) -> anyhow::Result<Vec<AgentEventKind>>;
    /// FTS5 search delegating to the transcripts index.
    async fn search_events(
        &self,
        params: &AgentEventsSearchParams,
    ) -> anyhow::Result<Vec<SearchHit>>;
}

/// `nexo/admin/agent_events/list` — most-recent events for one
/// agent. Filter shape is fully optional except `agent_id`.
pub async fn list(reader: &dyn TranscriptReader, params: Value) -> AdminRpcResult {
    let mut filter: AgentEventsListFilter = match serde_json::from_value(params) {
        Ok(f) => f,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if filter.agent_id.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("agent_id is empty".into()));
    }
    if filter.limit == 0 {
        filter.limit = DEFAULT_LIST_LIMIT;
    }
    filter.limit = filter.limit.min(MAX_LIST_LIMIT);

    let events = match reader.list_recent_events(&filter).await {
        Ok(e) => e,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "transcript_reader.list_recent_events: {e}"
            )));
        }
    };
    let response = AgentEventsListResponse { events };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/agent_events/read` — one scope of events.
/// Unknown `session_id` returns `events: []` (NOT
/// `MethodNotFound`) so callers tolerating empty-scope reads
/// don't have to special-case missing sessions.
pub async fn read(reader: &dyn TranscriptReader, params: Value) -> AdminRpcResult {
    let mut p: AgentEventsReadParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if p.agent_id.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("agent_id is empty".into()));
    }
    if p.limit == 0 {
        p.limit = DEFAULT_READ_LIMIT;
    }
    p.limit = p.limit.min(MAX_READ_LIMIT);

    let events = match reader.read_session_events(&p).await {
        Ok(e) => e,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "transcript_reader.read_session_events: {e}"
            )));
        }
    };
    let response = AgentEventsReadResponse { events };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/agent_events/search` — FTS5 query.
pub async fn search(reader: &dyn TranscriptReader, params: Value) -> AdminRpcResult {
    let mut p: AgentEventsSearchParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if p.agent_id.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("agent_id is empty".into()));
    }
    if p.query.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams("query is empty".into()));
    }
    if p.limit == 0 {
        p.limit = DEFAULT_SEARCH_LIMIT;
    }
    p.limit = p.limit.min(MAX_SEARCH_LIMIT);

    let hits = match reader.search_events(&p).await {
        Ok(h) => h,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "transcript_reader.search_events: {e}"
            )));
        }
    };
    let response = AgentEventsSearchResponse { hits };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use nexo_tool_meta::admin::agent_events::TranscriptRole;
    use uuid::Uuid;

    /// Minimal in-memory reader for the handler tests. Stores a
    /// flat `Vec<AgentEventKind>` indexed nothing fancy — the
    /// production adapter (Step 3) is the one that exercises
    /// real persistence.
    #[derive(Default)]
    struct MockReader {
        events: Mutex<Vec<AgentEventKind>>,
        last_filter: Mutex<Option<AgentEventsListFilter>>,
        last_read: Mutex<Option<AgentEventsReadParams>>,
        last_search: Mutex<Option<AgentEventsSearchParams>>,
        force_error: bool,
    }

    impl MockReader {
        fn new() -> Self {
            Self::default()
        }
        fn with_event(mut self, e: AgentEventKind) -> Self {
            self.events.get_mut().unwrap().push(e);
            self
        }
        fn forcing_error() -> Self {
            Self {
                force_error: true,
                ..Self::default()
            }
        }
    }

    #[async_trait]
    impl TranscriptReader for MockReader {
        async fn list_recent_events(
            &self,
            filter: &AgentEventsListFilter,
        ) -> anyhow::Result<Vec<AgentEventKind>> {
            *self.last_filter.lock().unwrap() = Some(filter.clone());
            if self.force_error {
                anyhow::bail!("forced");
            }
            // Newest-first ordering by `sent_at_ms`.
            let mut out: Vec<AgentEventKind> = self
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| match e {
                    AgentEventKind::TranscriptAppended { agent_id, .. } => {
                        agent_id == &filter.agent_id
                    }
                    _ => false,
                })
                .cloned()
                .collect();
            out.sort_by_key(|e| match e {
                AgentEventKind::TranscriptAppended { sent_at_ms, .. } => {
                    std::cmp::Reverse(*sent_at_ms)
                }
                _ => std::cmp::Reverse(0u64),
            });
            out.truncate(filter.limit);
            Ok(out)
        }

        async fn read_session_events(
            &self,
            params: &AgentEventsReadParams,
        ) -> anyhow::Result<Vec<AgentEventKind>> {
            *self.last_read.lock().unwrap() = Some(params.clone());
            if self.force_error {
                anyhow::bail!("forced");
            }
            let after = params.since_seq.unwrap_or(0);
            let mut out: Vec<AgentEventKind> = self
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| match e {
                    AgentEventKind::TranscriptAppended {
                        agent_id,
                        session_id,
                        seq,
                        ..
                    } => {
                        agent_id == &params.agent_id
                            && *session_id == params.session_id
                            && (params.since_seq.is_none() || *seq > after)
                    }
                    _ => false,
                })
                .cloned()
                .collect();
            out.sort_by_key(|e| match e {
                AgentEventKind::TranscriptAppended { seq, .. } => *seq,
                _ => 0u64,
            });
            out.truncate(params.limit);
            Ok(out)
        }

        async fn search_events(
            &self,
            params: &AgentEventsSearchParams,
        ) -> anyhow::Result<Vec<SearchHit>> {
            *self.last_search.lock().unwrap() = Some(params.clone());
            if self.force_error {
                anyhow::bail!("forced");
            }
            // Trivial mock — return one synthetic hit per matching body.
            let mut out: Vec<SearchHit> = Vec::new();
            for evt in self.events.lock().unwrap().iter() {
                let AgentEventKind::TranscriptAppended {
                    agent_id,
                    session_id,
                    sent_at_ms,
                    role,
                    body,
                    source_plugin,
                    ..
                } = evt
                else {
                    continue;
                };
                if agent_id != &params.agent_id {
                    continue;
                }
                if !body.contains(&params.query) {
                    continue;
                }
                out.push(SearchHit {
                    session_id: *session_id,
                    timestamp_ms: *sent_at_ms,
                    role: *role,
                    source_plugin: source_plugin.clone(),
                    snippet: format!("[{}]", params.query),
                });
            }
            out.truncate(params.limit);
            Ok(out)
        }
    }

    fn evt(agent: &str, sid: Uuid, seq: u64, body: &str, ts_ms: u64) -> AgentEventKind {
        AgentEventKind::TranscriptAppended {
            agent_id: agent.into(),
            session_id: sid,
            seq,
            role: TranscriptRole::User,
            body: body.into(),
            sent_at_ms: ts_ms,
            sender_id: None,
            source_plugin: "whatsapp".into(),
        }
    }

    #[tokio::test]
    async fn list_returns_events_newest_first_capped() {
        let s1 = Uuid::nil();
        let s2 = Uuid::from_u128(2);
        let reader = MockReader::new()
            .with_event(evt("ana", s1, 0, "hi", 1_000))
            .with_event(evt("ana", s2, 0, "yo", 3_000))
            .with_event(evt("ana", s1, 1, "hola", 2_000))
            .with_event(evt("bob", s1, 0, "ignored", 4_000));
        let result = list(
            &reader,
            serde_json::json!({ "agent_id": "ana", "limit": 5 }),
        )
        .await;
        let response: AgentEventsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.events.len(), 3);
        match &response.events[0] {
            AgentEventKind::TranscriptAppended { sent_at_ms, .. } => {
                assert_eq!(*sent_at_ms, 3_000);
            }
            _ => panic!("expected TranscriptAppended"),
        }
    }

    #[tokio::test]
    async fn list_rejects_empty_agent_id() {
        let reader = MockReader::new();
        let result = list(&reader, serde_json::json!({ "agent_id": "" })).await;
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn read_returns_events_after_since_seq() {
        let sid = Uuid::nil();
        let reader = MockReader::new()
            .with_event(evt("ana", sid, 0, "a", 1_000))
            .with_event(evt("ana", sid, 1, "b", 2_000))
            .with_event(evt("ana", sid, 2, "c", 3_000));
        let result = read(
            &reader,
            serde_json::json!({
                "agent_id": "ana",
                "session_id": sid,
                "since_seq": 0,
                "limit": 50
            }),
        )
        .await;
        let response: AgentEventsReadResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.events.len(), 2);
        match &response.events[0] {
            AgentEventKind::TranscriptAppended { seq, .. } => assert_eq!(*seq, 1),
            _ => panic!("expected TranscriptAppended"),
        }
    }

    #[tokio::test]
    async fn read_unknown_session_returns_empty_not_error() {
        let reader = MockReader::new().with_event(evt("ana", Uuid::nil(), 0, "x", 1));
        let result = read(
            &reader,
            serde_json::json!({
                "agent_id": "ana",
                "session_id": Uuid::from_u128(99),
            }),
        )
        .await;
        let response: AgentEventsReadResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.events.is_empty());
    }

    #[tokio::test]
    async fn search_delegates_and_caps_limit() {
        let reader = MockReader::new()
            .with_event(evt("ana", Uuid::nil(), 0, "[REDACTED:phone] hola", 1_000));
        let result = search(
            &reader,
            serde_json::json!({
                "agent_id": "ana",
                "query": "hola",
                "limit": 0,
            }),
        )
        .await;
        let response: AgentEventsSearchResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.hits.len(), 1);
        // limit=0 → server default; verify the mock saw the
        // resolved value, not the on-wire 0.
        let resolved = reader.last_search.lock().unwrap().clone().unwrap();
        assert_eq!(resolved.limit, DEFAULT_SEARCH_LIMIT);
    }

    #[tokio::test]
    async fn search_rejects_empty_query() {
        let reader = MockReader::new();
        let result = search(
            &reader,
            serde_json::json!({ "agent_id": "ana", "query": "   " }),
        )
        .await;
        let err = result.error.expect("error");
        match err {
            AdminRpcError::InvalidParams(m) => assert!(m.contains("query")),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_propagates_reader_error_as_internal() {
        let reader = MockReader::forcing_error();
        let result = list(&reader, serde_json::json!({ "agent_id": "ana" })).await;
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::Internal(_)));
    }
}
