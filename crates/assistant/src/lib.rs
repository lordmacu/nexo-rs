//! Phase 80.15 — assistant-mode runtime.
//!
//! Per-binding behavioural toggle. When active, the binding's
//! effective system prompt picks up an addendum that nudges the
//! agent toward proactive behaviour. The flag is a single bool on
//! `AgentContext`, readable by any downstream consumer
//! (driver-loop, cron, brief mode, dream context, etc.).
//!
//! The toggle is **boot-immutable**: enabling / disabling
//! `assistant_mode` requires a daemon restart so a single turn
//! never sees a half-flipped state. The addendum *content* itself
//! IS hot-reloadable through the Phase 18 ArcSwap path — operators
//! can iterate on the prompt text without bouncing the daemon.
//!
//! Provider-agnostic: the bundled default addendum is plain English
//! that works under any LLM client (Anthropic, MiniMax, OpenAI,
//! Gemini, DeepSeek, xAI, Mistral). No model-specific phrasing.

use std::sync::Arc;

pub use nexo_config::types::assistant::AssistantConfig;

/// Bundled default addendum text. Operator can override via
/// `assistant_mode.system_prompt_addendum`. Provider-agnostic.
pub const DEFAULT_ADDENDUM: &str = "\
You are running in assistant mode. Your default posture is \
proactive: when the user is away, you may use scheduled triggers \
(cron) and channel inbound to drive your own actions, including \
spawning teammates, calling tools to gather context, and waiting on \
external events. When you have something useful to report, surface \
it succinctly through the configured outbound channel; otherwise \
stay quiet rather than narrating idle time. Only block on user \
input when you genuinely need a decision they can supply.";

/// Resolved per-binding assistant view, built once at boot.
///
/// `enabled` is captured snapshot-style; toggling it requires a
/// restart. `addendum` lives behind an `Arc` so cloning into the
/// `AgentContext` is cheap. `initial_team` is also `Arc`-shared —
/// the resolved spec doesn't change at runtime (Phase 80.15.b will
/// wire actual spawn behaviour against this list).
#[derive(Clone, Debug)]
pub struct ResolvedAssistant {
    pub enabled: bool,
    /// Effective addendum — either the operator override or the
    /// bundled default. Empty string when `enabled == false`.
    pub addendum: Arc<String>,
    /// Resolved teammate names, or empty when disabled.
    pub initial_team: Arc<Vec<String>>,
}

impl ResolvedAssistant {
    /// Resolve from a binding's optional [`AssistantConfig`].
    /// Returns a disabled view when the field is `None` or
    /// `enabled == false`.
    pub fn resolve(cfg: Option<&AssistantConfig>) -> Self {
        match cfg {
            Some(c) if c.enabled => Self {
                enabled: true,
                addendum: Arc::new(
                    c.system_prompt_addendum
                        .clone()
                        .unwrap_or_else(|| DEFAULT_ADDENDUM.to_string()),
                ),
                initial_team: Arc::new(c.initial_team.clone()),
            },
            _ => Self::disabled(),
        }
    }

    /// The default zero-cost view: no addendum, no team, flag off.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            addendum: Arc::new(String::new()),
            initial_team: Arc::new(Vec::new()),
        }
    }

    /// Convenience accessor — `true` iff the addendum should be
    /// appended to the binding's system prompt at render time.
    pub fn should_append_addendum(&self) -> bool {
        self.enabled && !self.addendum.is_empty()
    }
}

impl Default for ResolvedAssistant {
    fn default() -> Self {
        Self::disabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_when_none() {
        let r = ResolvedAssistant::resolve(None);
        assert!(!r.enabled);
        assert!(r.addendum.is_empty());
        assert!(r.initial_team.is_empty());
        assert!(!r.should_append_addendum());
    }

    #[test]
    fn disabled_when_enabled_false() {
        let cfg = AssistantConfig {
            enabled: false,
            system_prompt_addendum: Some("ignored".into()),
            initial_team: vec!["nope".into()],
        };
        let r = ResolvedAssistant::resolve(Some(&cfg));
        assert!(!r.enabled);
        assert!(r.addendum.is_empty());
        assert!(r.initial_team.is_empty());
    }

    #[test]
    fn uses_default_when_no_override() {
        let cfg = AssistantConfig {
            enabled: true,
            system_prompt_addendum: None,
            initial_team: vec![],
        };
        let r = ResolvedAssistant::resolve(Some(&cfg));
        assert!(r.enabled);
        assert_eq!(&*r.addendum, DEFAULT_ADDENDUM);
        assert!(r.should_append_addendum());
    }

    #[test]
    fn honors_override() {
        let cfg = AssistantConfig {
            enabled: true,
            system_prompt_addendum: Some("custom nudge".into()),
            initial_team: vec![],
        };
        let r = ResolvedAssistant::resolve(Some(&cfg));
        assert_eq!(&*r.addendum, "custom nudge");
    }

    #[test]
    fn initial_team_passes_through() {
        let cfg = AssistantConfig {
            enabled: true,
            system_prompt_addendum: None,
            initial_team: vec!["researcher".into(), "coder".into()],
        };
        let r = ResolvedAssistant::resolve(Some(&cfg));
        assert_eq!(*r.initial_team, vec!["researcher", "coder"]);
    }

    #[test]
    fn default_is_disabled() {
        let r = ResolvedAssistant::default();
        assert!(!r.enabled);
    }
}
