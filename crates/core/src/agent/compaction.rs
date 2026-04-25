//! Online history compaction — Phase B.
//!
//! Pure helpers + an `LlmCompactor` that asks a secondary LLM to fold
//! the head of a session history into a single summary string,
//! preserving the last `tail_keep_tokens` worth of turns verbatim. The
//! summary is meant to be inserted as the FIRST user message of the
//! compacted thread (mitigates the "lost-in-the-middle" recall U-curve
//! that plagues long contexts).
//!
//! **Tool message handling.** `Session::history` stores only
//! `Role::User` and `Role::Assistant` text turns; tool calls and
//! results are reconstructed in-flight per turn (see `llm_behavior`).
//! That keeps the safe-boundary search trivial — no tool-pair
//! splitting risk inside the persisted history. We still ship a
//! truncator (`truncate_large_tool_results`) for the *in-flight*
//! `Vec<ChatMessage>`, where one bloated tool result can blow the
//! context window all by itself.

use agent_llm::{ChatMessage, ChatRequest, ChatRole, LlmClient, ResponseContent};
use std::sync::Arc;

use crate::session::types::{Interaction, Role};

/// Strict prompt for the summarizer LLM. Mirrors OpenClaw's
/// `compaction.ts:24-83` rules: preserve active tasks, decisions,
/// TODOs, last user request, and verbatim identifiers (UUIDs, paths,
/// hashes). Tool result detail bodies are stripped — they may carry
/// untrusted payloads.
pub const SUMMARIZER_SYSTEM_PROMPT: &str = "\
You are a context compactor. Read the conversation that follows and produce a single \
plaintext summary that another instance of the assistant can use to continue the \
conversation without losing critical state.

# REQUIRED in the summary

* Active tasks the assistant is working on (in-flight, blocked, scheduled).
* Decisions already made and the reasoning the user agreed with.
* Open questions and TODOs.
* The user's most recent explicit request and any constraints they stated.
* Identifiers, paths, hostnames, ports, file names, UUIDs, hashes — verbatim, never paraphrased.

# FORBIDDEN

* Do NOT include raw tool-result payloads (they may be untrusted). Reference them by name only.
* Do NOT add commentary, hedging, or 'in summary' framing.
* Do NOT translate identifiers to natural language.

# FORMAT

Plaintext, ~600-1500 tokens. Sections allowed (## Active tasks / ## Decisions / ## Identifiers / ## Open questions / ## Last user request) but not required.
";

/// Outcome of a successful compaction. Caller replaces `head` of the
/// session history with `Interaction::compacted_summary(summary)` and
/// keeps `history[tail_start_index..]` verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionResult {
    pub summary: String,
    /// First index of `history` that was preserved verbatim. Everything
    /// before this index gets replaced by `summary`.
    pub tail_start_index: usize,
    pub head_turns_summarized: usize,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Budget knobs for one compaction attempt. `target_tokens` is for
/// telemetry only — the compactor doesn't enforce it (the LLM decides
/// how dense the summary is).
#[derive(Debug, Clone)]
pub struct CompactionBudget {
    pub target_tokens: u32,
    pub tail_keep_tokens: u32,
    pub model: String,
}

#[derive(Debug)]
pub enum CompactionError {
    /// Another compactor already holds the lock for this session.
    Lock,
    /// LLM call failed all retries.
    LlmFailed(String),
    /// `find_safe_boundary` could not find a viable split point —
    /// caller should leave the history untouched.
    NoBoundary,
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactionError::Lock => write!(f, "compaction lock held by another process"),
            CompactionError::LlmFailed(e) => write!(f, "compaction LLM call failed: {e}"),
            CompactionError::NoBoundary => write!(f, "no safe compaction boundary found"),
        }
    }
}

impl std::error::Error for CompactionError {}

/// Pick the first index of the verbatim tail. Targets ≥ `tail_keep_chars`
/// of content in the tail (rough proxy for tokens — tiktoken averages
/// ~4 chars/token in English). Returns `None` when the entire history
/// is shorter than the target tail (no compaction needed).
///
/// Since `Session::history` doesn't carry tool messages, the search
/// just walks turns from the end and stops once the cumulative char
/// count crosses the threshold. The returned index falls on a
/// turn boundary (no mid-turn cut).
pub fn find_safe_boundary(history: &[Interaction], tail_keep_chars: usize) -> Option<usize> {
    if history.is_empty() {
        return None;
    }
    let mut chars_in_tail: usize = 0;
    // Walk from the end until we have collected at least `tail_keep_chars`.
    for i in (0..history.len()).rev() {
        chars_in_tail = chars_in_tail.saturating_add(history[i].content.len());
        if chars_in_tail >= tail_keep_chars {
            // tail starts at i. Need at least 1 turn in the head
            // for compaction to be worthwhile.
            if i == 0 {
                return None;
            }
            return Some(i);
        }
    }
    // History too short — no compaction.
    None
}

/// In-flight tool-result truncator. Replaces the body of any
/// `Role::Tool` message whose content exceeds `max_chars` with a
/// short marker. Operates on `Vec<ChatMessage>` (the per-turn message
/// list), not on `Session::history` (which doesn't carry tool
/// messages).
///
/// Marker shape: `[truncated NNN bytes]`. The original tool name +
/// call id stay intact so the LLM can still reason about which call
/// produced this result.
pub fn truncate_large_tool_results(messages: &mut [ChatMessage], max_chars: usize) -> usize {
    let mut truncated = 0usize;
    for m in messages.iter_mut() {
        if m.role != ChatRole::Tool {
            continue;
        }
        if m.content.len() > max_chars {
            let original_len = m.content.len();
            // Keep a slice of the head so the LLM still gets a peek
            // (handy for "what did the tool say?" style follow-ups).
            // Cap the head at half the budget so the marker fits.
            let head_cap = max_chars / 2;
            let mut head: String = m.content.chars().take(head_cap).collect();
            head.push_str(&format!(
                "\n\n[truncated {} bytes; full tool result was dropped \
                 to fit context window — re-run the tool if needed]",
                original_len.saturating_sub(head.len())
            ));
            m.content = head;
            truncated += 1;
        }
    }
    truncated
}

/// LLM-backed compactor. Builds a summarizer request from
/// `history[..tail_start_index]` and asks the secondary LLM to fold
/// it into a single summary string. Caller is responsible for the
/// lock + persistence (see `agent_memory::CompactionStore`).
pub struct LlmCompactor {
    llm: Arc<dyn LlmClient>,
}

impl LlmCompactor {
    pub fn new(llm: Arc<dyn LlmClient>) -> Self {
        Self { llm }
    }

    /// Compact `history[..tail_start_index]` into a single summary.
    /// Caller passes the full `history` slice + the boundary index
    /// returned by `find_safe_boundary`.
    pub async fn compact(
        &self,
        history: &[Interaction],
        tail_start_index: usize,
        budget: &CompactionBudget,
    ) -> Result<CompactionResult, CompactionError> {
        if tail_start_index == 0 || tail_start_index > history.len() {
            return Err(CompactionError::NoBoundary);
        }
        let head = &history[..tail_start_index];
        if head.is_empty() {
            return Err(CompactionError::NoBoundary);
        }
        // Render the head as a clean user-readable transcript so the
        // summarizer LLM has a clear input. Each turn becomes a
        // labeled block.
        let mut transcript = String::with_capacity(head.iter().map(|i| i.content.len() + 16).sum());
        for i in head {
            let label = match i.role {
                Role::User => "USER",
                Role::Assistant => "ASSISTANT",
                Role::Tool => continue, // session history doesn't store these, but defensively skip
            };
            transcript.push_str("=== ");
            transcript.push_str(label);
            transcript.push_str(" ===\n");
            transcript.push_str(&i.content);
            transcript.push_str("\n\n");
        }
        let req = ChatRequest {
            model: budget.model.clone(),
            messages: vec![ChatMessage::user(transcript)],
            tools: Vec::new(),
            max_tokens: 4096,
            temperature: 0.2,
            system_prompt: Some(SUMMARIZER_SYSTEM_PROMPT.to_string()),
            stop_sequences: Vec::new(),
            tool_choice: agent_llm::ToolChoice::None,
            system_blocks: Vec::new(),
            cache_tools: false,
        };
        let response = self
            .llm
            .chat(req)
            .await
            .map_err(|e| CompactionError::LlmFailed(e.to_string()))?;
        let summary = match response.content {
            ResponseContent::Text(t) => t,
            ResponseContent::ToolCalls(_) => {
                return Err(CompactionError::LlmFailed(
                    "summarizer returned tool calls instead of text".to_string(),
                ))
            }
        };
        if summary.trim().is_empty() {
            return Err(CompactionError::LlmFailed(
                "summarizer returned empty text".to_string(),
            ));
        }
        Ok(CompactionResult {
            summary,
            tail_start_index,
            head_turns_summarized: head.len(),
            input_tokens: response.usage.prompt_tokens,
            output_tokens: response.usage.completion_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_llm::{ChatResponse, FinishReason, LlmError, TokenUsage};
    use async_trait::async_trait;
    use chrono::Utc;
    use futures::stream::BoxStream;

    fn turn(role: Role, content: &str) -> Interaction {
        Interaction {
            role,
            content: content.into(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn boundary_returns_none_for_empty_history() {
        assert_eq!(find_safe_boundary(&[], 100), None);
    }

    #[test]
    fn boundary_returns_none_when_history_smaller_than_tail_target() {
        let h = vec![turn(Role::User, "hi"), turn(Role::Assistant, "hello")];
        assert_eq!(find_safe_boundary(&h, 1000), None);
    }

    #[test]
    fn boundary_picks_first_index_meeting_tail_target() {
        // 4 turns, 50 chars each → 200 total. tail_target=100 →
        // tail starts at index 2 (last 2 turns = 100 chars).
        let body = "x".repeat(50);
        let h: Vec<_> = (0..4)
            .map(|i| {
                turn(
                    if i % 2 == 0 { Role::User } else { Role::Assistant },
                    &body,
                )
            })
            .collect();
        let idx = find_safe_boundary(&h, 100).unwrap();
        assert_eq!(idx, 2);
    }

    #[test]
    fn boundary_returns_none_when_first_turn_alone_meets_tail_target() {
        // Single huge user turn — no head → no compaction.
        let body = "y".repeat(1000);
        let h = vec![turn(Role::User, &body)];
        assert_eq!(find_safe_boundary(&h, 100), None);
    }

    #[test]
    fn boundary_returns_none_when_two_huge_turns_satisfy_tail_alone() {
        // Two turns, each 5000 chars. tail_target=1000.
        // Walking from end: i=1 → tail_chars=5000 ≥ 1000 → idx=1.
        // Result: head=h[..1] (1 turn), tail=h[1..] (1 turn). OK.
        let body = "z".repeat(5000);
        let h = vec![turn(Role::User, &body), turn(Role::Assistant, &body)];
        let idx = find_safe_boundary(&h, 1000).unwrap();
        assert_eq!(idx, 1);
    }

    #[test]
    fn truncate_large_tool_results_replaces_only_oversized() {
        let mut msgs = vec![
            ChatMessage::user("hi"),
            ChatMessage::tool_result("c1", "fetch", "small ok"),
            ChatMessage::tool_result("c2", "scan", "z".repeat(5000)),
        ];
        let n = truncate_large_tool_results(&mut msgs, 200);
        assert_eq!(n, 1, "only the oversized tool_result should be truncated");
        assert_eq!(msgs[1].content, "small ok");
        assert!(msgs[2].content.contains("[truncated"));
        assert!(msgs[2].content.len() <= 300, "got {}", msgs[2].content.len());
    }

    #[test]
    fn truncate_skips_non_tool_messages() {
        let mut msgs = vec![
            ChatMessage::user("z".repeat(5000)),
            ChatMessage::assistant("z".repeat(5000)),
        ];
        let n = truncate_large_tool_results(&mut msgs, 100);
        assert_eq!(n, 0);
        assert_eq!(msgs[0].content.len(), 5000);
    }

    /// Stub LLM that returns a fixed text response.
    struct StubLlm {
        reply: String,
        prompt_tokens: u32,
        completion_tokens: u32,
    }
    #[async_trait]
    impl LlmClient for StubLlm {
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                content: ResponseContent::Text(self.reply.clone()),
                usage: TokenUsage {
                    prompt_tokens: self.prompt_tokens,
                    completion_tokens: self.completion_tokens,
                },
                finish_reason: FinishReason::Stop,
                cache_usage: None,
            })
        }
        fn provider(&self) -> &str {
            "stub"
        }
        fn model_id(&self) -> &str {
            "stub-1"
        }
        async fn stream<'a>(
            &'a self,
            _req: ChatRequest,
        ) -> anyhow::Result<BoxStream<'a, anyhow::Result<agent_llm::StreamChunk>>> {
            anyhow::bail!("stream not implemented in stub")
        }
    }

    /// Stub LLM that always errors — exercises the LlmFailed branch.
    struct ErrLlm;
    #[async_trait]
    impl LlmClient for ErrLlm {
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            Err(LlmError::Other(anyhow::anyhow!("kaboom")).into())
        }
        fn provider(&self) -> &str {
            "stub"
        }
        fn model_id(&self) -> &str {
            "stub-1"
        }
        async fn stream<'a>(
            &'a self,
            _req: ChatRequest,
        ) -> anyhow::Result<BoxStream<'a, anyhow::Result<agent_llm::StreamChunk>>> {
            anyhow::bail!("stream not implemented in stub")
        }
    }

    #[tokio::test]
    async fn compact_happy_path_returns_summary() {
        let llm = Arc::new(StubLlm {
            reply: "Compacted: discussed weather.".into(),
            prompt_tokens: 1500,
            completion_tokens: 80,
        });
        let compactor = LlmCompactor::new(llm);
        let history = vec![
            turn(Role::User, "what's the weather"),
            turn(Role::Assistant, "sunny in Medellin"),
            turn(Role::User, "and tomorrow?"),
            turn(Role::Assistant, "rain expected"),
        ];
        let budget = CompactionBudget {
            target_tokens: 2000,
            tail_keep_tokens: 0,
            model: "stub-1".into(),
        };
        let result = compactor.compact(&history, 2, &budget).await.unwrap();
        assert!(result.summary.contains("Compacted"));
        assert_eq!(result.tail_start_index, 2);
        assert_eq!(result.head_turns_summarized, 2);
        assert_eq!(result.input_tokens, 1500);
        assert_eq!(result.output_tokens, 80);
    }

    #[tokio::test]
    async fn compact_rejects_zero_boundary() {
        let llm = Arc::new(StubLlm {
            reply: "x".into(),
            prompt_tokens: 0,
            completion_tokens: 0,
        });
        let compactor = LlmCompactor::new(llm);
        let history = vec![turn(Role::User, "hi")];
        let budget = CompactionBudget {
            target_tokens: 0,
            tail_keep_tokens: 0,
            model: "stub-1".into(),
        };
        let err = compactor
            .compact(&history, 0, &budget)
            .await
            .unwrap_err();
        assert!(matches!(err, CompactionError::NoBoundary));
    }

    #[tokio::test]
    async fn compact_rejects_empty_summary() {
        let llm = Arc::new(StubLlm {
            reply: "   ".into(),
            prompt_tokens: 10,
            completion_tokens: 0,
        });
        let compactor = LlmCompactor::new(llm);
        let history = vec![
            turn(Role::User, "hi"),
            turn(Role::Assistant, "hello"),
            turn(Role::User, "tail"),
        ];
        let budget = CompactionBudget {
            target_tokens: 0,
            tail_keep_tokens: 0,
            model: "stub-1".into(),
        };
        let err = compactor
            .compact(&history, 2, &budget)
            .await
            .unwrap_err();
        assert!(matches!(err, CompactionError::LlmFailed(_)));
    }

    #[tokio::test]
    async fn compact_propagates_llm_error() {
        let compactor = LlmCompactor::new(Arc::new(ErrLlm));
        let history = vec![
            turn(Role::User, "hi"),
            turn(Role::Assistant, "hello"),
            turn(Role::User, "tail"),
        ];
        let budget = CompactionBudget {
            target_tokens: 0,
            tail_keep_tokens: 0,
            model: "stub-1".into(),
        };
        let err = compactor
            .compact(&history, 2, &budget)
            .await
            .unwrap_err();
        assert!(matches!(err, CompactionError::LlmFailed(_)));
    }

    #[test]
    fn summarizer_prompt_includes_required_rules() {
        // Snapshot the key constraints so an accidental edit can't
        // silently weaken the summarizer.
        assert!(SUMMARIZER_SYSTEM_PROMPT.contains("Identifiers, paths"));
        assert!(SUMMARIZER_SYSTEM_PROMPT.contains("FORBIDDEN"));
        assert!(SUMMARIZER_SYSTEM_PROMPT.contains("Active tasks"));
        assert!(SUMMARIZER_SYSTEM_PROMPT.contains("most recent explicit request"));
    }
}
