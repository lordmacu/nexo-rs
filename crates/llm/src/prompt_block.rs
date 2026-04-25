//! Structured system-prompt assembly with explicit cache breakpoints.
//!
//! A `Vec<PromptBlock>` replaces the legacy flat `system_prompt: String`
//! when callers want providers to apply prompt caching. Each block
//! carries an opaque `label` for telemetry and a `CachePolicy` that the
//! provider client materializes into its native shape (Anthropic
//! `cache_control: {"type":"ephemeral", "ttl":"1h"}`, OpenAI implicit
//! prefix matching, Gemini `cachedContents`, …).
//!
//! The block list is **ordered**: providers serialize it head-to-tail,
//! and Anthropic's prefix-matching cache only hits when the prefix is
//! byte-identical across requests. Producers should keep block order
//! stable — re-ordering invalidates every cache entry downstream of
//! the swap.
//!
//! Cache breakpoints are placed by the provider client, not the caller.
//! For Anthropic, the rule is: walk the blocks in order, set
//! `cache_control` on the **last** block of each contiguous run that
//! shares the same `CachePolicy != None`. Anthropic accepts at most 4
//! breakpoints per request; producers should design their block list so
//! the natural breakpoints (tools, identity, skills, tail) fit that
//! budget.

/// Cache policy for a `PromptBlock`. See module docs for how providers
/// translate each variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CachePolicy {
    /// Do not request caching for this block. Providers serialize it
    /// as plain content.
    #[default]
    None,
    /// Short-TTL ephemeral cache (~5 min). Right for blocks that rotate
    /// every few turns (rolling tail of conversation history).
    Ephemeral5m,
    /// Long-TTL ephemeral cache (~1 hour). Right for blocks that stay
    /// stable across an entire session (tool catalog, identity prompt,
    /// skills bundle). Anthropic charges a 2× write premium vs 5min,
    /// recovered after one extra hit beyond the 5-min window — a fair
    /// trade for heartbeat-driven agents that idle for minutes between
    /// turns.
    Ephemeral1h,
}

impl CachePolicy {
    /// True when this policy requests *some* caching. Used by providers
    /// to decide whether to attach a `cache_control` marker.
    pub fn is_cached(self) -> bool {
        !matches!(self, CachePolicy::None)
    }
}

/// One contiguous slice of the system prompt. Providers join blocks in
/// order and place cache breakpoints according to `cache`.
#[derive(Debug, Clone)]
pub struct PromptBlock {
    /// Stable label for telemetry — `"tools"`, `"identity"`, `"skills"`,
    /// `"tail"`, etc. Free-form, but should be reused across requests
    /// so dashboards can group by block. Not sent to the provider.
    pub label: &'static str,
    /// Block content, serialized verbatim. May be empty (the provider
    /// will skip it). Producers should avoid trailing newlines unless
    /// they want them in the rendered prompt.
    pub text: String,
    /// Caching policy for this block.
    pub cache: CachePolicy,
}

impl PromptBlock {
    /// Convenience: an uncached block. The most common shape for
    /// dynamic per-turn context (sender id, current time).
    pub fn plain(label: &'static str, text: impl Into<String>) -> Self {
        Self {
            label,
            text: text.into(),
            cache: CachePolicy::None,
        }
    }

    /// Convenience: a long-TTL cached block. Right for stable bundles
    /// (tools, identity, skills).
    pub fn cached_long(label: &'static str, text: impl Into<String>) -> Self {
        Self {
            label,
            text: text.into(),
            cache: CachePolicy::Ephemeral1h,
        }
    }

    /// Convenience: a short-TTL cached block. Right for the rolling
    /// tail of conversation context.
    pub fn cached_short(label: &'static str, text: impl Into<String>) -> Self {
        Self {
            label,
            text: text.into(),
            cache: CachePolicy::Ephemeral5m,
        }
    }
}

/// Flatten a block list into a single string, preserving order with
/// a `\n\n` separator. Provider clients that do not (yet) support
/// breakpoints fall back to this representation, which is byte-equivalent
/// to the legacy flat `system_prompt`.
pub fn flatten_blocks(blocks: &[PromptBlock]) -> String {
    let mut out = String::new();
    let mut first = true;
    for b in blocks {
        if b.text.is_empty() {
            continue;
        }
        if !first {
            out.push_str("\n\n");
        }
        out.push_str(&b.text);
        first = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_policy_default_is_none() {
        assert_eq!(CachePolicy::default(), CachePolicy::None);
        assert!(!CachePolicy::default().is_cached());
        assert!(CachePolicy::Ephemeral5m.is_cached());
        assert!(CachePolicy::Ephemeral1h.is_cached());
    }

    #[test]
    fn flatten_drops_empty_blocks_and_joins_with_double_newline() {
        let blocks = vec![
            PromptBlock::cached_long("a", "hello"),
            PromptBlock::plain("b", ""),
            PromptBlock::cached_short("c", "world"),
        ];
        assert_eq!(flatten_blocks(&blocks), "hello\n\nworld");
    }

    #[test]
    fn flatten_empty_list_yields_empty_string() {
        assert_eq!(flatten_blocks(&[]), "");
    }

    #[test]
    fn convenience_constructors_set_expected_policy() {
        assert_eq!(PromptBlock::plain("x", "y").cache, CachePolicy::None);
        assert_eq!(
            PromptBlock::cached_long("x", "y").cache,
            CachePolicy::Ephemeral1h
        );
        assert_eq!(
            PromptBlock::cached_short("x", "y").cache,
            CachePolicy::Ephemeral5m
        );
    }
}
