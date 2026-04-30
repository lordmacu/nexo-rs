//! `ForkHandle` + `ForkResult` — public outcome types of a fork.
//! Step 80.19 / 8.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
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
