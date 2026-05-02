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

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use nexo_tool_meta::admin::agent_events::{
    AgentEventKind, AgentEventsListFilter, AgentEventsListResponse, AgentEventsReadParams,
    AgentEventsReadResponse, AgentEventsSearchParams, AgentEventsSearchResponse, SearchHit,
};

use crate::agent::admin_rpc::agent_events_sqlite::{AgentEventLog, AgentEventLogFilter};
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

/// Phase 82.11.log.merge — `TranscriptReader` impl that
/// composes a transcripts source (JSONL via
/// `TranscriptReaderFs`) with a durable
/// [`AgentEventLog`] (firehose backfill). Operator UIs that
/// open after a `ProcessingStateChanged` / `EscalationRequested`
/// emit see those rows in `agent_events/list` even though the
/// transcripts JSONL never captured them.
///
/// The merge respects the `kind` filter:
/// - `Some("transcript_appended")`: query transcripts only.
/// - `Some(other)`: query the durable log only (with the kind
///   pushed into [`AgentEventLogFilter::kind`]).
/// - `None`: query both, drop transcripts from the log result
///   so they aren't double-counted (boot wires the log as a
///   `Tee` sink alongside the broadcast emitter, so it
///   receives every kind including TranscriptAppended), then
///   merge by `at_ms` desc and truncate to `filter.limit`.
///
/// `read_session_events` and `search_events` pass through to
/// the transcripts source — `session_id` semantics + FTS5 are
/// transcript-only today.
pub struct MergingAgentEventReader {
    transcripts: Arc<dyn TranscriptReader>,
    log: Arc<dyn AgentEventLog>,
}

impl std::fmt::Debug for MergingAgentEventReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergingAgentEventReader").finish()
    }
}

impl MergingAgentEventReader {
    /// Build the merger from the two backing sources. Boot
    /// wires `transcripts: Arc<TranscriptReaderFs>` and
    /// `log: Arc<SqliteAgentEventLog>`.
    pub fn new(
        transcripts: Arc<dyn TranscriptReader>,
        log: Arc<dyn AgentEventLog>,
    ) -> Self {
        Self { transcripts, log }
    }
}

/// Pulls the timestamp from any `AgentEventKind` variant —
/// matches the same field set the SQLite log denormalises.
/// Unknown future variants surface as `0` so they sort to the
/// bottom on a `DESC` order.
fn event_at_ms(event: &AgentEventKind) -> u64 {
    match event {
        AgentEventKind::TranscriptAppended { sent_at_ms, .. } => *sent_at_ms,
        AgentEventKind::PendingInboundsDropped { at_ms, .. } => *at_ms,
        AgentEventKind::EscalationRequested {
            requested_at_ms, ..
        } => *requested_at_ms,
        AgentEventKind::EscalationResolved { resolved_at_ms, .. } => {
            *resolved_at_ms
        }
        AgentEventKind::ProcessingStateChanged { at_ms, .. } => *at_ms,
        _ => 0,
    }
}

#[async_trait]
impl TranscriptReader for MergingAgentEventReader {
    async fn list_recent_events(
        &self,
        filter: &AgentEventsListFilter,
    ) -> anyhow::Result<Vec<AgentEventKind>> {
        let limit = if filter.limit == 0 {
            DEFAULT_LIST_LIMIT
        } else {
            filter.limit.min(MAX_LIST_LIMIT)
        };
        // Branch on kind — the merger pushes the filter down so
        // each backing source only loads what is asked.
        match filter.kind.as_deref() {
            Some("transcript_appended") => {
                self.transcripts.list_recent_events(filter).await
            }
            Some(other) => {
                let log_filter = AgentEventLogFilter {
                    agent_id: filter.agent_id.clone(),
                    kind: Some(other.to_string()),
                    since_ms: filter.since_ms,
                    tenant_id: filter.tenant_id.clone(),
                    limit,
                };
                self.log.list_recent(&log_filter).await
            }
            None => {
                // Pull from both — over-read each source so the
                // merge truncation does not lose recent rows from
                // the lighter side.
                let transcripts = self
                    .transcripts
                    .list_recent_events(filter)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "merger: transcripts side failed");
                        Vec::new()
                    });
                let log_filter = AgentEventLogFilter {
                    agent_id: filter.agent_id.clone(),
                    kind: None,
                    since_ms: filter.since_ms,
                    tenant_id: filter.tenant_id.clone(),
                    limit,
                };
                let log_rows = self
                    .log
                    .list_recent(&log_filter)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "merger: log side failed");
                        Vec::new()
                    });
                // Drop transcripts from the log side: the boot
                // wiring of `Tee([Broadcast, SqliteAgentEventLog])`
                // means the log captured TranscriptAppended too,
                // but the JSONL reader is the canonical source.
                let log_rows: Vec<AgentEventKind> = log_rows
                    .into_iter()
                    .filter(|e| {
                        !matches!(e, AgentEventKind::TranscriptAppended { .. })
                    })
                    .collect();
                let mut merged: Vec<AgentEventKind> =
                    Vec::with_capacity(transcripts.len() + log_rows.len());
                merged.extend(transcripts);
                merged.extend(log_rows);
                // Newest first; fall back to insertion order on
                // exact-tie timestamps (sort_by_key is stable).
                merged.sort_by_key(|e| std::cmp::Reverse(event_at_ms(e)));
                merged.truncate(limit);
                Ok(merged)
            }
        }
    }

    async fn read_session_events(
        &self,
        params: &AgentEventsReadParams,
    ) -> anyhow::Result<Vec<AgentEventKind>> {
        // session_id semantics live in the JSONL reader. The
        // durable log has no session_id (non-transcript kinds
        // don't carry one).
        self.transcripts.read_session_events(params).await
    }

    async fn search_events(
        &self,
        params: &AgentEventsSearchParams,
    ) -> anyhow::Result<Vec<SearchHit>> {
        // FTS5 is transcript-only today. Future revs may extend
        // search across non-transcript kinds; that lands as a new
        // `AgentEventLog::search` method.
        self.transcripts.search_events(params).await
    }
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
            tenant_id: None,
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

    // ── Phase 82.11.log.merge — `MergingAgentEventReader` tests ──

    use crate::agent::admin_rpc::SqliteAgentEventLog;
    use nexo_tool_meta::admin::escalations::{EscalationReason, EscalationUrgency};
    use nexo_tool_meta::admin::processing::{
        ProcessingControlState, ProcessingScope,
    };

    fn convo(agent: &str) -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: agent.into(),
            channel: "whatsapp".into(),
            account_id: "55-1234".into(),
            contact_id: "55-5678".into(),
            mcp_channel_source: None,
        }
    }

    fn transcript(agent: &str, sent_at_ms: u64) -> AgentEventKind {
        AgentEventKind::TranscriptAppended {
            agent_id: agent.into(),
            session_id: Uuid::nil(),
            seq: 0,
            role: TranscriptRole::User,
            body: "hola".into(),
            sent_at_ms,
            sender_id: None,
            source_plugin: "whatsapp".into(),
            tenant_id: None,
        }
    }

    fn pause_changed(agent: &str, at_ms: u64) -> AgentEventKind {
        AgentEventKind::ProcessingStateChanged {
            agent_id: agent.into(),
            scope: convo(agent),
            prev_state: ProcessingControlState::AgentActive,
            new_state: ProcessingControlState::PausedByOperator {
                scope: convo(agent),
                paused_at_ms: at_ms,
                operator_token_hash: "abcdef0123456789".into(),
                reason: None,
            },
            at_ms,
            tenant_id: None,
        }
    }

    fn escalation(agent: &str, requested_at_ms: u64) -> AgentEventKind {
        AgentEventKind::EscalationRequested {
            agent_id: agent.into(),
            scope: convo(agent),
            summary: "needs review".into(),
            reason: EscalationReason::UnknownQuery,
            urgency: EscalationUrgency::Normal,
            requested_at_ms,
            tenant_id: None,
        }
    }

    #[tokio::test]
    async fn merger_kind_none_interleaves_both_sources_by_at_ms_desc() {
        // transcripts: t=1000, t=3000
        let transcripts: Arc<dyn TranscriptReader> = Arc::new(
            MockReader::default()
                .with_event(transcript("ana", 1_000))
                .with_event(transcript("ana", 3_000)),
        );
        // log: pause at t=2000, escalation at t=4000
        let log = Arc::new(SqliteAgentEventLog::open_memory().await.unwrap());
        log.append(pause_changed("ana", 2_000)).await.unwrap();
        log.append(escalation("ana", 4_000)).await.unwrap();

        let merger = MergingAgentEventReader::new(transcripts, log);
        let out = merger
            .list_recent_events(&AgentEventsListFilter {
                agent_id: "ana".into(),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 4);
        let stamps: Vec<u64> = out.iter().map(event_at_ms).collect();
        assert_eq!(stamps, vec![4_000, 3_000, 2_000, 1_000]);
    }

    #[tokio::test]
    async fn merger_kind_transcript_appended_routes_to_transcripts_only() {
        let transcripts: Arc<dyn TranscriptReader> =
            Arc::new(MockReader::default().with_event(transcript("ana", 1_000)));
        let log = Arc::new(SqliteAgentEventLog::open_memory().await.unwrap());
        log.append(pause_changed("ana", 2_000)).await.unwrap();
        let merger = MergingAgentEventReader::new(transcripts, log);
        let out = merger
            .list_recent_events(&AgentEventsListFilter {
                agent_id: "ana".into(),
                kind: Some("transcript_appended".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(matches!(
            out[0],
            AgentEventKind::TranscriptAppended { .. }
        ));
    }

    #[tokio::test]
    async fn merger_kind_other_routes_to_log_only() {
        let transcripts: Arc<dyn TranscriptReader> = Arc::new(
            MockReader::default().with_event(transcript("ana", 1_000)),
        );
        let log = Arc::new(SqliteAgentEventLog::open_memory().await.unwrap());
        log.append(pause_changed("ana", 2_000)).await.unwrap();
        log.append(escalation("ana", 3_000)).await.unwrap();
        let merger = MergingAgentEventReader::new(transcripts, log);
        let out = merger
            .list_recent_events(&AgentEventsListFilter {
                agent_id: "ana".into(),
                kind: Some("processing_state_changed".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(matches!(
            out[0],
            AgentEventKind::ProcessingStateChanged { .. }
        ));
    }

    #[tokio::test]
    async fn merger_skips_log_transcripts_to_avoid_double_count() {
        // Boot wires the log as a Tee sink, so the log captured
        // TranscriptAppended too. The merger drops it on the log
        // side because the JSONL reader is canonical.
        let transcripts: Arc<dyn TranscriptReader> = Arc::new(
            MockReader::default().with_event(transcript("ana", 1_000)),
        );
        let log = Arc::new(SqliteAgentEventLog::open_memory().await.unwrap());
        log.append(transcript("ana", 1_000)).await.unwrap(); // duplicate via log
        log.append(pause_changed("ana", 2_000)).await.unwrap();
        let merger = MergingAgentEventReader::new(transcripts, log);
        let out = merger
            .list_recent_events(&AgentEventsListFilter {
                agent_id: "ana".into(),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        // Expect exactly 2: 1 transcript + 1 pause; the log's
        // TranscriptAppended copy is filtered out.
        assert_eq!(out.len(), 2);
        let n_transcripts = out
            .iter()
            .filter(|e| matches!(e, AgentEventKind::TranscriptAppended { .. }))
            .count();
        assert_eq!(n_transcripts, 1, "no duplicate transcripts");
    }

    #[tokio::test]
    async fn merger_truncates_to_limit_after_merging() {
        let transcripts: Arc<dyn TranscriptReader> = Arc::new(
            MockReader::default()
                .with_event(transcript("ana", 1_000))
                .with_event(transcript("ana", 2_000)),
        );
        let log = Arc::new(SqliteAgentEventLog::open_memory().await.unwrap());
        log.append(pause_changed("ana", 3_000)).await.unwrap();
        log.append(escalation("ana", 4_000)).await.unwrap();
        let merger = MergingAgentEventReader::new(transcripts, log);
        let out = merger
            .list_recent_events(&AgentEventsListFilter {
                agent_id: "ana".into(),
                limit: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        // Newest 2 wins.
        let stamps: Vec<u64> = out.iter().map(event_at_ms).collect();
        assert_eq!(stamps, vec![4_000, 3_000]);
    }

    #[tokio::test]
    async fn merger_passes_through_read_session_to_transcripts() {
        let transcripts: Arc<dyn TranscriptReader> = Arc::new(
            MockReader::default().with_event(transcript("ana", 1_000)),
        );
        let log = Arc::new(SqliteAgentEventLog::open_memory().await.unwrap());
        let merger = MergingAgentEventReader::new(transcripts, log);
        // session_id semantics live in transcripts; the read
        // path should not consult the log at all.
        let out = merger
            .read_session_events(&AgentEventsReadParams {
                agent_id: "ana".into(),
                session_id: Uuid::nil(),
                limit: 10,
                since_seq: None,
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
    }
}
