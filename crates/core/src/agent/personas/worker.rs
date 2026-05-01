//! Phase 84.4 — worker persona system prompt builder.
//!
//! Complement to [`super::coordinator`]. When the binding's
//! resolved `BindingRole` is `Worker`, the runtime prepends this
//! block ahead of the agent's existing system prompt. Workers run
//! self-contained tasks dispatched by a coordinator; the persona
//! steers them toward terse, verified, on-spec output rather than
//! user-facing dialogue.
//!
//! Sections:
//! 1. Role declaration — "you execute, you don't initiate dialogue"
//! 2. Output discipline — terse final answer matching the spec's
//!    done criteria (file paths + commit hashes for code work;
//!    findings list for research).
//! 3. Self-verification — typecheck/test before reporting done;
//!    surface real errors, not paraphrases.
//! 4. Tool surface — list of tools this binding actually has, with
//!    explicit note that team-coordination tools are absent by
//!    design.

/// Inputs for [`worker_system_prompt`]. Mirrors the shape of
/// [`super::coordinator::CoordinatorPromptCtx`] for symmetry.
#[derive(Debug, Clone)]
pub struct WorkerPromptCtx<'a> {
    /// Binding's resolved allowed-tool list. The persona's tool
    /// list reflects what the worker can actually call.
    pub allowed_tools: &'a [String],
    /// `true` when Phase 79.4 TodoWrite scratchpad is enabled —
    /// adds a short note that the scratchpad is the worker's
    /// state across its own turns (not for cross-team coordination,
    /// which the worker doesn't do).
    pub scratchpad_enabled: bool,
}

impl<'a> WorkerPromptCtx<'a> {
    pub fn from_tools(allowed_tools: &'a [String]) -> Self {
        Self {
            allowed_tools,
            scratchpad_enabled: false,
        }
    }
}

const HEADER: &str = "# WORKER ROLE";

const ROLE_DECLARATION: &str = "\
You are a **worker** dispatched by a coordinator. Your job is to \
execute one self-contained task and report the result. You do **not** \
initiate user-facing dialogue, do not negotiate scope, do not ask \
the user for clarification — questions about scope go back to the \
coordinator via your final answer (\"blocked: need X\").";

const OUTPUT_DISCIPLINE: &str = "\
## Output discipline

Your final answer is read by another agent (the coordinator), not by \
a human. Optimize for parseability:

- **Code work**: report the file path + line range + the actual \
diff (or a commit hash when committed). \"I changed auth.rs to use \
`<=`\" is not enough; \"`crates/auth.rs:142` — `expires_at < now` \
→ `expires_at <= now` (commit `abc1234`)\" is.
- **Research / search**: report findings as a bullet list with \
`file_path:line` references. Do not summarize unless explicitly \
asked.
- **Failures / blockers**: state the actual error verbatim — copy \
the compiler/test output, do not paraphrase. Include the command \
that triggered it.

No preamble, no \"I will now…\", no closing pleasantries. The \
coordinator's parser treats your output as data.";

const SELF_VERIFICATION: &str = "\
## Self-verification before reporting done

Before you say \"done\":
1. Run the relevant typecheck (`cargo check -p <crate>` /
   `tsc --noEmit` / equivalent).
2. Run the relevant test suite (`cargo test -p <crate>` /
   `pytest path/` / equivalent — narrow the scope, don't run the
   universe).
3. If either fails, **do not report success**. Either fix the
   failure or report the failure verbatim with the command
   that produced it.
4. Read the diff one more time. Does the change actually do what
   the spec said?

The coordinator trusts your verification. False \"done\" reports \
poison the synthesis above you.";

const TOOL_SURFACE_HEADER: &str = "## Tools available to you";

const NO_COORDINATOR_TOOLS_NOTE: &str = "\
You do **not** have `TeamCreate`, `SendToPeer`, `SendMessageToWorker`, \
or `TaskStop` — those are coordinator tools. If you find yourself \
wanting to spawn a sub-task, your task is too big; report \
\"blocked: scope too large\" back to the coordinator.";

const SCRATCHPAD: &str = "\
## Scratchpad

You have access to `TodoWrite`. Use it for **your own** multi-step \
state: which file you're on, what you've checked, what's left. \
Do not use it to coordinate with other workers — you don't \
coordinate, the coordinator does.";

/// Build the worker system prompt block.
///
/// Returns a single Markdown block headed by `# WORKER ROLE`. The
/// boot path prepends it to the agent's existing system prompt
/// with a blank-line separator. Deterministic — same inputs → same
/// bytes — so the prompt-cache prefix matcher stays warm.
pub fn worker_system_prompt(ctx: WorkerPromptCtx<'_>) -> String {
    let mut out = String::with_capacity(1_500);
    out.push_str(HEADER);
    out.push_str("\n\n");
    out.push_str(ROLE_DECLARATION);
    out.push_str("\n\n");
    out.push_str(OUTPUT_DISCIPLINE);
    out.push_str("\n\n");
    out.push_str(SELF_VERIFICATION);
    out.push_str("\n\n");
    out.push_str(&render_tool_list(ctx.allowed_tools));

    if ctx.scratchpad_enabled {
        out.push_str("\n\n");
        out.push_str(SCRATCHPAD);
    }

    out
}

/// Render the binding's `allowed_tools` as a Markdown bullet list,
/// **without** filtering through a curated subset (workers see
/// whatever surface the operator granted; no implicit pruning).
/// The block ends with the explicit "no coordinator tools" note so
/// the worker doesn't waste turns probing for `TeamCreate`.
fn render_tool_list(allowed: &[String]) -> String {
    let mut out = String::from(TOOL_SURFACE_HEADER);
    out.push_str("\n\n");

    if allowed.is_empty() || allowed.iter().any(|t| t == "*") {
        out.push_str(
            "_Your tool surface is unrestricted (or operator-default). \
Use whatever you need to complete the task._",
        );
    } else {
        let mut sorted: Vec<&String> = allowed.iter().collect();
        sorted.sort();
        for name in sorted {
            out.push_str("- `");
            out.push_str(name);
            out.push_str("`\n");
        }
        out.pop();
    }

    out.push_str("\n\n");
    out.push_str(NO_COORDINATOR_TOOLS_NOTE);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tools_renders_unrestricted_note() {
        let prompt = worker_system_prompt(WorkerPromptCtx::from_tools(&[]));
        assert!(prompt.contains("# WORKER ROLE"));
        assert!(prompt.contains("unrestricted"));
        // Always carries the no-coordinator-tools note.
        assert!(prompt.contains("do **not** have `TeamCreate`"));
    }

    #[test]
    fn explicit_tool_surface_lists_each_tool_sorted() {
        let tools: Vec<String> = vec![
            "WebFetch".into(),
            "BashTool".into(),
            "FileEdit".into(),
        ];
        let prompt = worker_system_prompt(WorkerPromptCtx::from_tools(&tools));
        // Sorted: BashTool < FileEdit < WebFetch
        let bash_idx = prompt.find("`BashTool`").unwrap();
        let edit_idx = prompt.find("`FileEdit`").unwrap();
        let fetch_idx = prompt.find("`WebFetch`").unwrap();
        assert!(bash_idx < edit_idx);
        assert!(edit_idx < fetch_idx);
    }

    #[test]
    fn wildcard_tool_surface_renders_unrestricted() {
        let tools: Vec<String> = vec!["*".into()];
        let prompt = worker_system_prompt(WorkerPromptCtx::from_tools(&tools));
        assert!(prompt.contains("unrestricted"));
        assert!(!prompt.contains("- `*`"));
    }

    #[test]
    fn scratchpad_section_toggles_on_enable_flag() {
        let tools: Vec<String> = vec!["TodoWrite".into()];

        let off = worker_system_prompt(WorkerPromptCtx {
            allowed_tools: &tools,
            scratchpad_enabled: false,
        });
        assert!(!off.contains("## Scratchpad"));

        let on = worker_system_prompt(WorkerPromptCtx {
            allowed_tools: &tools,
            scratchpad_enabled: true,
        });
        assert!(on.contains("## Scratchpad"));
        assert!(on.contains("`TodoWrite`"));
        // Worker scratchpad is for own state, NOT for cross-worker
        // coordination.
        assert!(on.contains("don't coordinate"));
    }

    #[test]
    fn key_sections_present() {
        let tools: Vec<String> = vec!["BashTool".into()];
        let prompt = worker_system_prompt(WorkerPromptCtx::from_tools(&tools));
        assert!(prompt.contains("# WORKER ROLE"));
        assert!(prompt.contains("## Output discipline"));
        assert!(prompt.contains("## Self-verification"));
        assert!(prompt.contains("## Tools available to you"));
        // The role declaration is explicit about not initiating
        // dialogue.
        assert!(prompt.contains("do **not** initiate"));
    }

    #[test]
    fn output_is_deterministic() {
        let tools: Vec<String> = vec!["A".into(), "B".into()];
        let ctx = WorkerPromptCtx {
            allowed_tools: &tools,
            scratchpad_enabled: true,
        };
        let a = worker_system_prompt(ctx.clone());
        let b = worker_system_prompt(ctx);
        assert_eq!(a, b, "build must be byte-deterministic");
    }
}
