//! `CacheSafeParams` — fields that must match the parent's last LLM
//! request for a prompt-cache hit on the fork's first turn. Step 80.19/3.
//!
//! Verbatim semantics from `claude-code-leak/src/utils/forkedAgent.ts:57-115`.
//!
//! # Critical invariant (leak `:522-525`)
//!
//! `fork_context_messages` MUST preserve any incomplete tool_use blocks
//! the parent sent. Filtering them strips the paired tool_result rows and
//! breaks Anthropic's API (400 error), AND breaks the cache prefix.
//! `nexo_llm` repairs missing pairings in transport — same as the main
//! thread — so identical post-repair prefix keeps the cache hit.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use nexo_llm::prompt_block::{flatten_blocks, PromptBlock};
use nexo_llm::types::{ChatMessage, ChatRequest, ToolChoice, ToolDef};
use tokio::sync::RwLock;

/// Snapshot of the cache-affecting fields the fork must reuse to share
/// the parent's prompt cache.
///
/// All fields are `Clone` for cheap snapshotting. Callers typically build
/// this from the parent's most-recent `ChatRequest` via
/// [`CacheSafeParams::from_parent_request`].
#[derive(Debug, Clone)]
pub struct CacheSafeParams {
    /// Flat system prompt (legacy). When `system_blocks` is non-empty,
    /// providers append this AFTER the blocks (uncached) for back-compat.
    pub system_prompt: Option<String>,

    /// Structured system blocks with per-block cache_control. Preferred
    /// path; preserves Anthropic's 4-block cache-key shape.
    pub system_blocks: Vec<PromptBlock>,

    /// Tool catalog. Same ordering critical for cache key.
    pub tools: Vec<ToolDef>,

    /// Whether the tool catalog should be cached (Phase 77.x).
    pub cache_tools: bool,

    /// Model id (e.g. `claude-opus-4-7`). Cache key.
    pub model: String,

    /// Temperature. Some providers fold this into the cache key.
    pub temperature: f32,

    /// Max-output-tokens cap. Per leak `forkedAgent.ts:96-104`, setting
    /// this clamps `budget_tokens` and may invalidate cache when it
    /// differs from the parent. Mirror parent's value (or `None` to
    /// fall back to `ChatRequest` default `4096`).
    pub max_tokens: Option<u32>,

    /// Parent context messages — prepended to the fork's prompt
    /// messages on the first turn. Forms the cache prefix.
    ///
    /// **DO NOT FILTER** incomplete tool_use blocks from this list. See
    /// the module docstring + leak `forkedAgent.ts:522-525`.
    pub fork_context_messages: Vec<ChatMessage>,
}

impl CacheSafeParams {
    /// Snapshot a parent's last `ChatRequest` for fork reuse.
    /// Mirrors `createCacheSafeParams(REPLHookContext)` (leak `:131-145`).
    pub fn from_parent_request(req: &ChatRequest) -> Self {
        Self {
            system_prompt: req.system_prompt.clone(),
            system_blocks: req.system_blocks.clone(),
            tools: req.tools.clone(),
            cache_tools: req.cache_tools,
            model: req.model.clone(),
            temperature: req.temperature,
            max_tokens: Some(req.max_tokens),
            fork_context_messages: req.messages.clone(),
        }
    }

    /// Build the fork's first-turn `ChatRequest`. Prepends the parent's
    /// message prefix to the caller-supplied prompt messages so the
    /// cache prefix lines up.
    pub fn build_request(&self, mut prompt_messages: Vec<ChatMessage>) -> ChatRequest {
        let mut messages = self.fork_context_messages.clone();
        messages.append(&mut prompt_messages);
        ChatRequest {
            model: self.model.clone(),
            messages,
            tools: self.tools.clone(),
            max_tokens: self.max_tokens.unwrap_or(4096),
            temperature: self.temperature,
            system_prompt: self.system_prompt.clone(),
            stop_sequences: Vec::new(),
            tool_choice: ToolChoice::Auto,
            system_blocks: self.system_blocks.clone(),
            cache_tools: self.cache_tools,
        }
    }

    /// Hash of cache-affecting fields. Used to compare parent vs fork
    /// snapshots and detect drift via Phase 77.4 cache-break detection.
    ///
    /// `temperature` and `max_tokens` are NOT hashed — Anthropic does
    /// not key the prompt cache on them. `messages` are NOT hashed
    /// either; we measure runtime cache hit ratio as a proxy.
    pub fn cache_key_hash(&self) -> u64 {
        let mut h = DefaultHasher::new();
        // System prompt
        match &self.system_prompt {
            Some(s) => {
                1u8.hash(&mut h);
                s.hash(&mut h);
            }
            None => 0u8.hash(&mut h),
        }
        // System blocks — flatten preserves byte order
        flatten_blocks(&self.system_blocks).hash(&mut h);
        for block in &self.system_blocks {
            // Per-block cache policy still affects cache_control marker
            (block.cache as u8).hash(&mut h);
        }
        // Tools — name + description + parameters JSON
        for t in &self.tools {
            t.name.hash(&mut h);
            t.description.hash(&mut h);
            t.parameters.to_string().hash(&mut h);
        }
        self.cache_tools.hash(&mut h);
        // Model
        self.model.hash(&mut h);
        h.finish()
    }
}

/// Per-goal slot for the most-recent `CacheSafeParams`. Caller-owned;
/// LLM-behavior code calls [`save`](Self::save) after each successful
/// turn so post-turn forks (autoDream, AWAY_SUMMARY, future eval) can
/// snapshot the parent's last request without threading params through.
///
/// Mirrors `lastCacheSafeParams` slot from leak `forkedAgent.ts:75-83`,
/// but per-goal (held by goal driver) instead of global mutable static.
#[derive(Debug, Default, Clone)]
pub struct CacheSafeSlot {
    inner: Arc<RwLock<Option<CacheSafeParams>>>,
}

impl CacheSafeSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the stored params. Last write wins.
    pub async fn save(&self, params: CacheSafeParams) {
        let mut g = self.inner.write().await;
        *g = Some(params);
    }

    /// Snapshot a clone of the stored params, if any.
    pub async fn get_clone(&self) -> Option<CacheSafeParams> {
        self.inner.read().await.clone()
    }

    /// Clear the slot. Useful when starting a fresh goal.
    pub async fn clear(&self) {
        let mut g = self.inner.write().await;
        *g = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_llm::prompt_block::{CachePolicy, PromptBlock};
    use nexo_llm::types::{ChatMessage, ChatRole, ToolDef};

    fn mk_request() -> ChatRequest {
        ChatRequest {
            model: "claude-opus-4-7".into(),
            messages: vec![
                ChatMessage::user("hello"),
                ChatMessage::assistant("hi back"),
            ],
            tools: vec![ToolDef {
                name: "tool_a".into(),
                description: "first tool".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            max_tokens: 4096,
            temperature: 0.7,
            system_prompt: Some("You are useful.".into()),
            stop_sequences: vec![],
            tool_choice: ToolChoice::Auto,
            system_blocks: vec![PromptBlock::cached_long("identity", "You are nexo.")],
            cache_tools: true,
        }
    }

    #[test]
    fn from_parent_request_preserves_fields() {
        let req = mk_request();
        let cs = CacheSafeParams::from_parent_request(&req);
        assert_eq!(cs.model, "claude-opus-4-7");
        assert_eq!(cs.tools.len(), 1);
        assert_eq!(cs.system_blocks.len(), 1);
        assert_eq!(cs.fork_context_messages.len(), 2);
        assert_eq!(cs.max_tokens, Some(4096));
        assert!(cs.cache_tools);
    }

    /// Critical: messages with assistant tool_calls (and no paired
    /// tool result) must pass through untouched. Leak `:522-525`.
    #[test]
    fn from_parent_request_preserves_message_prefix_with_partial_tool_use() {
        let mut req = mk_request();
        // Push an assistant turn with a tool_use that has no paired tool_result.
        req.messages.push(ChatMessage::assistant_tool_calls(
            vec![nexo_llm::types::ToolCall {
                id: "call_1".into(),
                name: "tool_a".into(),
                arguments: serde_json::json!({}),
            }],
            String::new(),
        ));
        let original_count = req.messages.len();
        let cs = CacheSafeParams::from_parent_request(&req);
        assert_eq!(
            cs.fork_context_messages.len(),
            original_count,
            "filterIncompleteToolCalls regression — leak forkedAgent.ts:522-525 violated"
        );
        // Confirm the dangling tool_use is still present unchanged.
        let last = cs.fork_context_messages.last().unwrap();
        assert!(matches!(last.role, ChatRole::Assistant));
        assert_eq!(last.tool_calls.len(), 1);
        assert_eq!(last.tool_calls[0].id, "call_1");
    }

    #[test]
    fn cache_key_hash_stable_across_clones() {
        let cs = CacheSafeParams::from_parent_request(&mk_request());
        let h1 = cs.cache_key_hash();
        let cs2 = cs.clone();
        let h2 = cs2.cache_key_hash();
        assert_eq!(h1, h2);
    }

    #[test]
    fn cache_key_hash_changes_on_tool_addition() {
        let req1 = mk_request();
        let mut req2 = mk_request();
        req2.tools.push(ToolDef {
            name: "tool_b".into(),
            description: "second tool".into(),
            parameters: serde_json::json!({"type": "object"}),
        });
        let h1 = CacheSafeParams::from_parent_request(&req1).cache_key_hash();
        let h2 = CacheSafeParams::from_parent_request(&req2).cache_key_hash();
        assert_ne!(h1, h2);
    }

    #[test]
    fn cache_key_hash_unchanged_when_temperature_drifts() {
        let req1 = mk_request();
        let mut req2 = mk_request();
        req2.temperature = 0.1;
        let h1 = CacheSafeParams::from_parent_request(&req1).cache_key_hash();
        let h2 = CacheSafeParams::from_parent_request(&req2).cache_key_hash();
        assert_eq!(
            h1, h2,
            "temperature must not affect Anthropic prompt-cache key"
        );
    }

    #[test]
    fn build_request_prepends_parent_messages() {
        let cs = CacheSafeParams::from_parent_request(&mk_request());
        let prompt = vec![ChatMessage::user("new prompt")];
        let req = cs.build_request(prompt);
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[2].content, "new prompt");
        // Parent prefix preserved verbatim
        assert_eq!(req.messages[0].content, "hello");
        assert_eq!(req.messages[1].content, "hi back");
    }

    #[test]
    fn system_blocks_cache_policy_changes_hash() {
        let req1 = mk_request();
        let mut req2 = mk_request();
        if let Some(b) = req2.system_blocks.get_mut(0) {
            b.cache = CachePolicy::None;
        }
        let h1 = CacheSafeParams::from_parent_request(&req1).cache_key_hash();
        let h2 = CacheSafeParams::from_parent_request(&req2).cache_key_hash();
        assert_ne!(h1, h2);
    }

    // ── CacheSafeSlot tests ──

    #[tokio::test]
    async fn slot_save_then_get_clone_returns_params() {
        let slot = CacheSafeSlot::new();
        assert!(slot.get_clone().await.is_none());
        let cs = CacheSafeParams::from_parent_request(&mk_request());
        slot.save(cs.clone()).await;
        let got = slot.get_clone().await.unwrap();
        assert_eq!(got.cache_key_hash(), cs.cache_key_hash());
    }

    #[tokio::test]
    async fn slot_clear_resets() {
        let slot = CacheSafeSlot::new();
        slot.save(CacheSafeParams::from_parent_request(&mk_request())).await;
        slot.clear().await;
        assert!(slot.get_clone().await.is_none());
    }
}
