//! Phase 67.E.1 — `program_phase` tool entry-point.
//!
//! Inputs: `phase_id` (required), optional `acceptance_override`.
//! The handler:
//!
//! 1. Reads `PHASES.md` via the project tracker to derive the goal
//!    description (sub-phase title + body).
//! 2. Validates the request through `DispatchGate`.
//! 3. Constructs a `Goal` with the dispatcher / origin metadata.
//! 4. Asks `AgentRegistry::admit` for a slot. Cap reached + queue
//!    enabled → returns `Queued`; cap reached + queue disabled →
//!    returns `Rejected`.
//! 5. Spawns the goal via `DriverOrchestrator::spawn_goal` if
//!    admitted; otherwise leaves the registry entry as `Queued` and
//!    relies on `release()` to surface it later.
//!
//! Returns `ProgramPhaseOutput` so the calling agent can echo
//! `goal_id` / `status` back to the chat.
//!
//! Hook + completion-router wiring lands in 67.F.x; this step
//! exposes the dispatch surface itself.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use nexo_agent_registry::{
    AdmitOutcome, AgentHandle, AgentRegistry, AgentRunStatus, AgentSnapshot,
};
use nexo_config::DispatchPolicy;
use nexo_driver_claude::{DispatcherIdentity, OriginChannel};
use nexo_driver_loop::DriverOrchestrator;
use nexo_driver_types::{AcceptanceCriterion, BudgetGuards, Goal, GoalId};
use nexo_project_tracker::tracker::ProjectTracker;
use serde::{Deserialize, Serialize};

use crate::policy_gate::{
    CapSnapshot, DispatchDenied, DispatchGate, DispatchKind, DispatchRequest,
};

/// Tool input.
#[derive(Clone, Debug, Deserialize)]
pub struct ProgramPhaseInput {
    pub phase_id: String,
    /// When set, replaces the auto-derived acceptance list. The
    /// auto-list is `cargo build --workspace && cargo test
    /// --workspace`; an operator override might pin a smaller crate
    /// for a fast iteration loop.
    #[serde(default)]
    pub acceptance_override: Option<Vec<AcceptanceCriterion>>,
    /// Bump default budget knobs. `None` keeps the orchestrator
    /// default.
    #[serde(default)]
    pub budget_override: Option<BudgetOverride>,
    /// B4 — hooks attached at dispatch time. Common usage from a
    /// chat tool call is `[{ id: "h1", on: "done", action: {
    /// kind: "notify_origin" } }]`. The handler stores them in the
    /// HookRegistry under the new goal id; the completion router
    /// fires them on goal transitions.
    #[serde(default)]
    pub hooks: Vec<crate::hooks::types::CompletionHook>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BudgetOverride {
    pub max_turns: Option<u32>,
    #[serde(default, with = "humantime_serde::option")]
    pub max_wall_time: Option<Duration>,
    pub max_tokens: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProgramPhaseOutput {
    Dispatched {
        goal_id: GoalId,
        phase_id: String,
    },
    Queued {
        goal_id: GoalId,
        phase_id: String,
        position: usize,
    },
    Rejected {
        phase_id: String,
        reason: String,
    },
    NotFound {
        phase_id: String,
    },
    Forbidden {
        phase_id: String,
        reason: String,
    },
    NotTracked,
}

#[derive(Debug, thiserror::Error)]
pub enum ProgramPhaseError {
    #[error("tracker: {0}")]
    Tracker(String),
    #[error("registry: {0}")]
    Registry(String),
}

/// Default budget — modest enough to fit a single dev session,
/// generous enough to ship a sub-phase. 67.E.1 does not yet read
/// `program_phase.yaml`; that wiring lands when the tool is
/// registered into the runtime by the binary (67.H.1).
pub fn apply_default_budget(ov: Option<BudgetOverride>) -> BudgetGuards {
    apply_budget_override(default_budget(), ov)
}

pub fn apply_default_acceptance() -> Vec<AcceptanceCriterion> {
    default_acceptance()
}

fn default_budget() -> BudgetGuards {
    BudgetGuards {
        max_turns: 40,
        max_wall_time: Duration::from_secs(60 * 60 * 4),
        max_tokens: 2_000_000,
        max_consecutive_denies: 3,
        max_consecutive_errors: 5,
    }
}

fn apply_budget_override(mut budget: BudgetGuards, ov: Option<BudgetOverride>) -> BudgetGuards {
    if let Some(o) = ov {
        if let Some(t) = o.max_turns {
            budget.max_turns = t;
        }
        if let Some(t) = o.max_wall_time {
            budget.max_wall_time = t;
        }
        if let Some(t) = o.max_tokens {
            budget.max_tokens = t;
        }
    }
    budget
}

/// Phase 75.1 — runtime project-type autodetect.
///
/// The previous default hardcoded `cargo build --workspace` +
/// `cargo test --workspace`, which:
///   * Wedged every Python / Node / Bash goal into a permanent
///     `needs_retry` loop because `cargo build` cannot succeed
///     without a `Cargo.toml`.
///   * Spent 30–60 s per turn rebuilding 200 crates for self-
///     modify goals against the nexo-rs workspace, even when the
///     goal's diff was a one-line tweak inside `crates/skills/`.
///
/// The detection now runs inside the worktree at acceptance-eval
/// time — the orchestrator `cd`s into the worktree before
/// executing each criterion, so `Cargo.toml` here means "the goal
/// is working in a Rust project". Order matters: Cargo first
/// (most common in this repo), then pyproject / setup, then
/// package.json, then cmake, fall back to `true` (auto-pass).
///
/// Operators that want stricter checks override per-goal via
/// `acceptance_override`, or per-phase via the markdown
/// `acceptance:` bullets in PHASES.md (parsed by the tracker).
fn default_acceptance() -> Vec<AcceptanceCriterion> {
    // One shell criterion — branches inside via test -f. Keeps the
    // contract "exit 0 = pass, anything else = retry" and lets the
    // existing AcceptanceEvaluator drive it without new variants.
    let script = r#"if [ -f Cargo.toml ]; then
  cargo build --workspace && cargo test --workspace
elif [ -f pyproject.toml ] || [ -f setup.py ]; then
  python3 -m pytest -q
elif [ -f package.json ]; then
  npm test --silent
elif [ -f CMakeLists.txt ]; then
  cmake -S . -B build && cmake --build build
else
  true
fi"#;
    vec![AcceptanceCriterion::shell(script)]
}

/// Phase 77.1 — render the prompt the Claude Code subprocess
/// receives as its first user message. Wraps the operator's raw
/// `(title, body)` from PHASES.md with a verb-first goal line, the
/// acceptance commands, and a HARD RULES checklist. Without this
/// scaffolding Claude routinely declared the goal "done" at turn 0
/// without touching a single file (because the body alone reads
/// like a status update, not an instruction).
pub fn build_goal_prompt(
    phase_id: &str,
    title: &str,
    body: Option<&str>,
    acceptance: &[AcceptanceCriterion],
) -> String {
    build_goal_prompt_with_followups(phase_id, title, body, acceptance, &[])
}

/// Same as [`build_goal_prompt`] but injects a "## Related
/// FOLLOWUPS" section between the sub-phase body and the
/// acceptance block. Use this when the sub-phase body references
/// `PR-N` codes — without the referenced FOLLOWUPS content the
/// LLM has no spec to implement against and routinely claims the
/// goal "done" at turn 0 (see Phase 78 incident: 26.z body was
/// "Tracks PR-3 in FOLLOWUPS.md … Blocked on …" → Claude wrote
/// nothing). Each follow-up is rendered as `### {code} — {title}`
/// followed by its body verbatim.
pub fn build_goal_prompt_with_followups(
    phase_id: &str,
    title: &str,
    body: Option<&str>,
    acceptance: &[AcceptanceCriterion],
    followups: &[(&str, &str, &str)],
) -> String {
    use std::fmt::Write as _;
    let mut p = String::new();
    let _ = writeln!(p, "# Goal");
    let _ = writeln!(p);
    let _ = writeln!(
        p,
        "Implement sub-phase **{phase_id}** of the active project. \
         Edit code, write new files, and run the project's build / \
         tests until the acceptance criteria below all pass. Do NOT \
         declare the goal done until the acceptance commands exit 0."
    );
    let _ = writeln!(p);
    let _ = writeln!(p, "## Phase {phase_id} — {title}");
    let _ = writeln!(p);
    if let Some(b) = body.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let _ = writeln!(p, "{b}");
        let _ = writeln!(p);
    }
    if !followups.is_empty() {
        let _ = writeln!(p, "## Related FOLLOWUPS spec");
        let _ = writeln!(p);
        let _ = writeln!(
            p,
            "The sub-phase body references the items below. Read \
             each one before deciding what code to write — they \
             carry the concrete acceptance for the work."
        );
        let _ = writeln!(p);
        for (code, fu_title, fu_body) in followups {
            let _ = writeln!(p, "### {code} — {fu_title}");
            let _ = writeln!(p);
            let _ = writeln!(p, "{}", fu_body.trim());
            let _ = writeln!(p);
        }
    }
    let _ = writeln!(p, "## Acceptance");
    let _ = writeln!(p);
    let _ = writeln!(
        p,
        "Every command below must exit with status 0 before the goal \
         can be marked done. The harness runs them automatically \
         after each turn; the failure output is fed back to you so \
         you can iterate."
    );
    let _ = writeln!(p);
    if acceptance.is_empty() {
        let _ = writeln!(
            p,
            "_(no acceptance criteria configured — use your best judgement)_"
        );
    } else {
        for c in acceptance {
            match c {
                AcceptanceCriterion::ShellCommand { command, .. } => {
                    let preview: String = command
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(120)
                        .collect();
                    let _ = writeln!(p, "- `{}`", preview);
                }
                AcceptanceCriterion::FileMatches { path, regex, .. } => {
                    let _ = writeln!(p, "- file `{path}` must match `{regex}`");
                }
                AcceptanceCriterion::Custom { name, .. } => {
                    let _ = writeln!(p, "- custom verifier: `{name}`");
                }
            }
        }
    }
    let _ = writeln!(p);
    let _ = writeln!(p, "## HARD RULES");
    let _ = writeln!(p);
    let _ = writeln!(
        p,
        "1. READ before you write. Open `PHASES.md`, `FOLLOWUPS.md`, \
         and the source files the description references; understand \
         the surrounding code before changing it.\n\
         2. MAKE THE CHANGES the description asks for. The body of \
         the sub-phase IS the spec — every requirement listed there \
         must land as code, tests, or docs in this turn or a follow-\
         up turn.\n\
         3. RUN the acceptance commands locally before claiming done. \
         A passing local run is the only signal that closes the goal.\n\
         4. NEVER claim done while the build is failing. If a turn \
         leaves errors, the next turn's job is to fix them, not to \
         re-state the plan.\n\
         5. KEEP the diff focused on this sub-phase. Other follow-ups \
         go in `FOLLOWUPS.md`, not in this commit."
    );
    p
}

/// Scan `body` for `PR-N` codes and return the matching follow-ups
/// from the tracker, preserving the order each code first appears
/// in the body. Silent fallback to `[]` on any tracker error — the
/// FOLLOWUPS appendix is best-effort, the prompt should still go
/// out without it.
async fn collect_referenced_followups(
    body: Option<&str>,
    tracker: &dyn ProjectTracker,
) -> Vec<nexo_project_tracker::FollowUp> {
    let Some(body) = body else { return vec![] };
    let codes = extract_pr_codes(body);
    if codes.is_empty() {
        return vec![];
    }
    let all = match tracker.followups().await {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let mut out = Vec::with_capacity(codes.len());
    for code in &codes {
        if let Some(fu) = all.iter().find(|f| f.code.eq_ignore_ascii_case(code)) {
            if !out.iter().any(|f: &nexo_project_tracker::FollowUp| {
                f.code.eq_ignore_ascii_case(code)
            }) {
                out.push(fu.clone());
            }
        }
    }
    out
}

fn extract_pr_codes(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if (bytes[i] == b'P' || bytes[i] == b'p')
            && (bytes[i + 1] == b'R' || bytes[i + 1] == b'r')
            && bytes[i + 2] == b'-'
        {
            let mut j = i + 3;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 3 {
                let prev_alphanum = i > 0 && bytes[i - 1].is_ascii_alphanumeric();
                if !prev_alphanum {
                    out.push(format!("PR-{}", &body[i + 3..j]));
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Dispatch implementation, decoupled from the `ToolHandler` trait
/// so tests can drive it directly. The runtime registration that
/// adapts this into a `nexo_core::ToolHandler` lands in 67.E.x once
/// the dispatcher identity / origin context plumbing is in place.
#[allow(clippy::too_many_arguments)]
pub async fn program_phase_dispatch(
    input: ProgramPhaseInput,
    tracker: &dyn ProjectTracker,
    orchestrator: Arc<DriverOrchestrator>,
    registry: Arc<AgentRegistry>,
    policy: &DispatchPolicy,
    require_trusted: bool,
    sender_trusted: bool,
    dispatcher: DispatcherIdentity,
    origin: Option<OriginChannel>,
    caps: CapSnapshot,
    hook_registry: Option<Arc<crate::hooks::HookRegistry>>,
) -> Result<ProgramPhaseOutput, ProgramPhaseError> {
    // Tracker: fetch the sub-phase. Missing PHASES.md → NotTracked.
    let sub = match tracker.phase_detail(&input.phase_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Ok(ProgramPhaseOutput::NotFound {
                phase_id: input.phase_id,
            });
        }
        Err(nexo_project_tracker::TrackerError::NotTracked(_)) => {
            return Ok(ProgramPhaseOutput::NotTracked);
        }
        Err(e) => return Err(ProgramPhaseError::Tracker(e.to_string())),
    };

    let request = DispatchRequest {
        kind: DispatchKind::Write,
        phase_id: &input.phase_id,
        policy,
        require_trusted,
        sender_trusted,
        caps,
    };
    if let Err(denied) = DispatchGate::check(&request) {
        return Ok(match denied {
            DispatchDenied::CapabilityNone
            | DispatchDenied::CapabilityReadOnly
            | DispatchDenied::SenderNotTrusted
            | DispatchDenied::PhaseForbidden(_)
            | DispatchDenied::PhaseNotAllowed(_) => ProgramPhaseOutput::Forbidden {
                phase_id: input.phase_id,
                reason: denied.to_string(),
            },
            DispatchDenied::DispatcherCapReached { .. }
            | DispatchDenied::SenderCapReached { .. }
            | DispatchDenied::GlobalCapReached { .. } => ProgramPhaseOutput::Rejected {
                phase_id: input.phase_id,
                reason: denied.to_string(),
            },
        });
    }

    // Acceptance precedence:
    //   1. caller-supplied `acceptance_override`
    //   2. acceptance bullets parsed out of the sub-phase body
    //   3. workspace defaults (cargo build + cargo test)
    let acceptance = if let Some(ov) = input.acceptance_override.clone() {
        ov
    } else if let Some(parsed) = sub.acceptance.clone() {
        parsed.into_iter().map(AcceptanceCriterion::shell).collect()
    } else {
        default_acceptance()
    };
    let budget = apply_budget_override(default_budget(), input.budget_override.clone());

    // Phase 77.1 — directive prompt. The previous form just
    // concatenated `title + body`, leaving Claude to infer the verb
    // (`implement`, `add`, `fix`). On vague phase descriptions
    // ("Tracks PR-3 in FOLLOWUPS.md. … Blocked on …") Claude would
    // interpret the goal as already-done and exit at turn 0
    // without writing a single file. The wrapped form below states
    // the verb ("Implement"), shows the acceptance commands the
    // run will be judged against, and ends with a HARD RULES list
    // that mirrors the operator's mental model so Claude does not
    // declare done until the build / tests it sees here actually
    // pass.
    //
    // Phase 78.3 — when the body cross-references `PR-N` items
    // tracked in FOLLOWUPS.md, splice the referenced entries into
    // the prompt verbatim. Without this Claude only sees the
    // pointer ("Tracks PR-3") and has no spec to implement.
    let referenced = collect_referenced_followups(sub.body.as_deref(), tracker).await;
    let referenced_view: Vec<(&str, &str, &str)> = referenced
        .iter()
        .map(|f| (f.code.as_str(), f.title.as_str(), f.body.as_str()))
        .collect();
    let description = build_goal_prompt_with_followups(
        &input.phase_id,
        &sub.title,
        sub.body.as_deref(),
        &acceptance,
        &referenced_view,
    );

    // B1 — stamp origin + dispatcher into goal.metadata so
    // attempt.rs can lift them into the SessionBinding when the
    // first turn lands. Persists across daemon restart so reattach
    // can find the chat that triggered the goal.
    let mut metadata = serde_json::Map::new();
    if let Some(o) = &origin {
        metadata.insert(
            "origin_channel".into(),
            serde_json::to_value(o).unwrap_or(serde_json::Value::Null),
        );
    }
    metadata.insert(
        "dispatcher".into(),
        serde_json::to_value(&dispatcher).unwrap_or(serde_json::Value::Null),
    );

    // Phase 76 — when the active tracker root is itself a git repo
    // (i.e. `init_project` ran `git init` on a fresh project, or
    // the operator pointed `set_active_workspace` at a stand-alone
    // checkout), stamp it as the per-goal source so the
    // orchestrator clones a worktree from THAT repo instead of the
    // daemon's outer source root. Without this, scaffolding a
    // /tmp/calc-rust project would still pull a worktree of nexo-rs
    // and Claude would have no idea where to write.
    if let Some(root) = tracker.root() {
        if root.join(".git").exists() {
            metadata.insert(
                "worktree.source_repo".into(),
                serde_json::Value::String(root.display().to_string()),
            );
        }
    }

    let goal = Goal {
        id: GoalId::new(),
        description,
        acceptance,
        budget,
        workspace: None,
        metadata,
    };
    let goal_id = goal.id;

    // Register in agent-registry. Cap-reached + queue enabled → goal
    // is parked as Queued; the orchestrator's release() callback
    // pops the next-up via promote_queued. Cap-reached + queue
    // disabled → Rejected via DispatchGate above; we should never
    // see Rejected here, but match it defensively.
    let handle = AgentHandle {
        goal_id,
        phase_id: input.phase_id.clone(),
        status: AgentRunStatus::Running,
        origin: origin.clone(),
        dispatcher: Some(dispatcher.clone()),
        started_at: Utc::now(),
        finished_at: None,
        snapshot: AgentSnapshot {
            max_turns: goal.budget.max_turns,
            ..AgentSnapshot::default()
        },
    };
    let outcome = registry
        .admit(handle, caps.queue_when_full)
        .await
        .map_err(|e| ProgramPhaseError::Registry(e.to_string()))?;

    // B4 — attach hooks before spawn so the completion router
    // sees them when the goal terminates. Done for both Admitted
    // (will spawn) and Queued (will spawn after promote).
    if let Some(hr) = &hook_registry {
        for hook in input.hooks.clone() {
            hr.add(goal_id, hook);
        }
    }
    match outcome {
        AdmitOutcome::Admitted => {
            registry.set_max_turns(goal_id, goal.budget.max_turns);
            // Fire-and-forget. Caller does not await the join handle —
            // the registry + driver events are how we observe the run.
            std::mem::drop(orchestrator.clone().spawn_goal(goal));
            Ok(ProgramPhaseOutput::Dispatched {
                goal_id,
                phase_id: input.phase_id,
            })
        }
        AdmitOutcome::Queued { position } => Ok(ProgramPhaseOutput::Queued {
            goal_id,
            phase_id: input.phase_id,
            position,
        }),
        AdmitOutcome::Rejected => Ok(ProgramPhaseOutput::Rejected {
            phase_id: input.phase_id,
            reason: "registry rejected (cap reached + queue disabled)".into(),
        }),
    }
}

#[cfg(test)]
mod default_acceptance_tests {
    //! Phase 75.2 — the default acceptance script branches on
    //! project-marker files inside the worktree. These tests run
    //! the script through `bash -c` (same shell the orchestrator
    //! uses) inside a tempdir per case and assert the right
    //! command was selected. Each case also asserts the script
    //! exit-code matches the expected outcome ("0 = pass" /
    //! "non-zero = retry") given the marker file alone — no
    //! actual cargo / pytest / npm runs because we redirect the
    //! commands through `PATH` to a tiny stub that records the
    //! invocation.

    use super::*;
    use std::fs;

    fn script() -> String {
        match &default_acceptance()[0] {
            AcceptanceCriterion::ShellCommand { command, .. } => command.clone(),
            other => panic!("expected ShellCommand criterion, got {other:?}"),
        }
    }

    /// Build a stub `bin/` directory whose tools all `echo CALLED <name> "$@"`
    /// and exit 0. Prepend it to PATH so the script picks it up
    /// instead of the host's real cargo / pytest / npm.
    fn stub_path(dir: &std::path::Path, names: &[&str]) -> String {
        let bin = dir.join("__stub_bin__");
        fs::create_dir_all(&bin).unwrap();
        for n in names {
            let p = bin.join(n);
            fs::write(&p, format!("#!/bin/sh\necho CALLED {n} \"$@\"\nexit 0\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = fs::metadata(&p).unwrap().permissions();
                perm.set_mode(0o755);
                fs::set_permissions(&p, perm).unwrap();
            }
        }
        let host_path = std::env::var("PATH").unwrap_or_default();
        format!("{}:{}", bin.display(), host_path)
    }

    fn run(dir: &std::path::Path, path: &str) -> (i32, String) {
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(script())
            .current_dir(dir)
            .env("PATH", path)
            .output()
            .unwrap();
        let code = out.status.code().unwrap_or(-1);
        let mut s = String::from_utf8_lossy(&out.stdout).to_string();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        (code, s)
    }

    #[test]
    fn cargo_marker_runs_cargo_build_and_test() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let path = stub_path(dir.path(), &["cargo"]);
        let (code, out) = run(dir.path(), &path);
        assert_eq!(code, 0);
        assert!(out.contains("CALLED cargo build --workspace"), "got: {out}");
        assert!(out.contains("CALLED cargo test --workspace"), "got: {out}");
    }

    #[test]
    fn pyproject_marker_runs_pytest() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        let path = stub_path(dir.path(), &["python3"]);
        let (code, out) = run(dir.path(), &path);
        assert_eq!(code, 0);
        assert!(out.contains("CALLED python3 -m pytest -q"), "got: {out}");
    }

    #[test]
    fn setup_py_marker_also_runs_pytest() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("setup.py"), "").unwrap();
        let path = stub_path(dir.path(), &["python3"]);
        let (code, out) = run(dir.path(), &path);
        assert_eq!(code, 0);
        assert!(out.contains("CALLED python3 -m pytest -q"), "got: {out}");
    }

    #[test]
    fn package_json_marker_runs_npm_test() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        let path = stub_path(dir.path(), &["npm"]);
        let (code, out) = run(dir.path(), &path);
        assert_eq!(code, 0);
        assert!(out.contains("CALLED npm test --silent"), "got: {out}");
    }

    #[test]
    fn cmake_marker_runs_cmake_build() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("CMakeLists.txt"), "").unwrap();
        let path = stub_path(dir.path(), &["cmake"]);
        let (code, out) = run(dir.path(), &path);
        assert_eq!(code, 0);
        assert!(out.contains("CALLED cmake -S . -B build"), "got: {out}");
        assert!(out.contains("CALLED cmake --build build"), "got: {out}");
    }

    #[test]
    fn empty_dir_falls_back_to_true_and_passes() {
        // No markers → script runs `true` → exit 0, pass. This
        // is the path that lets a "scaffold a fresh project"
        // goal succeed without forcing the operator to declare
        // acceptance bullets in PHASES.md upfront.
        let dir = tempfile::tempdir().unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        let (code, out) = run(dir.path(), &path);
        assert_eq!(code, 0, "no markers should auto-pass; output: {out}");
    }

    #[test]
    fn cargo_marker_takes_precedence_over_python() {
        // Mixed repo: a Rust workspace that also has a python
        // sub-tool. Cargo is the project's primary build, so
        // it wins. Operators can still override per-goal.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        let path = stub_path(dir.path(), &["cargo", "python3"]);
        let (_code, out) = run(dir.path(), &path);
        assert!(out.contains("CALLED cargo"), "cargo should win: {out}");
        assert!(
            !out.contains("CALLED python3"),
            "python3 should NOT run: {out}"
        );
    }
}

#[cfg(test)]
mod build_goal_prompt_tests {
    use super::*;

    fn shell(cmd: &str) -> AcceptanceCriterion {
        AcceptanceCriterion::shell(cmd)
    }

    #[test]
    fn includes_phase_id_title_body_and_imperative_verb() {
        let p = build_goal_prompt(
            "26.z",
            "tunnel.url integration",
            Some("Wire the accessor."),
            &[shell("cargo build"), shell("cargo test --lib pairing")],
        );
        assert!(p.contains("26.z"), "phase_id missing: {p}");
        assert!(p.contains("tunnel.url integration"));
        assert!(p.contains("Wire the accessor."));
        assert!(
            p.to_lowercase().contains("implement"),
            "imperative verb missing: {p}"
        );
        assert!(p.contains("`cargo build`"));
        assert!(p.contains("`cargo test --lib pairing`"));
        assert!(p.contains("HARD RULES"));
    }

    #[test]
    fn empty_body_omits_the_body_block_but_keeps_acceptance() {
        let p = build_goal_prompt("1.1", "Initial scaffold", None, &[shell("true")]);
        assert!(p.contains("Initial scaffold"));
        assert!(p.contains("`true`"));
    }

    #[test]
    fn empty_acceptance_falls_back_to_best_judgement_note() {
        let p = build_goal_prompt("9.9", "explore", Some("look around"), &[]);
        assert!(p.contains("no acceptance criteria"));
    }

    #[test]
    fn file_match_and_custom_criteria_render_in_table() {
        let p = build_goal_prompt(
            "9.9",
            "x",
            None,
            &[
                AcceptanceCriterion::FileMatches {
                    path: "README.md".into(),
                    regex: "Phase 77".into(),
                    required: true,
                },
                AcceptanceCriterion::Custom {
                    name: "audit".into(),
                    args: serde_json::json!({}),
                },
            ],
        );
        assert!(p.contains("README.md"));
        assert!(p.contains("Phase 77"));
        assert!(p.contains("audit"));
    }

    #[test]
    fn shell_criterion_with_multiline_command_truncates_to_first_line() {
        let p = build_goal_prompt(
            "9.9",
            "x",
            None,
            &[shell("if [ -f Cargo.toml ]; then\n  cargo build\nfi")],
        );
        assert!(p.contains("if [ -f Cargo.toml ]"));
        // Ensure the second line of the script doesn't bleed into
        // the prompt as its own list item.
        assert!(!p.contains("\n  cargo build\nfi"));
    }
}
