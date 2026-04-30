//! Dream progress watcher. Phase 80.1.
//!
//! Verbatim port of
//! `claude-code-leak/src/services/autoDream/autoDream.ts:281-313`
//! (`makeDreamProgressWatcher`).
//!
//! For each assistant message in the forked dream loop:
//! 1. Extract concatenated text from the message content.
//! 2. Count tool_use blocks → `tool_use_count`.
//! 3. For `FileEdit` / `FileWrite` tool calls, extract `file_path`.
//! 4. Canonicalize each path:
//!    - inside `memory_dir` → record in `files_touched`
//!    - outside `memory_dir` → record in `escapes` (post-fork audit
//!      defense-in-depth — leak doesn't have this layer, nexo
//!      addition per pillar Robusto)
//! 5. Append the turn + files to [`DreamRunStore`] (Phase 80.18).
//!
//! Skipping empty no-op turns is handled server-side by 80.18's
//! `append_turn` (mirrors leak `DreamTask.ts:87-92`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use nexo_agent_registry::{DreamRunStore, DreamTurn};
use nexo_fork::{tool_names, OnMessage};
use nexo_llm::types::{ChatMessage, ChatRole};
use uuid::Uuid;

/// Result of a fork run for the post-fork audit. Mirror leak `:281-313`
/// behavior + adds `escapes` for defense-in-depth (nexo addition).
#[derive(Debug, Clone, Default)]
pub struct ProgressResult {
    /// Paths whose canonical resolution lives inside `memory_dir`.
    /// Deduplicated by the underlying [`DreamRunStore::append_files_touched`].
    pub touched: Vec<PathBuf>,
    /// Paths whose canonical resolution escaped `memory_dir`. Non-empty
    /// = post-fork audit FAIL — caller should mark the run Failed
    /// even if the fork itself reported Completed (defense-in-depth).
    pub escapes: Vec<PathBuf>,
}

/// Watcher that consumes assistant messages from the fork's
/// `OnMessage` callback chain and records them in 80.18's audit store.
pub struct DreamProgressWatcher {
    run_id: Uuid,
    audit: Arc<dyn DreamRunStore>,
    memory_dir: PathBuf, // canonical
    state: Mutex<ProgressState>,
}

#[derive(Default)]
struct ProgressState {
    touched: Vec<PathBuf>,
    escapes: Vec<PathBuf>,
}

impl DreamProgressWatcher {
    pub fn new(
        run_id: Uuid,
        audit: Arc<dyn DreamRunStore>,
        memory_dir: PathBuf,
    ) -> Self {
        Self {
            run_id,
            audit,
            memory_dir,
            state: Mutex::new(ProgressState::default()),
        }
    }

    /// Snapshot the in-memory state for the post-fork audit. Returns
    /// the touched + escaped paths the watcher observed across the
    /// entire fork run. The audit store is the canonical source for
    /// `files_touched`; this is the watcher's local cache used for
    /// the fast path where the operator-facing system message reads
    /// the audit row.
    pub fn finish(&self) -> ProgressResult {
        let state = self.state.lock().unwrap();
        ProgressResult {
            touched: state.touched.clone(),
            escapes: state.escapes.clone(),
        }
    }
}

#[async_trait]
impl OnMessage for DreamProgressWatcher {
    async fn on_message(&self, msg: &ChatMessage) {
        // Only assistant turns matter. Mirror leak `:286`.
        if !matches!(msg.role, ChatRole::Assistant) {
            return;
        }

        let text = msg.content.trim().to_string();
        let tool_use_count = msg.tool_calls.len() as u32;

        // Collect FileEdit/FileWrite paths for audit + escape detection.
        let mut new_inside: Vec<PathBuf> = Vec::new();
        for tc in &msg.tool_calls {
            if !matches!(
                tc.name.as_str(),
                tool_names::FILE_EDIT | tool_names::FILE_WRITE
            ) {
                continue;
            }
            let raw_path = match tc.arguments.get("file_path").and_then(|v| v.as_str()) {
                Some(s) if !s.is_empty() => s,
                _ => continue,
            };
            let p = Path::new(raw_path);
            // Only canonicalize ABS paths; relative paths in fork
            // dispatcher are filtered upstream by AutoMemFilter.
            if !p.is_absolute() {
                continue;
            }
            let canonical = if p.exists() {
                match p.canonicalize() {
                    Ok(c) => c,
                    Err(_) => continue,
                }
            } else if let Some(parent) = p.parent() {
                match parent.canonicalize() {
                    Ok(c) => match p.file_name() {
                        Some(name) => c.join(name),
                        None => continue,
                    },
                    Err(_) => continue,
                }
            } else {
                continue;
            };

            let mut state = self.state.lock().unwrap();
            if canonical.starts_with(&self.memory_dir) {
                if !state.touched.contains(&canonical) {
                    state.touched.push(canonical.clone());
                    new_inside.push(canonical);
                }
            } else {
                if !state.escapes.contains(&canonical) {
                    state.escapes.push(canonical.clone());
                    tracing::error!(
                        target: "auto_dream.escape",
                        path = %canonical.display(),
                        memory_dir = %self.memory_dir.display(),
                        "fork attempted to edit path outside memory_dir"
                    );
                }
            }
        }

        // Append the turn (80.18 server-side skips empty no-ops).
        let turn = DreamTurn {
            text,
            tool_use_count,
        };
        if let Err(e) = self.audit.append_turn(self.run_id, &turn).await {
            tracing::warn!(
                target: "auto_dream.audit",
                error = %e,
                "append_turn failed"
            );
        }

        // Append new in-memdir paths (80.18 server-side dedupes).
        if !new_inside.is_empty() {
            if let Err(e) = self
                .audit
                .append_files_touched(self.run_id, &new_inside)
                .await
            {
                tracing::warn!(
                    target: "auto_dream.audit",
                    error = %e,
                    "append_files_touched failed"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_agent_registry::{DreamPhase, DreamRunRow, DreamRunStatus, SqliteDreamRunStore};
    use nexo_driver_types::GoalId;
    use nexo_llm::types::ToolCall;
    use serde_json::json;
    use std::fs;

    async fn mk_store() -> Arc<dyn DreamRunStore> {
        Arc::new(SqliteDreamRunStore::open_memory().await.unwrap())
    }

    fn mk_row(id: Uuid, goal_id: GoalId) -> DreamRunRow {
        DreamRunRow {
            id,
            goal_id,
            status: DreamRunStatus::Running,
            phase: DreamPhase::Starting,
            sessions_reviewing: 5,
            prior_mtime_ms: Some(0),
            files_touched: vec![],
            turns: vec![],
            started_at: chrono::Utc::now(),
            ended_at: None,
            fork_label: "auto_dream".into(),
            fork_run_id: None,
        }
    }

    fn assistant_msg_text(text: &str) -> ChatMessage {
        ChatMessage::assistant(text)
    }

    fn assistant_msg_tool_call(
        text: &str,
        tool: &str,
        file_path: &str,
    ) -> ChatMessage {
        ChatMessage::assistant_tool_calls(
            vec![ToolCall {
                id: "c1".into(),
                name: tool.into(),
                arguments: json!({"file_path": file_path}),
            }],
            text,
        )
    }

    #[tokio::test]
    async fn ignores_user_messages() {
        let store = mk_store().await;
        let run_id = Uuid::new_v4();
        store.insert(&mk_row(run_id, GoalId(Uuid::new_v4()))).await.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let watcher = DreamProgressWatcher::new(
            run_id,
            store.clone(),
            tmp.path().canonicalize().unwrap(),
        );

        watcher.on_message(&ChatMessage::user("hello")).await;
        let row = store.get(run_id).await.unwrap().unwrap();
        // No turn appended.
        assert_eq!(row.turns.len(), 0);
    }

    #[tokio::test]
    async fn appends_turn_with_text_only() {
        let store = mk_store().await;
        let run_id = Uuid::new_v4();
        store.insert(&mk_row(run_id, GoalId(Uuid::new_v4()))).await.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let watcher = DreamProgressWatcher::new(
            run_id,
            store.clone(),
            tmp.path().canonicalize().unwrap(),
        );

        watcher.on_message(&assistant_msg_text("thinking out loud")).await;
        let row = store.get(run_id).await.unwrap().unwrap();
        assert_eq!(row.turns.len(), 1);
        assert_eq!(row.turns[0].text, "thinking out loud");
        assert_eq!(row.turns[0].tool_use_count, 0);
    }

    #[tokio::test]
    async fn counts_tool_uses_in_turn() {
        let store = mk_store().await;
        let run_id = Uuid::new_v4();
        store.insert(&mk_row(run_id, GoalId(Uuid::new_v4()))).await.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let watcher = DreamProgressWatcher::new(
            run_id,
            store.clone(),
            tmp.path().canonicalize().unwrap(),
        );

        let msg = ChatMessage::assistant_tool_calls(
            vec![
                ToolCall {
                    id: "1".into(),
                    name: "Grep".into(),
                    arguments: json!({}),
                },
                ToolCall {
                    id: "2".into(),
                    name: "FileRead".into(),
                    arguments: json!({}),
                },
            ],
            "running tools",
        );
        watcher.on_message(&msg).await;
        let row = store.get(run_id).await.unwrap().unwrap();
        assert_eq!(row.turns[0].tool_use_count, 2);
    }

    #[tokio::test]
    async fn collects_file_edit_inside_memdir() {
        let store = mk_store().await;
        let run_id = Uuid::new_v4();
        store.insert(&mk_row(run_id, GoalId(Uuid::new_v4()))).await.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let memdir = tmp.path().canonicalize().unwrap();
        let inside = memdir.join("topic.md");
        fs::write(&inside, "x").unwrap();

        let watcher =
            DreamProgressWatcher::new(run_id, store.clone(), memdir.clone());

        let path_str = inside.to_string_lossy().into_owned();
        watcher
            .on_message(&assistant_msg_tool_call("editing", "FileEdit", &path_str))
            .await;

        let row = store.get(run_id).await.unwrap().unwrap();
        assert_eq!(row.files_touched.len(), 1);
        assert_eq!(row.files_touched[0], inside);

        // Local watcher state matches the audit row.
        let progress = watcher.finish();
        assert_eq!(progress.touched.len(), 1);
        assert!(progress.escapes.is_empty());
    }

    #[tokio::test]
    async fn flags_escape_outside_memdir() {
        let store = mk_store().await;
        let run_id = Uuid::new_v4();
        store.insert(&mk_row(run_id, GoalId(Uuid::new_v4()))).await.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let memdir = tmp.path().canonicalize().unwrap();
        // /tmp is outside memdir.
        let outside = std::env::temp_dir().canonicalize().unwrap().join("escape_test_dream.md");
        fs::write(&outside, "x").unwrap();

        let watcher = DreamProgressWatcher::new(run_id, store.clone(), memdir);

        let path_str = outside.to_string_lossy().into_owned();
        watcher
            .on_message(&assistant_msg_tool_call("editing", "FileWrite", &path_str))
            .await;

        let progress = watcher.finish();
        assert!(progress.touched.is_empty());
        assert_eq!(progress.escapes.len(), 1);

        // Audit should NOT have the escape recorded as files_touched.
        let row = store.get(run_id).await.unwrap().unwrap();
        assert_eq!(row.files_touched.len(), 0);

        let _ = fs::remove_file(&outside);
    }

    #[tokio::test]
    async fn ignores_non_edit_tools() {
        let store = mk_store().await;
        let run_id = Uuid::new_v4();
        store.insert(&mk_row(run_id, GoalId(Uuid::new_v4()))).await.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let memdir = tmp.path().canonicalize().unwrap();
        let watcher = DreamProgressWatcher::new(run_id, store.clone(), memdir);

        watcher
            .on_message(&assistant_msg_tool_call(
                "reading",
                "FileRead",
                "/etc/passwd", // would be flagged if FileEdit
            ))
            .await;

        let progress = watcher.finish();
        assert!(progress.touched.is_empty());
        assert!(progress.escapes.is_empty());
    }

    #[tokio::test]
    async fn dedupes_repeated_paths() {
        let store = mk_store().await;
        let run_id = Uuid::new_v4();
        store.insert(&mk_row(run_id, GoalId(Uuid::new_v4()))).await.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let memdir = tmp.path().canonicalize().unwrap();
        let inside = memdir.join("topic.md");
        fs::write(&inside, "x").unwrap();
        let path_str = inside.to_string_lossy().into_owned();

        let watcher =
            DreamProgressWatcher::new(run_id, store.clone(), memdir);
        watcher
            .on_message(&assistant_msg_tool_call("e1", "FileEdit", &path_str))
            .await;
        watcher
            .on_message(&assistant_msg_tool_call("e2", "FileEdit", &path_str))
            .await;

        let row = store.get(run_id).await.unwrap().unwrap();
        // Server-side dedupe in 80.18 keeps it at 1.
        assert_eq!(row.files_touched.len(), 1);
        // Watcher local state also dedupes.
        let progress = watcher.finish();
        assert_eq!(progress.touched.len(), 1);
        // But two turns appended.
        assert_eq!(row.turns.len(), 2);
    }
}
