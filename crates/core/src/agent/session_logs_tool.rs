use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use super::transcripts::{SessionHeader, TranscriptEntry, TranscriptLine, TranscriptWriter};
use super::transcripts_index::TranscriptsIndex;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;
const DEFAULT_MAX_CHARS: usize = 200;
/// LLM-facing tool that reads this agent's own JSONL transcripts. Useful for
/// self-reflection ("what did I tell her yesterday?"), debugging ("what did
/// the tool return in session X?"), and lightweight cross-session search.
///
/// The tool is scoped to `ctx.config.transcripts_dir` — it cannot see other
/// agents' transcripts. Current-session access is allowed by default; other
/// sessions are readable because they belong to the same agent owner.
pub struct SessionLogsTool {
    index: Option<Arc<TranscriptsIndex>>,
}
impl SessionLogsTool {
    pub fn new() -> Self {
        Self { index: None }
    }
    pub fn with_index(mut self, index: Arc<TranscriptsIndex>) -> Self {
        self.index = Some(index);
        self
    }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "session_logs".to_string(),
            description: "Inspect this agent's stored session transcripts. Actions: \
                list_sessions (summary of every recorded session), \
                read_session (all lines of one session, capped), \
                search (grep substring across sessions, case-insensitive), \
                recent (last N entries of a session, default current)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list_sessions", "read_session", "search", "recent"]
                    },
                    "session_id": { "type": "string", "description": "UUID of the session. For read_session/recent; optional on recent (defaults to current)." },
                    "query":      { "type": "string", "description": "Substring to match for search action (case-insensitive)." },
                    "limit":      { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": "Max entries/sessions to return." },
                    "max_chars":  { "type": "integer", "minimum": 20, "maximum": 4000, "description": "Truncate each content preview at this length (default 200)." }
                },
                "required": ["action"]
            }),
        }
    }
}
impl Default for SessionLogsTool {
    fn default() -> Self {
        Self::new()
    }
}
#[async_trait]
impl ToolHandler for SessionLogsTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let action = args["action"].as_str().unwrap_or("").trim();
        let transcripts_dir = ctx.config.transcripts_dir.trim();
        if transcripts_dir.is_empty() {
            return Ok(json!({
                "ok": false,
                "error": "transcripts_dir is not configured for this agent"
            }));
        }
        let root = PathBuf::from(transcripts_dir);
        let writer = TranscriptWriter::new(root.clone(), ctx.agent_id.clone());
        match action {
            "list_sessions" => {
                let limit = optional_usize(&args, "limit")?
                    .unwrap_or(DEFAULT_LIMIT)
                    .min(MAX_LIMIT);
                list_sessions(&writer, &root, limit).await
            }
            "read_session" => {
                let id = required_uuid(&args, "session_id")?;
                let limit = optional_usize(&args, "limit")?
                    .unwrap_or(DEFAULT_LIMIT)
                    .min(MAX_LIMIT);
                let max_chars = optional_usize(&args, "max_chars")?
                    .unwrap_or(DEFAULT_MAX_CHARS)
                    .clamp(20, 4000);
                read_session(&writer, id, limit, max_chars).await
            }
            "search" => {
                let query = required_nonempty_string(&args, "query")?;
                let limit = optional_usize(&args, "limit")?
                    .unwrap_or(DEFAULT_LIMIT)
                    .min(MAX_LIMIT);
                let max_chars = optional_usize(&args, "max_chars")?
                    .unwrap_or(DEFAULT_MAX_CHARS)
                    .clamp(20, 4000);
                if let Some(index) = self.index.as_ref() {
                    search_via_fts(index, &ctx.agent_id, &query, limit).await
                } else {
                    search_sessions(&writer, &root, &query, limit, max_chars).await
                }
            }
            "recent" => {
                let id = match optional_string(&args, "session_id") {
                    Some(s) => Uuid::parse_str(&s)
                        .map_err(|e| anyhow::anyhow!("`session_id` is not a valid UUID: {e}"))?,
                    None => ctx.session_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "recent action requires either `session_id` or a session-scoped context"
                        )
                    })?,
                };
                let limit = optional_usize(&args, "limit")?.unwrap_or(10).min(MAX_LIMIT);
                let max_chars = optional_usize(&args, "max_chars")?
                    .unwrap_or(DEFAULT_MAX_CHARS)
                    .clamp(20, 4000);
                let lines = writer.read_session(id).await?;
                let entries = entries_only(lines);
                let slice = tail(entries, limit);
                Ok(json!({
                    "ok": true,
                    "session_id": id.to_string(),
                    "count": slice.len(),
                    "entries": render_entries(&slice, max_chars),
                }))
            }
            other => Err(anyhow::anyhow!(
                "unknown action `{other}`; expected list_sessions|read_session|search|recent"
            )),
        }
    }
}
async fn list_sessions(
    writer: &TranscriptWriter,
    root: &Path,
    limit: usize,
) -> anyhow::Result<Value> {
    let mut rows: Vec<Value> = Vec::new();
    let mut entries = tokio::fs::read_dir(root)
        .await
        .map_err(|e| anyhow::anyhow!("cannot read transcripts_dir `{}`: {e}", root.display()))?;
    let mut files: Vec<PathBuf> = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    // Most-recently-modified first.
    files.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    files.reverse();
    files.truncate(limit);
    for path in &files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let Ok(uuid) = Uuid::parse_str(stem) else {
            continue;
        };
        let lines = writer.read_session(uuid).await.unwrap_or_default();
        let header = lines.iter().find_map(|l| match l {
            TranscriptLine::Session(h) => Some(h.clone()),
            _ => None,
        });
        let entry_count = lines
            .iter()
            .filter(|l| matches!(l, TranscriptLine::Entry(_)))
            .count();
        let first_ts = lines.iter().find_map(|l| match l {
            TranscriptLine::Entry(e) => Some(e.timestamp.to_rfc3339()),
            _ => None,
        });
        let last_ts = lines.iter().rev().find_map(|l| match l {
            TranscriptLine::Entry(e) => Some(e.timestamp.to_rfc3339()),
            _ => None,
        });
        rows.push(summary_row(&header, uuid, entry_count, first_ts, last_ts));
    }
    Ok(json!({
        "ok": true,
        "transcripts_dir": root.display().to_string(),
        "count": rows.len(),
        "sessions": rows,
    }))
}
async fn read_session(
    writer: &TranscriptWriter,
    session_id: Uuid,
    limit: usize,
    max_chars: usize,
) -> anyhow::Result<Value> {
    let lines = writer.read_session(session_id).await?;
    if lines.is_empty() {
        return Ok(json!({
            "ok": false,
            "error": "session_not_found",
            "session_id": session_id.to_string()
        }));
    }
    let header = lines.iter().find_map(|l| match l {
        TranscriptLine::Session(h) => Some(h.clone()),
        _ => None,
    });
    let entries = entries_only(lines);
    let total = entries.len();
    let truncated = total > limit;
    let slice: Vec<TranscriptEntry> = entries.into_iter().take(limit).collect();
    Ok(json!({
        "ok": true,
        "session_id": session_id.to_string(),
        "header": header.map(|h| json!({
            "agent_id": h.agent_id,
            "timestamp": h.timestamp.to_rfc3339(),
            "source_plugin": h.source_plugin,
            "version": h.version,
        })),
        "total_entries": total,
        "returned": slice.len(),
        "truncated": truncated,
        "entries": render_entries(&slice, max_chars),
    }))
}
async fn search_via_fts(
    index: &TranscriptsIndex,
    agent_id: &str,
    query: &str,
    limit: usize,
) -> anyhow::Result<Value> {
    let hits = index.search(agent_id, query, limit).await?;
    let rows: Vec<Value> = hits
        .into_iter()
        .map(|h| {
            let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(h.timestamp_unix, 0)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default();
            json!({
                "session_id": h.session_id.to_string(),
                "timestamp": ts,
                "role": h.role,
                "source_plugin": h.source_plugin,
                "preview": h.snippet,
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "query": query,
        "backend": "fts5",
        "count": rows.len(),
        "hits": rows,
    }))
}

async fn search_sessions(
    writer: &TranscriptWriter,
    root: &Path,
    query: &str,
    limit: usize,
    max_chars: usize,
) -> anyhow::Result<Value> {
    let needle = query.to_lowercase();
    let mut hits: Vec<Value> = Vec::new();
    let mut entries = tokio::fs::read_dir(root)
        .await
        .map_err(|e| anyhow::anyhow!("cannot read transcripts_dir `{}`: {e}", root.display()))?;
    let mut files: Vec<PathBuf> = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            files.push(p);
        }
    }
    files.sort();
    'outer: for path in &files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let Ok(uuid) = Uuid::parse_str(stem) else {
            continue;
        };
        let lines = writer.read_session(uuid).await.unwrap_or_default();
        for line in &lines {
            let TranscriptLine::Entry(e) = line else {
                continue;
            };
            if e.content.to_lowercase().contains(&needle) {
                hits.push(json!({
                    "session_id": uuid.to_string(),
                    "timestamp": e.timestamp.to_rfc3339(),
                    "role": format!("{:?}", e.role).to_lowercase(),
                    "source_plugin": e.source_plugin,
                    "preview": truncate_chars(&e.content, max_chars),
                }));
                if hits.len() >= limit {
                    break 'outer;
                }
            }
        }
    }
    Ok(json!({
        "ok": true,
        "query": query,
        "count": hits.len(),
        "hits": hits,
    }))
}
fn summary_row(
    header: &Option<SessionHeader>,
    id: Uuid,
    entry_count: usize,
    first_ts: Option<String>,
    last_ts: Option<String>,
) -> Value {
    json!({
        "session_id": id.to_string(),
        "agent_id": header.as_ref().map(|h| h.agent_id.clone()),
        "source_plugin": header.as_ref().map(|h| h.source_plugin.clone()),
        "entry_count": entry_count,
        "first_timestamp": first_ts,
        "last_timestamp": last_ts,
    })
}
fn entries_only(lines: Vec<TranscriptLine>) -> Vec<TranscriptEntry> {
    lines
        .into_iter()
        .filter_map(|l| match l {
            TranscriptLine::Entry(e) => Some(e),
            _ => None,
        })
        .collect()
}
fn tail(mut entries: Vec<TranscriptEntry>, n: usize) -> Vec<TranscriptEntry> {
    if entries.len() <= n {
        return entries;
    }
    let drop = entries.len() - n;
    entries.drain(..drop);
    entries
}
fn render_entries(entries: &[TranscriptEntry], max_chars: usize) -> Value {
    let out: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "timestamp": e.timestamp.to_rfc3339(),
                "role": format!("{:?}", e.role).to_lowercase(),
                "source_plugin": e.source_plugin,
                "message_id": e.message_id.map(|u| u.to_string()),
                "sender_id": e.sender_id,
                "content": truncate_chars(&e.content, max_chars),
                "truncated": e.content.chars().count() > max_chars,
            })
        })
        .collect();
    Value::Array(out)
}
fn truncate_chars(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}
fn required_nonempty_string(args: &Value, key: &str) -> anyhow::Result<String> {
    let s = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing or invalid `{key}`"))?
        .trim()
        .to_string();
    if s.is_empty() {
        anyhow::bail!("`{key}` cannot be empty");
    }
    Ok(s)
}
fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
fn required_uuid(args: &Value, key: &str) -> anyhow::Result<Uuid> {
    let s = required_nonempty_string(args, key)?;
    Uuid::parse_str(&s).map_err(|e| anyhow::anyhow!("`{key}` is not a valid UUID: {e}"))
}
fn optional_usize(args: &Value, key: &str) -> anyhow::Result<Option<usize>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(|n| Some(n as usize))
            .ok_or_else(|| anyhow::anyhow!("`{key}` must be a positive integer")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use chrono::Utc;
    use nexo_broker::{AnyBroker, BrokerHandle};
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig,
    };
    use std::sync::Arc;
    use std::time::Duration;
    async fn setup() -> (SessionLogsTool, AgentContext, PathBuf, Uuid) {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let broker = AnyBroker::local();
        let _ = broker.subscribe("_").await;
        let cfg = Arc::new(AgentConfig {
            id: "kate".into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m1".into(),
            },
            plugins: vec![],
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: vec![],
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: dir.to_string_lossy().to_string(),
            dreaming: Default::default(),
            workspace_git: Default::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            outbound_allowlist: Default::default(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
        repl: Default::default(),
            auto_dream: None,
            assistant_mode: None,
            away_summary: None,
            brief: None,
            channels: None,
            auto_approve: false,
            extract_memories: None,
            event_subscribers: Vec::new(),
        });
        let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
        let sid = Uuid::new_v4();
        let ctx = AgentContext::new("kate", cfg, broker, sessions).with_session_id(sid);
        (SessionLogsTool::new(), ctx, dir, sid)
    }
    async fn write_some_entries(
        root: &Path,
        agent_id: &str,
        session: Uuid,
        msgs: &[(TranscriptRole, &str)],
    ) {
        let w = TranscriptWriter::new(root, agent_id);
        for (role, content) in msgs {
            let entry = TranscriptEntry {
                timestamp: Utc::now(),
                role: *role,
                content: (*content).to_string(),
                message_id: None,
                source_plugin: "test".into(),
                sender_id: Some("user-1".into()),
            };
            w.append_entry(session, entry).await.unwrap();
        }
    }
    use super::super::transcripts::TranscriptRole;
    #[tokio::test]
    async fn list_sessions_returns_summaries() {
        let (tool, ctx, dir, _) = setup().await;
        let s1 = Uuid::new_v4();
        let s2 = Uuid::new_v4();
        write_some_entries(
            &dir,
            "kate",
            s1,
            &[
                (TranscriptRole::User, "hola"),
                (TranscriptRole::Assistant, "buenos dias"),
            ],
        )
        .await;
        write_some_entries(&dir, "kate", s2, &[(TranscriptRole::User, "que haces")]).await;
        let out = tool
            .call(&ctx, json!({ "action": "list_sessions" }))
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["count"], 2);
        let sessions = out["sessions"].as_array().unwrap();
        assert!(sessions
            .iter()
            .any(|s| s["session_id"].as_str().unwrap() == s1.to_string()));
        assert!(sessions
            .iter()
            .any(|s| s["session_id"].as_str().unwrap() == s2.to_string()));
    }
    #[tokio::test]
    async fn read_session_returns_entries_in_order() {
        let (tool, ctx, dir, _) = setup().await;
        let s = Uuid::new_v4();
        write_some_entries(
            &dir,
            "kate",
            s,
            &[
                (TranscriptRole::User, "first"),
                (TranscriptRole::Assistant, "second"),
                (TranscriptRole::User, "third"),
            ],
        )
        .await;
        let out = tool
            .call(
                &ctx,
                json!({ "action": "read_session", "session_id": s.to_string() }),
            )
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["total_entries"], 3);
        let entries = out["entries"].as_array().unwrap();
        assert_eq!(entries[0]["content"], "first");
        assert_eq!(entries[1]["role"], "assistant");
    }
    #[tokio::test]
    async fn read_session_missing_returns_error() {
        let (tool, ctx, _dir, _) = setup().await;
        let out = tool
            .call(
                &ctx,
                json!({ "action": "read_session", "session_id": Uuid::new_v4().to_string() }),
            )
            .await
            .unwrap();
        assert_eq!(out["ok"], false);
        assert_eq!(out["error"], "session_not_found");
    }
    #[tokio::test]
    async fn search_finds_case_insensitive_substring() {
        let (tool, ctx, dir, _) = setup().await;
        let s = Uuid::new_v4();
        write_some_entries(
            &dir,
            "kate",
            s,
            &[
                (TranscriptRole::User, "cuéntame sobre el CÓDIGO de Kate"),
                (TranscriptRole::Assistant, "no lo sé"),
            ],
        )
        .await;
        let out = tool
            .call(&ctx, json!({ "action": "search", "query": "código" }))
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["count"], 1);
        assert_eq!(out["hits"][0]["session_id"], s.to_string());
    }
    #[tokio::test]
    async fn recent_defaults_to_current_session() {
        let (tool, ctx, dir, sid) = setup().await;
        // Write entries into the current session.
        write_some_entries(
            &dir,
            "kate",
            sid,
            &[
                (TranscriptRole::User, "a"),
                (TranscriptRole::Assistant, "b"),
                (TranscriptRole::User, "c"),
                (TranscriptRole::Assistant, "d"),
            ],
        )
        .await;
        let out = tool
            .call(&ctx, json!({ "action": "recent", "limit": 2 }))
            .await
            .unwrap();
        assert_eq!(out["count"], 2);
        // tail(…, 2) → last two
        let entries = out["entries"].as_array().unwrap();
        assert_eq!(entries[0]["content"], "c");
        assert_eq!(entries[1]["content"], "d");
    }
    #[tokio::test]
    async fn truncates_long_content() {
        let (tool, ctx, dir, sid) = setup().await;
        let long = "a".repeat(1000);
        write_some_entries(&dir, "kate", sid, &[(TranscriptRole::User, long.as_str())]).await;
        let out = tool
            .call(
                &ctx,
                json!({ "action": "read_session", "session_id": sid.to_string(), "max_chars": 50 }),
            )
            .await
            .unwrap();
        let entries = out["entries"].as_array().unwrap();
        let content = entries[0]["content"].as_str().unwrap();
        assert_eq!(content.chars().count(), 51); // 50 + ellipsis
        assert_eq!(entries[0]["truncated"], true);
    }
    #[tokio::test]
    async fn missing_transcripts_dir_returns_ok_false() {
        let broker = AnyBroker::local();
        let _ = broker.subscribe("_").await;
        let cfg = Arc::new(AgentConfig {
            id: "kate".into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m1".into(),
            },
            plugins: vec![],
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: vec![],
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(), // empty!
            dreaming: Default::default(),
            workspace_git: Default::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            outbound_allowlist: Default::default(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
        repl: Default::default(),
            auto_dream: None,
            assistant_mode: None,
            away_summary: None,
            brief: None,
            channels: None,
            auto_approve: false,
            extract_memories: None,
            event_subscribers: Vec::new(),
        });
        let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
        let ctx = AgentContext::new("kate", cfg, broker, sessions);
        let tool = SessionLogsTool::new();
        let out = tool
            .call(&ctx, json!({ "action": "list_sessions" }))
            .await
            .unwrap();
        assert_eq!(out["ok"], false);
        assert!(out["error"].as_str().unwrap().contains("transcripts_dir"));
    }
    #[tokio::test]
    async fn unknown_action_errors() {
        let (tool, ctx, _, _) = setup().await;
        let err = tool
            .call(&ctx, json!({ "action": "frobnicate" }))
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("unknown action"));
    }

    #[tokio::test]
    async fn search_uses_fts_when_index_present() {
        let (_tool_unused, ctx, dir, sid) = setup().await;
        // Build index, write entries through the indexed writer.
        let idx_path = dir.join("idx.db");
        let index = std::sync::Arc::new(
            crate::agent::transcripts_index::TranscriptsIndex::open(&idx_path)
                .await
                .unwrap(),
        );
        let writer = TranscriptWriter::with_extras(
            &dir,
            "kate",
            std::sync::Arc::new(crate::agent::redaction::Redactor::disabled()),
            Some(index.clone()),
        );
        for (role, content) in [
            (TranscriptRole::User, "buscame el codigo secreto"),
            (TranscriptRole::Assistant, "no comparto eso"),
        ] {
            writer
                .append_entry(
                    sid,
                    TranscriptEntry {
                        timestamp: chrono::Utc::now(),
                        role,
                        content: content.into(),
                        message_id: None,
                        source_plugin: "wa".into(),
                        sender_id: None,
                    },
                )
                .await
                .unwrap();
        }
        let tool = SessionLogsTool::new().with_index(index);
        let out = tool
            .call(&ctx, json!({"action": "search", "query": "codigo"}))
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["backend"], "fts5");
        assert_eq!(out["count"], 1);
    }
}
