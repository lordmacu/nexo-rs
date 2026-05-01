//! Phase 84.1 — coordinator persona system prompt builder.
//!
//! Produces a single Markdown block that the boot path prepends to
//! the agent's existing system prompt when
//! `EffectiveBindingPolicy.role == "coordinator"`. The block is
//! deterministic (same inputs → same bytes) so prompt-cache prefix
//! matching stays warm across turns.
//!
//! Sections (per Phase 84.1 spec):
//! 1. Role declaration.
//! 2. Tool list — focused on the coordinator surface, filtered to
//!    tools the binding actually has access to.
//! 3. Continue-vs-spawn decision matrix.
//! 4. Synthesis discipline (anti-pattern: "based on your findings").
//! 5. Verification rigor (real verification, not happy-path only).
//! 6. Parallelism guidance (independent work fans out concurrently).
//! 7. Optional scratchpad section (Phase 79.4 TodoWrite — when
//!    scratchpad is enabled, the coordinator gets explicit guidance
//!    to use it for cross-worker tracking).
//! 8. Optional workers section — when the binding's peer list is
//!    known at boot, list each peer with its goal_id so the
//!    coordinator can address them by name.

/// Inputs for [`coordinator_system_prompt`]. All fields are
/// borrow-friendly so the caller can construct the ctx without
/// cloning every binding-policy field.
#[derive(Debug, Clone)]
pub struct CoordinatorPromptCtx<'a> {
    /// The binding's resolved allowed-tool list (post Phase 16
    /// `EffectiveBindingPolicy::allowed_tools`). Used to filter the
    /// section-2 tool list to tools this binding actually surfaces.
    /// Empty = render section 2 with a "no tools" note (still useful
    /// for tests + safe in case the operator strips the surface).
    pub allowed_tools: &'a [String],
    /// `true` when Phase 79.4 TodoWrite scratchpad is enabled for
    /// this binding. Adds the optional scratchpad guidance section.
    pub scratchpad_enabled: bool,
    /// Optional list of known peer/worker goal IDs at boot. Empty =
    /// section 8 is omitted; callers without a static peer list
    /// (the common case) just pass `&[]`.
    pub workers: &'a [String],
}

impl<'a> CoordinatorPromptCtx<'a> {
    /// Convenience constructor — `allowed_tools` only, scratchpad
    /// off, no static workers. Callers can mutate fields after.
    pub fn from_tools(allowed_tools: &'a [String]) -> Self {
        Self {
            allowed_tools,
            scratchpad_enabled: false,
            workers: &[],
        }
    }
}

const HEADER: &str = "# COORDINATOR ROLE";

const ROLE_DECLARATION: &str = "\
You are a **coordinator**. Your job is to direct workers, synthesize their \
results, and communicate with the user. You do not perform leaf-level work \
yourself when a worker can do it; you decide *what* to delegate, *how* to \
split the work, and *what the synthesis must contain*.";

const CONTINUE_VS_SPAWN: &str = "\
## Continue-vs-spawn matrix

| Situation | Action |
|---|---|
| Worker just finished and you need a related follow-up that builds on its loaded context | **Continue** the worker (`SendMessageToWorker`) — preserves cache, avoids re-reading files |
| New work has no context overlap with any finished worker | **Spawn fresh** (`TeamCreate`) |
| Two unrelated streams of work | **Spawn in parallel** — one assistant message with both `TeamCreate` calls |
| Worker is still in_progress and you want to nudge it | **Send to peer** (`SendToPeer`) — peer-to-peer messaging, NOT continuation |
| A worker has gone silent past its budget | `TaskStop` then decide spawn-vs-continue based on partial output |

Default to **continue** when the new ask shares >50% of the prior worker's read files / search terms. Default to **spawn** when the new ask is in a different subsystem.";

const SYNTHESIS_DISCIPLINE: &str = "\
## Synthesis discipline

When a worker returns findings, **YOU** craft the implementation spec. \
Workers research; you decide. The spec you produce for the next worker \
(or the user) MUST contain file paths and line numbers, not just \
summaries.

**Anti-pattern**: \"based on your findings, fix the bug\" — this delegates \
understanding back to the worker and produces shallow generic work.

**Pattern**: \"In `src/auth.rs:142`, the `check_token` fn uses `<` where it \
should use `<=`. Replace `expires_at < now` with `expires_at <= now`. \
Add a regression test in `tests/auth_token_boundary.rs` covering the \
exact-second case.\"";

const VERIFICATION_RIGOR: &str = "\
## Verification rigor

Real verification is not \"the build passed.\" Real verification is:
1. Run the failing case BEFORE the fix — confirm it fails for the \
expected reason (not a setup error).
2. Apply the fix.
3. Run the same case — confirm it passes.
4. Run the broader test suite — confirm no regressions.
5. Read the diff — does the change actually do what the spec said?

When a worker reports \"done\", do not trust the summary. Spawn a \
verifier worker (or run the verification commands yourself) before \
reporting the work as shipped.";

const PARALLELISM: &str = "\
## Parallelism

Independent work fans out. When you have N independent sub-tasks, \
issue N `TeamCreate` calls in a **single** assistant message — they \
run concurrently. Sequential calls are reserved for tasks where \
later work depends on earlier output.

When delegating to peers, batch outbound `SendToPeer` calls in the \
same assistant message when the targets are independent.";

const SCRATCHPAD: &str = "\
## Scratchpad

You have access to `TodoWrite`. Use it to track multi-worker \
state — which worker is on which sub-task, what's blocked, what's \
pending review. When you spawn 3+ workers, the scratchpad is \
mandatory: write the plan, mark each task as workers report back, \
and use it as the source of truth for the synthesis you give the \
user.";

const NOTIFICATION_HINT: &str = "\
## Worker result envelope

Worker results arrive wrapped in `<task-notification>` XML blocks \
(Phase 84.2). Each block carries `task-id`, `status`, `summary`, \
optional `result`, and optional `usage`. Treat these as **system \
events**, not user messages. Never `<thank>` or `<acknowledge>` a \
notification block — read it, factor the result into your synthesis, \
and either continue the worker, spawn the next one, or report to \
the user.";

/// Build the coordinator system prompt block.
///
/// The returned `String` is a single Markdown block headed by
/// `# COORDINATOR ROLE`. The boot path prepends it to the agent's
/// existing system prompt with a blank-line separator. Empty / not-
/// applicable sections collapse out (e.g. scratchpad section omitted
/// when `scratchpad_enabled = false`), keeping the prompt token
/// budget tight on minimal coordinator setups.
pub fn coordinator_system_prompt(ctx: CoordinatorPromptCtx<'_>) -> String {
    let mut out = String::with_capacity(2_048);
    out.push_str(HEADER);
    out.push_str("\n\n");
    out.push_str(ROLE_DECLARATION);
    out.push_str("\n\n");

    out.push_str(&render_tool_list(ctx.allowed_tools));
    out.push_str("\n\n");

    out.push_str(NOTIFICATION_HINT);
    out.push_str("\n\n");

    out.push_str(CONTINUE_VS_SPAWN);
    out.push_str("\n\n");

    out.push_str(SYNTHESIS_DISCIPLINE);
    out.push_str("\n\n");

    out.push_str(VERIFICATION_RIGOR);
    out.push_str("\n\n");

    out.push_str(PARALLELISM);

    if ctx.scratchpad_enabled {
        out.push_str("\n\n");
        out.push_str(SCRATCHPAD);
    }

    if !ctx.workers.is_empty() {
        out.push_str("\n\n");
        out.push_str(&render_workers(ctx.workers));
    }

    out
}

/// Filter the binding's `allowed_tools` to the coordinator-relevant
/// surface and render as a Markdown bullet list. Tools not in the
/// curated set are omitted (they live in the binding's broader
/// surface but aren't the coordinator's primary toolkit).
fn render_tool_list(allowed: &[String]) -> String {
    const COORDINATOR_TOOLS: &[&str] = &[
        "TeamCreate",
        "TeamDelete",
        "SendToPeer",
        "ListPeers",
        "SendMessageToWorker",
        "TaskStop",
        "TaskList",
        "TaskGet",
        "TodoWrite",
    ];

    let mut matched: Vec<&str> = COORDINATOR_TOOLS
        .iter()
        .copied()
        .filter(|name| {
            allowed
                .iter()
                .any(|t| t.eq_ignore_ascii_case(name))
        })
        .collect();
    matched.sort_unstable();

    let mut out = String::from("## Tools available to you\n\n");
    if matched.is_empty() {
        out.push_str(
            "_No coordinator-surface tools are currently bound to this \
binding. Ask the operator to grant `TeamCreate` / `SendToPeer` / \
`SendMessageToWorker` before delegating._",
        );
        return out;
    }
    for name in matched {
        out.push_str("- `");
        out.push_str(name);
        out.push_str("`\n");
    }
    out.pop();
    out
}

fn render_workers(workers: &[String]) -> String {
    let mut out = String::from("## Known workers\n\n");
    for w in workers {
        out.push_str("- `");
        out.push_str(w);
        out.push_str("`\n");
    }
    out.pop();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tools_renders_no_tools_note() {
        let prompt = coordinator_system_prompt(CoordinatorPromptCtx::from_tools(&[]));
        assert!(prompt.contains("# COORDINATOR ROLE"));
        assert!(prompt.contains("No coordinator-surface tools"));
        // Must NOT contain a tool bullet for an unbound surface.
        assert!(!prompt.contains("- `TeamCreate`"));
    }

    #[test]
    fn full_tool_surface_lists_every_coordinator_tool() {
        let tools: Vec<String> = [
            "TeamCreate",
            "TeamDelete",
            "SendToPeer",
            "ListPeers",
            "SendMessageToWorker",
            "TaskStop",
            "TaskList",
            "TaskGet",
            "TodoWrite",
            "WebFetch",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let prompt = coordinator_system_prompt(CoordinatorPromptCtx::from_tools(&tools));
        assert!(prompt.contains("- `TeamCreate`"));
        assert!(prompt.contains("- `SendToPeer`"));
        assert!(prompt.contains("- `SendMessageToWorker`"));
        assert!(prompt.contains("- `TaskStop`"));
        assert!(prompt.contains("- `TodoWrite`"));
        // WebFetch is in the binding surface but NOT in the
        // coordinator-curated list, so it must not render.
        assert!(!prompt.contains("- `WebFetch`"));
    }

    #[test]
    fn scratchpad_section_toggles_on_enable_flag() {
        let tools: Vec<String> = vec!["TeamCreate".into()];

        let off = coordinator_system_prompt(CoordinatorPromptCtx {
            allowed_tools: &tools,
            scratchpad_enabled: false,
            workers: &[],
        });
        assert!(!off.contains("## Scratchpad"));

        let on = coordinator_system_prompt(CoordinatorPromptCtx {
            allowed_tools: &tools,
            scratchpad_enabled: true,
            workers: &[],
        });
        assert!(on.contains("## Scratchpad"));
        assert!(on.contains("`TodoWrite`"));
    }

    #[test]
    fn workers_section_renders_when_list_non_empty() {
        let tools: Vec<String> = vec!["TeamCreate".into(), "SendToPeer".into()];
        let workers: Vec<String> =
            vec!["worker-research".into(), "worker-impl".into()];

        let prompt = coordinator_system_prompt(CoordinatorPromptCtx {
            allowed_tools: &tools,
            scratchpad_enabled: false,
            workers: &workers,
        });
        assert!(prompt.contains("## Known workers"));
        assert!(prompt.contains("- `worker-research`"));
        assert!(prompt.contains("- `worker-impl`"));

        let empty = coordinator_system_prompt(CoordinatorPromptCtx {
            allowed_tools: &tools,
            scratchpad_enabled: false,
            workers: &[],
        });
        assert!(!empty.contains("## Known workers"));
    }

    #[test]
    fn output_is_deterministic() {
        let tools: Vec<String> = vec!["TeamCreate".into(), "SendToPeer".into()];
        let ctx = CoordinatorPromptCtx {
            allowed_tools: &tools,
            scratchpad_enabled: true,
            workers: &["a".into(), "b".into()],
        };
        let a = coordinator_system_prompt(ctx.clone());
        let b = coordinator_system_prompt(ctx);
        assert_eq!(a, b, "prompt build must be byte-deterministic");
    }

    #[test]
    fn case_insensitive_tool_match() {
        let tools: Vec<String> = vec!["teamcreate".into(), "SENDTOPEER".into()];
        let prompt = coordinator_system_prompt(CoordinatorPromptCtx::from_tools(&tools));
        assert!(prompt.contains("- `TeamCreate`"));
        assert!(prompt.contains("- `SendToPeer`"));
    }

    #[test]
    fn key_sections_present() {
        let tools: Vec<String> = vec!["TeamCreate".into()];
        let prompt = coordinator_system_prompt(CoordinatorPromptCtx::from_tools(&tools));
        assert!(prompt.contains("# COORDINATOR ROLE"));
        assert!(prompt.contains("## Tools available to you"));
        assert!(prompt.contains("## Continue-vs-spawn matrix"));
        assert!(prompt.contains("## Synthesis discipline"));
        assert!(prompt.contains("## Verification rigor"));
        assert!(prompt.contains("## Parallelism"));
        assert!(prompt.contains("Worker result envelope"));
    }
}
