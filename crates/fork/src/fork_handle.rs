//! `ForkHandle` + `ForkResult` — public outcome types of a fork.
//! Step 80.19 / 8.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use nexo_driver_types::{TaskNotification, TaskStatus, TaskUsage};
use nexo_llm::types::{CacheUsage, ChatMessage, ChatRole, TokenUsage};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::ForkError;
use crate::turn_loop::TurnLoopResult;

/// Live handle to a running fork. Returned by
/// [`crate::ForkSubagent::fork`].
///
/// Drop semantics: if [`take_completion`](Self::take_completion) was
/// never called (caller dropped the handle without consuming the
/// future), the `Drop` impl signals `abort` so the fire-and-forget
/// tokio task gets cleaned up rather than running to completion in
/// the background.
pub struct ForkHandle {
    pub run_id: Uuid,
    /// `Some(goal_id)` only when `skip_transcript: false` AND the
    /// registry was wired. Otherwise `None`.
    pub goal_id: Option<Uuid>,
    /// Future that resolves to the fork's outcome. Wrapped in `Option`
    /// so [`take_completion`](Self::take_completion) can move it out
    /// despite the `Drop` impl. Once taken, drops are no-ops.
    completion: Option<BoxFuture<'static, Result<ForkResult, ForkError>>>,
    /// External abort signal — cancel from outside.
    pub abort: CancellationToken,
    /// Mirrors `completion.is_none()` but stays cheap to read from
    /// `Drop` without having to inspect the moved-out option.
    consumed: Arc<AtomicBool>,
}

impl ForkHandle {
    /// Build a new handle. Internal — public callers go through
    /// `ForkSubagent::fork`.
    pub fn new(
        run_id: Uuid,
        goal_id: Option<Uuid>,
        completion: BoxFuture<'static, Result<ForkResult, ForkError>>,
        abort: CancellationToken,
    ) -> Self {
        Self {
            run_id,
            goal_id,
            completion: Some(completion),
            abort,
            consumed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Take the completion future, marking the handle consumed. Calling
    /// twice returns `None` the second time. Awaiting the returned
    /// future drives the fork to completion.
    pub fn take_completion(
        &mut self,
    ) -> Option<BoxFuture<'static, Result<ForkResult, ForkError>>> {
        self.consumed.store(true, Ordering::Release);
        self.completion.take()
    }

    /// True after the completion future has been extracted (or after
    /// [`mark_consumed`](Self::mark_consumed) explicitly).
    pub fn is_consumed(&self) -> bool {
        self.consumed.load(Ordering::Acquire)
    }

    /// Mark consumed without taking the future. Tests that verify drop
    /// semantics use this; production callers go through
    /// [`take_completion`](Self::take_completion).
    pub fn mark_consumed(&self) {
        self.consumed.store(true, Ordering::Release);
    }
}

impl Drop for ForkHandle {
    fn drop(&mut self) {
        // If the completion was never consumed, cancel the loop so
        // tokio::spawn'd ForkAndForget tasks don't leak.
        if !self.consumed.load(Ordering::Acquire) && !self.abort.is_cancelled() {
            self.abort.cancel();
        }
    }
}

/// Fork outcome — produced by the `completion` future.
#[derive(Debug)]
pub struct ForkResult {
    pub messages: Vec<ChatMessage>,
    pub total_usage: TokenUsage,
    pub total_cache_usage: CacheUsage,
    /// Last assistant message text, when present.
    pub final_text: Option<String>,
    pub turns_executed: u32,
}

impl ForkResult {
    /// Lift a [`TurnLoopResult`] into a public [`ForkResult`].
    pub fn from_turn_loop(r: TurnLoopResult) -> Self {
        Self {
            messages: r.messages,
            total_usage: r.total_usage,
            total_cache_usage: r.total_cache_usage,
            final_text: r.final_text,
            turns_executed: r.turns_executed,
        }
    }

    /// Extract the last assistant text from a message list. Mirror of
    /// `extractResultText` (leak `forkedAgent.ts:237-258`).
    pub fn extract_final_text(messages: &[ChatMessage]) -> Option<String> {
        messages.iter().rev().find_map(|m| {
            if matches!(m.role, ChatRole::Assistant) && !m.content.is_empty() {
                Some(m.content.clone())
            } else {
                None
            }
        })
    }

    /// Phase 84.2 — render this fork outcome as a
    /// [`TaskNotification`] so the coordinator's session sees a
    /// canonical `<task-notification>` envelope (Phase 84.2.1)
    /// instead of free-form text.
    ///
    /// `task_id` is the worker's stable id (the `goal_id` from
    /// `ForkHandle.goal_id` when the fork was registered, or any
    /// caller-chosen handle when the fork was unregistered).
    /// `summary` is a one-line synthesis from the consumer; if
    /// omitted, the helper auto-derives one from the fork's first
    /// 80 chars of `final_text` (or `"completed"` when empty).
    /// `duration_ms` is wall-clock time the consumer measured;
    /// the fork loop itself does not track wall time.
    pub fn to_task_notification(
        &self,
        task_id: impl Into<String>,
        summary: Option<String>,
        duration_ms: u64,
    ) -> TaskNotification {
        let auto_summary = || {
            self.final_text
                .as_deref()
                .map(|t| {
                    t.chars()
                        .take_while(|c| *c != '\n')
                        .take(80)
                        .collect::<String>()
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "completed".to_string())
        };
        TaskNotification {
            task_id: task_id.into(),
            status: TaskStatus::Completed,
            summary: summary.unwrap_or_else(auto_summary),
            result: self.final_text.clone(),
            usage: Some(TaskUsage {
                total_tokens: self.total_usage.prompt_tokens as u64
                    + self.total_usage.completion_tokens as u64,
                tool_uses: self.turns_executed as u64,
                duration_ms,
            }),
        }
    }
}

/// Phase 84.2 — render a [`ForkError`] as a failure-shaped
/// [`TaskNotification`].
///
/// Maps abort-token cancellation to [`TaskStatus::Killed`] and
/// budget breaches to [`TaskStatus::Timeout`]. All other errors
/// resolve to [`TaskStatus::Failed`]. The error's `Display` text
/// becomes the summary (XML-escaped at render time, so embedded
/// `<`/`>`/`&` round-trip safely).
pub fn fork_error_to_task_notification(
    err: &ForkError,
    task_id: impl Into<String>,
    duration_ms: u64,
) -> TaskNotification {
    let status = match err {
        ForkError::Aborted => TaskStatus::Killed,
        ForkError::Timeout(_) => TaskStatus::Timeout,
        _ => TaskStatus::Failed,
    };
    let usage = if duration_ms > 0 {
        Some(TaskUsage {
            total_tokens: 0,
            tool_uses: 0,
            duration_ms,
        })
    } else {
        None
    };
    TaskNotification {
        task_id: task_id.into(),
        status,
        summary: err.to_string(),
        result: None,
        usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_future() -> BoxFuture<'static, Result<ForkResult, ForkError>> {
        Box::pin(async {
            Ok(ForkResult {
                messages: vec![],
                total_usage: TokenUsage::default(),
                total_cache_usage: CacheUsage::default(),
                final_text: Some("done".into()),
                turns_executed: 1,
            })
        })
    }

    #[tokio::test]
    async fn take_completion_then_await_succeeds() {
        let mut handle = ForkHandle::new(
            Uuid::new_v4(),
            None,
            ok_future(),
            CancellationToken::new(),
        );
        let fut = handle.take_completion().expect("first take returns Some");
        let result = fut.await.unwrap();
        assert_eq!(result.final_text.as_deref(), Some("done"));
        assert!(handle.is_consumed());
        // Second take returns None.
        assert!(handle.take_completion().is_none());
    }

    #[test]
    fn extract_final_text_returns_last_assistant() {
        let messages = vec![
            ChatMessage::user("hi"),
            ChatMessage::assistant("first"),
            ChatMessage::user("more"),
            ChatMessage::assistant("second"),
        ];
        assert_eq!(
            ForkResult::extract_final_text(&messages).as_deref(),
            Some("second")
        );
    }

    #[test]
    fn extract_final_text_returns_none_when_no_assistant() {
        let messages = vec![ChatMessage::user("hi")];
        assert!(ForkResult::extract_final_text(&messages).is_none());
    }

    #[test]
    fn extract_final_text_skips_empty_assistant() {
        let messages = vec![
            ChatMessage::assistant("hello"),
            ChatMessage::assistant(String::new()),
        ];
        assert_eq!(
            ForkResult::extract_final_text(&messages).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn drop_without_consume_cancels_abort() {
        let abort = CancellationToken::new();
        {
            let _handle = ForkHandle::new(
                Uuid::new_v4(),
                None,
                ok_future(),
                abort.clone(),
            );
            // not awaited, not consumed
        }
        assert!(
            abort.is_cancelled(),
            "drop must cancel abort when completion was never consumed"
        );
    }

    fn fork_result_with(final_text: Option<&str>, prompt: u32, completion: u32, turns: u32) -> ForkResult {
        ForkResult {
            messages: vec![],
            total_usage: TokenUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
            },
            total_cache_usage: CacheUsage::default(),
            final_text: final_text.map(str::to_string),
            turns_executed: turns,
        }
    }

    #[test]
    fn to_task_notification_completed_with_explicit_summary() {
        let r = fork_result_with(Some("Found bug in auth.rs:142"), 800, 200, 5);
        let n = r.to_task_notification("goal-x", Some("explicit summary".into()), 12_000);
        assert_eq!(n.task_id, "goal-x");
        assert_eq!(n.status, TaskStatus::Completed);
        assert_eq!(n.summary, "explicit summary");
        assert_eq!(n.result.as_deref(), Some("Found bug in auth.rs:142"));
        let u = n.usage.expect("usage present");
        assert_eq!(u.total_tokens, 1_000);
        assert_eq!(u.tool_uses, 5);
        assert_eq!(u.duration_ms, 12_000);
    }

    #[test]
    fn to_task_notification_auto_summary_uses_first_line() {
        let r = fork_result_with(
            Some("Header line first\nBody line second"),
            0,
            0,
            1,
        );
        let n = r.to_task_notification("g", None, 0);
        assert_eq!(n.summary, "Header line first");
    }

    #[test]
    fn to_task_notification_auto_summary_caps_at_80_chars() {
        let long = "x".repeat(200);
        let r = fork_result_with(Some(&long), 0, 0, 1);
        let n = r.to_task_notification("g", None, 0);
        assert_eq!(n.summary.chars().count(), 80);
    }

    #[test]
    fn to_task_notification_falls_back_when_no_final_text() {
        let r = fork_result_with(None, 0, 0, 1);
        let n = r.to_task_notification("g", None, 0);
        assert_eq!(n.summary, "completed");
        assert!(n.result.is_none());
    }

    #[test]
    fn fork_error_aborted_maps_to_killed() {
        let err = ForkError::Aborted;
        let n = fork_error_to_task_notification(&err, "g", 500);
        assert_eq!(n.status, TaskStatus::Killed);
        assert_eq!(n.summary, "Aborted by caller");
        assert!(n.result.is_none());
    }

    #[test]
    fn fork_error_timeout_maps_to_timeout() {
        let err = ForkError::Timeout(std::time::Duration::from_secs(60));
        let n = fork_error_to_task_notification(&err, "g", 60_000);
        assert_eq!(n.status, TaskStatus::Timeout);
        let u = n.usage.expect("usage when duration > 0");
        assert_eq!(u.duration_ms, 60_000);
    }

    #[test]
    fn fork_error_other_maps_to_failed() {
        let err = ForkError::Llm("network".into());
        let n = fork_error_to_task_notification(&err, "g", 0);
        assert_eq!(n.status, TaskStatus::Failed);
        // duration_ms == 0 collapses usage entirely.
        assert!(n.usage.is_none());
    }

    #[test]
    fn to_task_notification_renders_clean_xml() {
        // End-to-end: ForkResult → TaskNotification → XML.
        let r = fork_result_with(Some("if a < b && c > d {}"), 100, 50, 2);
        let n = r.to_task_notification("g-xml", None, 0);
        let xml = n.to_xml();
        assert!(xml.contains("<status>completed</status>"));
        assert!(xml.contains("&lt;"));
        assert!(xml.contains("&amp;"));
        // Round-trip safety.
        let parsed = TaskNotification::parse_block(&xml).expect("parse");
        assert_eq!(parsed.result.as_deref(), Some("if a < b && c > d {}"));
    }

    #[test]
    fn drop_after_consume_does_not_cancel_abort() {
        let abort = CancellationToken::new();
        {
            let handle = ForkHandle::new(
                Uuid::new_v4(),
                None,
                ok_future(),
                abort.clone(),
            );
            handle.mark_consumed();
        }
        assert!(
            !abort.is_cancelled(),
            "drop after mark_consumed must NOT cancel abort"
        );
    }
}
