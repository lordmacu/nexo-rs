//! Offline tiktoken-based token approximation.
//!
//! Uses `cl100k_base` (the GPT-4 / GPT-3.5 encoding). Drift vs Anthropic
//! and Gemini real billing has been measured at 5–15% in production —
//! good enough for budget gating with a conservative `compact_at_pct`,
//! not for hard caps. `is_exact()` returns false so callers can label
//! emitted metrics accordingly.

use async_trait::async_trait;
use tiktoken_rs::{cl100k_base, CoreBPE};

use super::TokenCounter;
use crate::prompt_block::{flatten_blocks, PromptBlock};
use crate::retry::LlmError;
use crate::types::{ChatMessage, ChatRole};

pub struct TiktokenCounter {
    bpe: CoreBPE,
}

impl TiktokenCounter {
    pub fn new() -> Self {
        Self {
            bpe: cl100k_base().expect("tiktoken cl100k_base bundled"),
        }
    }

    fn encode_len(&self, text: &str) -> u32 {
        if text.is_empty() {
            return 0;
        }
        // `encode_with_special_tokens` is the conservative variant —
        // it avoids the disallowed-special-token panic when the input
        // happens to contain a control sequence the BPE recognises.
        self.bpe.encode_with_special_tokens(text).len() as u32
    }
}

impl Default for TiktokenCounter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TokenCounter for TiktokenCounter {
    async fn count_blocks(&self, blocks: &[PromptBlock]) -> Result<u32, LlmError> {
        Ok(self.encode_len(&flatten_blocks(blocks)))
    }

    async fn count_messages(
        &self,
        _model: &str,
        messages: &[ChatMessage],
    ) -> Result<u32, LlmError> {
        // OpenAI message-overhead heuristic: ~4 tokens per message frame
        // (role + content separators) + ~3 priming tokens for the reply.
        // Same shape applies well enough to other providers for the
        // budget-gating use-case.
        let mut total: u32 = 3;
        for m in messages {
            total = total.saturating_add(4);
            total = total.saturating_add(self.encode_len(&m.content));
            // Tool calls + results carry extra structured text.
            for tc in &m.tool_calls {
                total = total.saturating_add(self.encode_len(&tc.name));
                if let Ok(s) = serde_json::to_string(&tc.arguments) {
                    total = total.saturating_add(self.encode_len(&s));
                }
            }
            if let Some(name) = &m.name {
                total = total.saturating_add(self.encode_len(name));
            }
            if matches!(m.role, ChatRole::System) {
                // System framing burns a few extra tokens in most encodings.
                total = total.saturating_add(2);
            }
        }
        Ok(total)
    }

    fn is_exact(&self) -> bool {
        false
    }

    fn backend(&self) -> &'static str {
        "tiktoken"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt_block::PromptBlock;

    #[tokio::test]
    async fn empty_blocks_count_zero() {
        let c = TiktokenCounter::new();
        assert_eq!(c.count_blocks(&[]).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn block_count_is_deterministic() {
        let c = TiktokenCounter::new();
        let blocks = vec![PromptBlock::cached_long("identity", "You are Ana.")];
        let a = c.count_blocks(&blocks).await.unwrap();
        let b = c.count_blocks(&blocks).await.unwrap();
        assert_eq!(a, b);
        assert!(a > 0);
    }

    #[tokio::test]
    async fn known_phrase_count() {
        // "Hello, world!" is 4 tokens in cl100k_base — anchor test that
        // catches accidental encoder swaps.
        let c = TiktokenCounter::new();
        let blocks = vec![PromptBlock::plain("x", "Hello, world!")];
        assert_eq!(c.count_blocks(&blocks).await.unwrap(), 4);
    }

    #[tokio::test]
    async fn message_count_includes_overhead() {
        let c = TiktokenCounter::new();
        let msgs = vec![ChatMessage::user("hi")];
        let n = c.count_messages("anything", &msgs).await.unwrap();
        // 3 priming + 4 frame + at least 1 for "hi"
        assert!(n >= 8, "got {n}");
    }

    #[test]
    fn metadata() {
        let c = TiktokenCounter::new();
        assert!(!c.is_exact());
        assert_eq!(c.backend(), "tiktoken");
    }
}
