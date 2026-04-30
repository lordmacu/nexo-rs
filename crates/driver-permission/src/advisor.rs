//! Phase advisory_hook — generic tool-call advisory framework.
//!
//! Generalizes the bash-only `gather_bash_warnings` pipeline
//! (Phase 77.8-10 + C4.a-b) into an extensible registry that any
//! plugin can hook into. A [`ToolAdvisor`] inspects a tool call's
//! `(tool_name, input)` pair and optionally returns a one-line
//! warning. The `permission_prompt` MCP response composes all
//! advisor outputs into a unified `WARNING — tool advisories:`
//! block prefixed with the advisor `id` so consumers can group /
//! filter per source.
//!
//! All advisories are **advisory only** — they NEVER block tool
//! execution. The upstream LLM decider remains the authoritative
//! allow/deny gate. Plugins that want hard blocks integrate with
//! `nexo-core::plan_mode`'s `MUTATING_TOOLS` slice instead.
//!
//! Provider-agnostic: advisors operate on `(tool_name, input)`,
//! no LLM-provider assumption — the same composition runs whether
//! the upstream decider is Anthropic / MiniMax / OpenAI / Gemini /
//! DeepSeek / xAI / Mistral.
//!
//! IRROMPIBLE refs:
//! - claude-code-leak `src/tools/BashTool/bashSecurity.ts` — the
//!   single-tier-class pattern this module generalizes. The leak
//!   hardcodes bash; the registry composes the bash advisor with
//!   arbitrary plugin advisors so future surfaces (marketing,
//!   payment, CRM) can register their own tiers without touching
//!   `nexo-driver-permission`.
//! - `research/` — no relevant prior art (OpenClaw is channel-side
//!   and does not implement permission advisory layers).

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use serde_json::Value;

/// Inspect a tool call and optionally produce an advisory line.
///
/// `advise` returns `None` when the advisor has nothing to say
/// (zero-overhead default — most calls land here). `Some(text)`
/// becomes a line in the unified `WARNING — tool advisories:`
/// block, prefixed with `[<id>]`.
///
/// Implementations MUST be cheap. Heavy work (DB lookup, network)
/// belongs behind an internal cache or in an async follow-up
/// (`advisory_hook.b`).
pub trait ToolAdvisor: Send + Sync + 'static {
    /// Stable identifier — used as the per-line bracket prefix
    /// `[<id>]` and for telemetry. Should be lowercase /
    /// kebab-case (e.g. `"bash"`, `"marketing"`, `"payment"`).
    fn id(&self) -> &str;

    /// Inspect the tool call. Return `Some(line)` to add an
    /// advisory; `None` to stay silent. Multi-line `Some(...)`
    /// is split on `\n` by the registry and each non-empty line
    /// gets its own `[<id>]` prefix.
    fn advise(&self, tool_name: &str, input: &Value) -> Option<String>;
}

/// Composes multiple advisors into a unified advisory block.
///
/// Ordered registration; deterministic firing order. Panic
/// isolation per advisor — a buggy plugin cannot break the
/// permission flow. Empty registry returns `None` from
/// [`gather`](Self::gather), preserving zero-warning output for
/// unhinted tool calls.
#[derive(Default)]
pub struct AdvisorRegistry {
    advisors: Vec<Arc<dyn ToolAdvisor>>,
}

impl AdvisorRegistry {
    /// Empty registry. No advisors fire until `register` is
    /// called. Useful when a caller wants full control over the
    /// advisor list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-registers [`BashSecurityAdvisor`] so the legacy
    /// 5-tier bash pipeline keeps composing into the
    /// `permission_prompt` response. Default for
    /// `PermissionMcpServer::new` — back-compat with the
    /// pre-`advisory_hook` behavior.
    pub fn with_default() -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(BashSecurityAdvisor));
        reg
    }

    /// Append an advisor. Fires in registration order; ties
    /// broken by insertion order. No deduplication — duplicate
    /// advisors fire twice (caller's responsibility to avoid).
    pub fn register(&mut self, advisor: Arc<dyn ToolAdvisor>) {
        self.advisors.push(advisor);
    }

    /// Run every advisor and compose results into the unified
    /// `WARNING — tool advisories:\n- [<id>] <line>\n- ...`
    /// block. Returns `None` when every advisor stayed silent
    /// (or panicked).
    ///
    /// Panic isolation: an advisor that panics inside `advise`
    /// is logged via `tracing::warn!` and skipped; other
    /// advisors run unaffected. Use case: a buggy plugin
    /// shouldn't crash the permission decider.
    pub fn gather(&self, tool_name: &str, input: &Value) -> Option<String> {
        let mut lines: Vec<String> = Vec::new();
        for advisor in &self.advisors {
            let id = advisor.id().to_string();
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                advisor.advise(tool_name, input)
            }));
            match result {
                Ok(Some(text)) => {
                    for line in text.lines() {
                        if line.is_empty() {
                            continue;
                        }
                        lines.push(format!("[{id}] {line}"));
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    tracing::warn!(
                        advisor_id = %id,
                        tool_name,
                        "[advisor] panic — skipped; other advisors continue"
                    );
                }
            }
        }
        if lines.is_empty() {
            None
        } else {
            Some(format!(
                "WARNING — tool advisories:\n- {}",
                lines.join("\n- ")
            ))
        }
    }
}

/// Wraps the existing 5-tier bash pipeline as a
/// registry-compatible advisor.
///
/// Internal logic stays in `crate::mcp::gather_bash_warnings`
/// (Phase 77.8-10 + C4.a-b). `BashSecurityAdvisor` strips the
/// legacy `"WARNING — bash security:\n- "` prefix so the registry
/// can re-wrap with the unified `WARNING — tool advisories:`
/// header. Multi-line tier output is preserved — the registry
/// re-prefixes each line with `[bash]`.
pub struct BashSecurityAdvisor;

impl ToolAdvisor for BashSecurityAdvisor {
    fn id(&self) -> &str {
        "bash"
    }

    fn advise(&self, tool_name: &str, input: &Value) -> Option<String> {
        let outer = crate::mcp::gather_bash_warnings(tool_name, input)?;
        // Strip the legacy fixed-format prefix so the registry
        // can re-wrap. The inner format is the joined tier lines
        // with `\n- ` separator.
        let stripped = outer
            .strip_prefix("WARNING — bash security:\n- ")
            .unwrap_or(&outer);
        Some(stripped.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct StubAdvisor {
        id: &'static str,
        text: Option<String>,
    }
    impl ToolAdvisor for StubAdvisor {
        fn id(&self) -> &str {
            self.id
        }
        fn advise(&self, _tool_name: &str, _input: &Value) -> Option<String> {
            self.text.clone()
        }
    }

    struct PanickingAdvisor {
        sentinel: Arc<AtomicU32>,
    }
    impl ToolAdvisor for PanickingAdvisor {
        fn id(&self) -> &str {
            "panic"
        }
        fn advise(&self, _tool_name: &str, _input: &Value) -> Option<String> {
            self.sentinel.fetch_add(1, Ordering::SeqCst);
            panic!("simulated advisor panic");
        }
    }

    #[test]
    fn advisor_registry_empty_returns_none() {
        let reg = AdvisorRegistry::new();
        let out = reg.gather("Bash", &json!({"command": "rm -rf /"}));
        assert!(out.is_none(), "empty registry must return None");
    }

    #[test]
    fn advisor_registry_single_includes_id_prefix() {
        let mut reg = AdvisorRegistry::new();
        reg.register(Arc::new(StubAdvisor {
            id: "stub",
            text: Some("hello world".into()),
        }));
        let out = reg.gather("AnyTool", &json!({})).expect("warning expected");
        assert!(out.contains("[stub] hello world"), "got {out:?}");
        assert!(
            out.starts_with("WARNING — tool advisories:"),
            "missing unified prefix: {out:?}"
        );
    }

    #[test]
    fn advisor_registry_multiple_joins_lines() {
        let mut reg = AdvisorRegistry::new();
        reg.register(Arc::new(StubAdvisor {
            id: "first",
            text: Some("alpha".into()),
        }));
        reg.register(Arc::new(StubAdvisor {
            id: "second",
            text: Some("beta".into()),
        }));
        let out = reg.gather("X", &json!({})).expect("warning expected");
        assert!(out.contains("[first] alpha"), "got {out:?}");
        assert!(out.contains("[second] beta"), "got {out:?}");
    }

    #[test]
    fn advisor_registry_skips_silent_advisors() {
        let mut reg = AdvisorRegistry::new();
        reg.register(Arc::new(StubAdvisor {
            id: "noop",
            text: None,
        }));
        reg.register(Arc::new(StubAdvisor {
            id: "loud",
            text: Some("present".into()),
        }));
        let out = reg.gather("X", &json!({})).expect("warning expected");
        assert!(
            !out.contains("[noop]"),
            "silent advisor leaked into output: {out:?}"
        );
        assert!(out.contains("[loud] present"), "got {out:?}");
    }

    #[test]
    fn advisor_registry_isolates_panicking_advisor() {
        let mut reg = AdvisorRegistry::new();
        let sentinel = Arc::new(AtomicU32::new(0));
        reg.register(Arc::new(PanickingAdvisor {
            sentinel: Arc::clone(&sentinel),
        }));
        reg.register(Arc::new(StubAdvisor {
            id: "ok",
            text: Some("survived".into()),
        }));
        let out = reg.gather("X", &json!({})).expect("non-panic line expected");
        assert!(
            out.contains("[ok] survived"),
            "post-panic advisor must still fire: {out:?}"
        );
        assert!(
            !out.contains("[panic]"),
            "panicking advisor must be skipped: {out:?}"
        );
        assert_eq!(sentinel.load(Ordering::SeqCst), 1, "advise called once");
    }

    #[test]
    fn bash_security_advisor_strips_legacy_prefix() {
        let advisor = BashSecurityAdvisor;
        // Use a destructive command that the bash pipeline flags.
        let out = advisor.advise("Bash", &json!({"command": "rm -rf /"}));
        let text = out.expect("destructive command should fire");
        assert!(
            !text.starts_with("WARNING — bash security"),
            "legacy prefix must be stripped: {text:?}"
        );
        // Sanity: at least one tier line content present.
        assert!(
            !text.is_empty(),
            "stripped advisory must carry tier content"
        );
    }
}
