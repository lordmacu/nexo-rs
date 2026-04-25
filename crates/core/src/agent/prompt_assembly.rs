//! System prompt assembly with explicit cache breakpoints.
//!
//! `LlmAgentBehavior::run_turn` collected the system prompt as a
//! `Vec<String>` joined with `\n\n`. Phase A.2 keeps that join as the
//! fallback for providers without prompt caching, but for Anthropic
//! the same text is emitted as a `Vec<PromptBlock>` with explicit
//! `CachePolicy` per block. The provider client (`anthropic.rs`)
//! materializes the policies into `cache_control` markers.
//!
//! Block layout (4 cache breakpoints, the Anthropic per-request cap):
//!   1. workspace bundle (IDENTITY+SOUL+USER+AGENTS+notes+MEMORY)
//!      → `Ephemeral1h` — stable per session.
//!   2. skills bundle (loaded from skills_dir per binding)
//!      → `Ephemeral1h` — stable per binding/agent.
//!   3. peers + system_prompt + language directive (per-binding glue)
//!      → `Ephemeral1h` — stable per binding/agent.
//!   4. channel metadata (sender id, etc.)
//!      → `Ephemeral5m` — varies per turn (different senders, time).
//!
//! The 5th potential breakpoint (the message tail) is set by the
//! provider on the last user message, not as a system block.
//!
//! Empty / missing sections collapse out of the list — they don't
//! occupy a breakpoint. The order is **stable**: callers that re-emit
//! the same content see byte-identical bytes, which is what
//! Anthropic's prefix matcher needs.

use nexo_llm::PromptBlock;

/// Inputs for the system prompt. Each is `Option<String>` because
/// every block is genuinely optional (an agent without skills, an
/// inbound without a sender id, …). Empty strings are treated like
/// `None`.
pub struct PromptInputs {
    pub workspace: Option<String>,
    pub skills: Option<String>,
    /// Peer directory + per-binding system_prompt + language directive,
    /// already merged. They share a cache breakpoint because they all
    /// rotate together when the binding policy changes.
    pub binding_glue: Option<String>,
    /// Sender id, current channel context, anything else that varies
    /// per turn but is small enough to carry in the system prompt.
    pub channel_meta: Option<String>,
}

/// Build the ordered, breakpointed prompt blocks. Empty sections drop
/// out of the result; never returns more than 4 blocks (the Anthropic
/// cap).
pub fn build_blocks(inputs: PromptInputs) -> Vec<PromptBlock> {
    let mut out: Vec<PromptBlock> = Vec::with_capacity(4);
    if let Some(text) = non_empty(inputs.workspace) {
        out.push(PromptBlock::cached_long("workspace", text));
    }
    if let Some(text) = non_empty(inputs.skills) {
        out.push(PromptBlock::cached_long("skills", text));
    }
    if let Some(text) = non_empty(inputs.binding_glue) {
        out.push(PromptBlock::cached_long("binding_glue", text));
    }
    if let Some(text) = non_empty(inputs.channel_meta) {
        out.push(PromptBlock::cached_short("channel_meta", text));
    }
    out
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.and_then(|t| if t.trim().is_empty() { None } else { Some(t) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_inputs_yield_empty_list() {
        let out = build_blocks(PromptInputs {
            workspace: None,
            skills: None,
            binding_glue: None,
            channel_meta: None,
        });
        assert!(out.is_empty());
    }

    #[test]
    fn empty_strings_drop_out() {
        let out = build_blocks(PromptInputs {
            workspace: Some("".to_string()),
            skills: Some("   ".to_string()),
            binding_glue: Some("real".to_string()),
            channel_meta: None,
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "binding_glue");
        assert_eq!(out[0].text, "real");
    }

    #[test]
    fn ordered_and_labeled() {
        let out = build_blocks(PromptInputs {
            workspace: Some("ws".into()),
            skills: Some("sk".into()),
            binding_glue: Some("bg".into()),
            channel_meta: Some("cm".into()),
        });
        assert_eq!(out.len(), 4);
        let labels: Vec<_> = out.iter().map(|b| b.label).collect();
        assert_eq!(
            labels,
            vec!["workspace", "skills", "binding_glue", "channel_meta"]
        );
    }

    #[test]
    fn workspace_skills_glue_get_long_ttl_and_meta_short() {
        use nexo_llm::CachePolicy;
        let out = build_blocks(PromptInputs {
            workspace: Some("ws".into()),
            skills: Some("sk".into()),
            binding_glue: Some("bg".into()),
            channel_meta: Some("cm".into()),
        });
        assert_eq!(out[0].cache, CachePolicy::Ephemeral1h);
        assert_eq!(out[1].cache, CachePolicy::Ephemeral1h);
        assert_eq!(out[2].cache, CachePolicy::Ephemeral1h);
        assert_eq!(out[3].cache, CachePolicy::Ephemeral5m);
    }

    #[test]
    fn cap_at_four_blocks_by_construction() {
        // The function only knows about 4 buckets — cap is structural.
        let out = build_blocks(PromptInputs {
            workspace: Some("a".into()),
            skills: Some("b".into()),
            binding_glue: Some("c".into()),
            channel_meta: Some("d".into()),
        });
        assert!(out.len() <= 4);
    }
}
