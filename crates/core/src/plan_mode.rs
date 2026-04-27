//! Phase 79.1 — plan-mode state, refusal, and tool classification.
//!
//! Plan mode is a per-goal toggle that puts the agent into a read-only
//! "exploration + design" phase. While active, every mutating tool call
//! is short-circuited at the dispatcher with a [`PlanModeRefusal`], and
//! the model is expected to call `ExitPlanMode { final_plan }` once it
//! has a coherent plan. Operator approval (delivered via the pairing
//! channel that owns the goal) unlocks plan mode and lets the model
//! resume mutating work.
//!
//! Design references:
//!   * `claude-code-leak/src/tools/EnterPlanModeTool/EnterPlanModeTool.ts`
//!   * `claude-code-leak/src/tools/ExitPlanModeTool/ExitPlanModeV2Tool.ts`
//!   * `claude-code-leak/src/utils/permissions/permissionSetup.ts:1458-1489`
//!     (`prepareContextForPlanMode` saves `prePlanMode`, restored on exit).
//!   * `research/src/acp/approval-classifier.ts:24-38` — taxonomy peer for
//!     [`ToolKind`].
//!
//! Centralised gate: every mutating tool MUST appear in
//! [`MUTATING_TOOLS`] and every read-only tool in [`READ_ONLY_TOOLS`].
//! [`assert_registry_classified`] is invoked at boot to refuse start-up
//! if a registered tool falls in neither bucket — ensures new tools are
//! never silently exempt from plan-mode gating.

use serde::{Deserialize, Serialize};

/// Restore target captured when plan mode is entered.
///
/// Mirrors the leak's `prePlanMode` field
/// (`permissionSetup.ts:1458-1489`). Today we only track the binary
/// "was-plan / was-not-plan" axis, but the field is a struct enum so
/// future modes (acceptEdits, bypassPermissions) can be added without
/// breaking existing serialised state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriorMode {
    /// Default permission mode — reapply standard policy on exit.
    Default,
}

impl Default for PriorMode {
    fn default() -> Self {
        PriorMode::Default
    }
}

/// Why plan mode was entered. Surfaces in the
/// `[plan-mode] entered ... reason: <…>` notify line and inside
/// [`PlanModeRefusal::entered_reason`] so the model can react with the
/// right framing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanModeReason {
    /// Model called `EnterPlanMode { reason }` voluntarily.
    ModelRequested {
        /// Optional free-form reason from the model.
        reason: Option<String>,
    },
    /// Operator forced plan mode via channel command (Phase 26 pairing).
    OperatorRequested,
    /// Soft-dep on Phase 77.8: dispatcher pre-empted a destructive
    /// command and auto-entered plan mode. `tripped_check` carries the
    /// classifier verdict (e.g. `"rm -rf $HOME"`, `"sed -i without
    /// validated path"`).
    AutoDestructive {
        /// 77.8 destructive-classifier verdict that triggered the
        /// auto-enter.
        tripped_check: String,
    },
}

/// Plan-mode state machine kept on the goal's [`AgentContext`] and
/// mirrored in `agent_registry.goals.plan_mode` (column added in step 4).
///
/// SQLite is canonical so daemon restart preserves the state via Phase
/// 71 reattach; the in-memory copy is a hot cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PlanModeState {
    #[default]
    Off,
    On {
        /// Unix-seconds timestamp the state flipped to On.
        entered_at: i64,
        /// Why plan mode was entered.
        reason: PlanModeReason,
        /// Mode to restore on `ExitPlanMode` approval.
        prior_mode: PriorMode,
    },
}

impl PlanModeState {
    pub fn is_on(&self) -> bool {
        matches!(self, PlanModeState::On { .. })
    }

    pub fn is_off(&self) -> bool {
        matches!(self, PlanModeState::Off)
    }

    /// Convenience constructor for `On` states.
    pub fn on(entered_at: i64, reason: PlanModeReason) -> Self {
        PlanModeState::On {
            entered_at,
            reason,
            prior_mode: PriorMode::default(),
        }
    }
}

/// Coarse classification surfaced inside [`PlanModeRefusal`] so the
/// model can react with the right framing without parsing tool names.
///
/// Peer reference: `research/src/acp/approval-classifier.ts:24-32`
/// (`AcpApprovalClass` — same intent, larger surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Bash / shell command. Mutating subset gated; read-only subset
    /// allowed (Phase 77.8 classifier provides the verdict at runtime).
    Bash,
    /// `FileWrite`, `FileEdit`, `NotebookEdit`.
    FileEdit,
    /// Plugin outbound — WhatsApp/Telegram/email send, browser
    /// click/type/navigate, etc.
    Outbound,
    /// `delegate_to`, future `TeamCreate` (79.6).
    Delegate,
    /// `program_phase`, `dispatch_followup`.
    Dispatch,
    /// 79.7 `ScheduleCron`, future schedulers.
    Schedule,
    /// 79.10 `Config { op: apply }`.
    Config,
    /// `FileRead`, `Glob`, `Grep`, `WebSearch`, MCP read tools, plan
    /// mode tools themselves, `AskUserQuestion`, `Sleep`, etc.
    ReadOnly,
}

impl ToolKind {
    pub fn is_mutating(self) -> bool {
        !matches!(self, ToolKind::ReadOnly)
    }
}

/// Structured refusal returned when a mutating tool is invoked while
/// plan mode is on. The dispatcher serialises this as a
/// `tool_result { is_error: true }` so all four LLM provider clients
/// (Anthropic, MiniMax, OpenAI-compat, Gemini) classify it identically.
///
/// Diff vs leak: leak returns `{result: false, message: '…',
/// errorCode: 1}` from `validateInput` (`ExitPlanModeV2Tool.ts:213-218`)
/// — a string. We carry structured fields so the model can reason
/// without parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanModeRefusal {
    /// Tool name the model attempted (e.g. `"FileWrite"`).
    pub tool_name: String,
    /// Coarse classification.
    pub tool_kind: ToolKind,
    /// One-line directive for the model.
    pub hint: &'static str,
    /// Unix-seconds timestamp plan mode was entered. Lets the model
    /// see how long it has been planning.
    pub entered_at: i64,
    /// Why plan mode is currently active.
    pub entered_reason: PlanModeReason,
}

impl PlanModeRefusal {
    pub const HINT: &'static str = "Call ExitPlanMode { final_plan } when the plan is ready.";
}

/// Frozen system-prompt suffix injected on every turn while plan
/// mode is on. The string is intentionally `&'static` (no
/// timestamps, no per-goal substitutions) so the Anthropic prompt
/// cache stays warm across turns — see
/// `claude-code-leak/src/services/api/promptCacheBreakDetection.ts`
/// for the inverse: cache-misses caused by sneaking variable text
/// into "stable" blocks.
pub const PLAN_MODE_SYSTEM_HINT: &str = "[plan-mode] Active. Read-only exploration. Mutating tools refuse with PlanModeRefusal. Call ExitPlanMode { final_plan } when ready.";

/// Return the canonical plan-mode hint when plan mode is active,
/// `None` otherwise. Callers append this to the per-turn system
/// prompt block (e.g. `channel_meta` in
/// `crates/core/src/agent/prompt_assembly.rs`).
pub fn plan_mode_system_hint(state: &PlanModeState) -> Option<&'static str> {
    state.is_on().then_some(PLAN_MODE_SYSTEM_HINT)
}

/// Acceptance verdict surfaced via the `[plan-mode] acceptance: ...`
/// notify line. The variants match the two terminal states a Phase 75
/// acceptance run can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptanceOutcome {
    Pass,
    Fail,
}

impl AcceptanceOutcome {
    fn as_str(self) -> &'static str {
        match self {
            AcceptanceOutcome::Pass => "pass",
            AcceptanceOutcome::Fail => "fail",
        }
    }
}

/// Format the canonical `[plan-mode] entered ...` notify line. Frozen
/// shape: any change here breaks operator-side parsers + dashboards.
pub fn format_notify_entered(entered_at: i64, reason: &PlanModeReason) -> String {
    let ts = chrono::DateTime::from_timestamp(entered_at, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_else(|| entered_at.to_string());
    let reason_str = match reason {
        PlanModeReason::ModelRequested { reason: Some(r) } => format!("model: {r}"),
        PlanModeReason::ModelRequested { reason: None } => "model".to_string(),
        PlanModeReason::OperatorRequested => "operator".to_string(),
        PlanModeReason::AutoDestructive { tripped_check } => {
            format!("auto-destructive: {tripped_check}")
        }
    };
    format!("[plan-mode] entered at {ts} — reason: {reason_str}")
}

/// Format the canonical `[plan-mode] exited — plan: ...` notify
/// line. The plan body is truncated to 200 chars with an ellipsis;
/// the full body lives in the Phase 72 turn log (referenced by index).
pub fn format_notify_exited(plan: &str, turn_log_index: u64) -> String {
    let snippet: String = plan.chars().take(200).collect();
    let ellipsis = if plan.chars().count() > 200 {
        "…"
    } else {
        ""
    };
    format!(
        "[plan-mode] exited — plan: {snippet}{ellipsis} (full plan in turn log #{turn_log_index})"
    )
}

/// Format the canonical `[plan-mode] acceptance: pass|fail (...)`
/// notify line. `summary` is rendered verbatim and SHOULD be one
/// short line — operators read this on the pairing channel.
pub fn format_notify_acceptance(outcome: AcceptanceOutcome, summary: &str) -> String {
    format!(
        "[plan-mode] acceptance: {} ({})",
        outcome.as_str(),
        summary.trim()
    )
}

/// Format the canonical `[plan-mode] refused tool=<name> kind=<kind>`
/// notify line — emitted when a mutating call short-circuits at the
/// dispatcher gate.
pub fn format_notify_refused(refusal: &PlanModeRefusal) -> String {
    let kind = match refusal.tool_kind {
        ToolKind::Bash => "bash",
        ToolKind::FileEdit => "file_edit",
        ToolKind::Outbound => "outbound",
        ToolKind::Delegate => "delegate",
        ToolKind::Dispatch => "dispatch",
        ToolKind::Schedule => "schedule",
        ToolKind::Config => "config",
        ToolKind::ReadOnly => "read_only",
    };
    format!(
        "[plan-mode] refused tool={tool} kind={kind}",
        tool = refusal.tool_name
    )
}

/// Canonical mutating-tool list. Adding a tool to the registry without
/// listing it here (or in [`READ_ONLY_TOOLS`]) makes
/// [`assert_registry_classified`] panic at boot.
///
/// Forward references (entries already valid even when their owning
/// sub-phase has not shipped — the gate just never fires for an
/// unregistered name):
///   * 79.7 `ScheduleCron`, 79.8 `RemoteTrigger`, 79.10
///     `Config { op: apply }`.
///   * 79.13 `NotebookEdit`, 79.6 `TeamCreate`.
pub const MUTATING_TOOLS: &[&str] = &[
    // Bash is special-cased — see `is_mutating_tool_call` below.
    "Bash",
    // File edits.
    "FileWrite",
    "FileEdit",
    "NotebookEdit",
    // Dispatch / programming.
    "program_phase",
    "delegate_to",
    "dispatch_followup",
    "TeamCreate",
    "TeamDelete",
    // Schedulers + remote.
    "ScheduleCron",
    "cron_create",
    "cron_delete",
    "cron_pause",
    "cron_resume",
    "RemoteTrigger",
    // Config self-edit (79.10) — only `apply` op is mutating; the gate
    // resolves the op at call time. `Config` as a name is listed here
    // so an unclassified registration fails the boot assert.
    "Config",
];

/// Canonical read-only tool list. Tools not in either bucket trigger a
/// boot panic. New plan-mode tools live here so they remain callable
/// while plan mode is on.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "FileRead",
    "Glob",
    "Grep",
    "WebSearch",
    "WebFetch",
    "ListMcpResources",
    "ReadMcpResource",
    "list_mcp_resources",
    "read_mcp_resource",
    "ToolSearch",
    "AskUserQuestion",
    "Sleep",
    "EnterPlanMode",
    "ExitPlanMode",
    // Phase 79.4 — intra-turn scratch list. Mutates only the
    // per-goal todos cache; never touches the workspace, broker, or
    // external state.
    "TodoWrite",
    // Phase 79.3 — terminal output validator. Pure validate-and-echo;
    // never touches any external state.
    "SyntheticOutput",
    // Phase 79.7 — cron list reads the schedule store.
    "cron_list",
    // Memory + observability tools that read but never write.
    "memory_search",
    "agent_query",
    "agent_turns_tail",
    "session_logs",
    "what_do_i_know",
    "who_am_i",
    "my_stats",
];

/// Decides whether `tool_name` is currently subject to plan-mode
/// gating. `Bash` returns `Some(ToolKind::Bash)` regardless — the
/// dispatcher pairs the verdict with the Phase 77.8 destructive
/// classifier (when shipped) to decide whether to actually refuse.
pub fn classify_tool(tool_name: &str) -> Option<ToolKind> {
    if MUTATING_TOOLS.contains(&tool_name) {
        return Some(match tool_name {
            "Bash" => ToolKind::Bash,
            "FileWrite" | "FileEdit" | "NotebookEdit" => ToolKind::FileEdit,
            "delegate_to" | "TeamCreate" | "TeamDelete" => ToolKind::Delegate,
            "program_phase" | "dispatch_followup" => ToolKind::Dispatch,
            "ScheduleCron" => ToolKind::Schedule,
            "RemoteTrigger" => ToolKind::Outbound,
            "Config" => ToolKind::Config,
            _ => ToolKind::Outbound, // future plugin outbound names
        });
    }
    if READ_ONLY_TOOLS.contains(&tool_name) {
        return Some(ToolKind::ReadOnly);
    }
    // Plugin outbound names follow the `<channel>.<verb>` convention
    // (e.g. `whatsapp.send`, `browser.click`). Treat any name with a
    // dot as an outbound mutator so the boot assert does not need to
    // enumerate every plugin verb.
    if tool_name.contains('.') {
        return Some(ToolKind::Outbound);
    }
    None
}

/// Centralised gate consulted by `DispatchGate::check`. Returns
/// `Some(refusal)` when the call must be blocked, `None` to let it
/// through.
///
/// Bash short-circuit: when the 77.8 destructive classifier ships,
/// callers will pass `bash_is_mutating: Some(verdict)`. Until then
/// (`None` is the only value), Bash is treated as mutating in plan
/// mode — fail-safe behaviour matches the spec ("default to blocking
/// if the classifier returns Unknown").
pub fn gate_tool_call(
    state: &PlanModeState,
    tool_name: &str,
    bash_is_mutating: Option<bool>,
) -> Option<PlanModeRefusal> {
    let PlanModeState::On {
        entered_at,
        reason,
        prior_mode: _,
    } = state
    else {
        return None;
    };
    let kind = classify_tool(tool_name)?;
    let blocked = match kind {
        ToolKind::Bash => bash_is_mutating.unwrap_or(true),
        ToolKind::ReadOnly => false,
        _ => true,
    };
    if !blocked {
        return None;
    }
    Some(PlanModeRefusal {
        tool_name: tool_name.to_string(),
        tool_kind: kind,
        hint: PlanModeRefusal::HINT,
        entered_at: *entered_at,
        entered_reason: reason.clone(),
    })
}

/// Boot-time guard: every name in `registered` must appear in
/// [`MUTATING_TOOLS`] or [`READ_ONLY_TOOLS`], OR follow the
/// `<channel>.<verb>` outbound convention. A tool that slips through
/// unclassified would silently bypass plan-mode gating, so we refuse
/// to start.
///
/// Returns the offending names instead of panicking so callers can
/// decide between hard-fail (production) and warn-only (dev fixtures).
pub fn unclassified_tools<I, S>(registered: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    registered
        .into_iter()
        .filter(|name| classify_tool(name.as_ref()).is_none())
        .map(|name| name.as_ref().to_string())
        .collect()
}

/// Hard-fail variant for production boot.
///
/// # Panics
/// If any registered tool is unclassified.
pub fn assert_registry_classified<I, S>(registered: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let bad = unclassified_tools(registered);
    if !bad.is_empty() {
        panic!(
            "plan_mode: {} tool(s) registered without mutating/read-only \
             classification: {:?}. Add them to MUTATING_TOOLS or \
             READ_ONLY_TOOLS in crates/core/src/plan_mode.rs, or use the \
             `<channel>.<verb>` outbound convention.",
            bad.len(),
            bad
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_default_off() {
        assert!(PlanModeState::default().is_off());
    }

    #[test]
    fn state_serde_roundtrip_on() {
        let state = PlanModeState::on(
            1_700_000_000,
            PlanModeReason::ModelRequested {
                reason: Some("explore auth flow".into()),
            },
        );
        let json = serde_json::to_string(&state).unwrap();
        let back: PlanModeState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }

    #[test]
    fn state_serde_roundtrip_auto_destructive() {
        let state = PlanModeState::on(
            1_700_000_000,
            PlanModeReason::AutoDestructive {
                tripped_check: "rm -rf $HOME".into(),
            },
        );
        let json = serde_json::to_string(&state).unwrap();
        let back: PlanModeState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }

    #[test]
    fn classify_known_mutators() {
        assert_eq!(classify_tool("Bash"), Some(ToolKind::Bash));
        assert_eq!(classify_tool("FileEdit"), Some(ToolKind::FileEdit));
        assert_eq!(classify_tool("delegate_to"), Some(ToolKind::Delegate));
        assert_eq!(classify_tool("ScheduleCron"), Some(ToolKind::Schedule));
        assert_eq!(classify_tool("Config"), Some(ToolKind::Config));
    }

    #[test]
    fn classify_known_read_only() {
        assert_eq!(classify_tool("FileRead"), Some(ToolKind::ReadOnly));
        assert_eq!(classify_tool("EnterPlanMode"), Some(ToolKind::ReadOnly));
        assert_eq!(classify_tool("ExitPlanMode"), Some(ToolKind::ReadOnly));
        assert_eq!(classify_tool("AskUserQuestion"), Some(ToolKind::ReadOnly));
    }

    #[test]
    fn classify_outbound_dotted_convention() {
        assert_eq!(classify_tool("whatsapp.send"), Some(ToolKind::Outbound));
        assert_eq!(classify_tool("browser.click"), Some(ToolKind::Outbound));
    }

    #[test]
    fn classify_unknown_returns_none() {
        assert_eq!(classify_tool("totally_unregistered_thing"), None);
    }

    #[test]
    fn gate_off_lets_everything_through() {
        let state = PlanModeState::Off;
        assert!(gate_tool_call(&state, "FileEdit", None).is_none());
        assert!(gate_tool_call(&state, "Bash", Some(true)).is_none());
    }

    #[test]
    fn gate_on_blocks_mutators() {
        let state = PlanModeState::on(123, PlanModeReason::ModelRequested { reason: None });
        let refusal = gate_tool_call(&state, "FileEdit", None).unwrap();
        assert_eq!(refusal.tool_kind, ToolKind::FileEdit);
        assert_eq!(refusal.entered_at, 123);
    }

    #[test]
    fn gate_on_allows_read_only() {
        let state = PlanModeState::on(123, PlanModeReason::ModelRequested { reason: None });
        assert!(gate_tool_call(&state, "FileRead", None).is_none());
        assert!(gate_tool_call(&state, "ExitPlanMode", None).is_none());
    }

    #[test]
    fn gate_bash_with_classifier_unknown_blocks() {
        // 77.8 not yet shipped: caller passes None → fail-safe block.
        let state = PlanModeState::on(123, PlanModeReason::ModelRequested { reason: None });
        let refusal = gate_tool_call(&state, "Bash", None).unwrap();
        assert_eq!(refusal.tool_kind, ToolKind::Bash);
    }

    #[test]
    fn gate_bash_read_only_passes() {
        let state = PlanModeState::on(123, PlanModeReason::ModelRequested { reason: None });
        assert!(gate_tool_call(&state, "Bash", Some(false)).is_none());
    }

    #[test]
    fn gate_bash_destructive_blocks() {
        let state = PlanModeState::on(123, PlanModeReason::ModelRequested { reason: None });
        let refusal = gate_tool_call(&state, "Bash", Some(true)).unwrap();
        assert_eq!(refusal.tool_kind, ToolKind::Bash);
    }

    #[test]
    fn unclassified_tools_reports_missing() {
        let names = ["FileEdit", "weird_new_tool", "Glob"];
        let bad = unclassified_tools(names);
        assert_eq!(bad, vec!["weird_new_tool".to_string()]);
    }

    #[test]
    fn assert_passes_for_known_registry() {
        // Smoke-test the canonical surface — every known name must
        // classify. Failures here would indicate a regression in the
        // const lists themselves.
        let names: Vec<&str> = MUTATING_TOOLS
            .iter()
            .chain(READ_ONLY_TOOLS.iter())
            .copied()
            .collect();
        assert_registry_classified(names);
    }

    #[test]
    #[should_panic(expected = "plan_mode:")]
    fn assert_panics_on_unclassified() {
        assert_registry_classified(["totally_unregistered_thing"]);
    }

    #[test]
    fn system_hint_returns_string_when_on() {
        let state = PlanModeState::on(1, PlanModeReason::ModelRequested { reason: None });
        assert_eq!(plan_mode_system_hint(&state), Some(PLAN_MODE_SYSTEM_HINT));
    }

    #[test]
    fn system_hint_returns_none_when_off() {
        assert_eq!(plan_mode_system_hint(&PlanModeState::Off), None);
    }

    #[test]
    fn system_hint_is_a_frozen_static() {
        // Sanity check: the constant body must contain the canonical
        // tokens so the model recognises it across providers and
        // the prompt cache treats it as stable.
        assert!(PLAN_MODE_SYSTEM_HINT.contains("[plan-mode]"));
        assert!(PLAN_MODE_SYSTEM_HINT.contains("ExitPlanMode"));
        assert!(PLAN_MODE_SYSTEM_HINT.contains("PlanModeRefusal"));
    }

    #[test]
    fn notify_entered_model_no_reason() {
        let s = format_notify_entered(
            1_700_000_000,
            &PlanModeReason::ModelRequested { reason: None },
        );
        assert!(s.starts_with("[plan-mode] entered at "));
        assert!(s.ends_with(" — reason: model"));
    }

    #[test]
    fn notify_entered_model_with_reason() {
        let s = format_notify_entered(
            1_700_000_000,
            &PlanModeReason::ModelRequested {
                reason: Some("auth flow".into()),
            },
        );
        assert!(s.contains("reason: model: auth flow"));
    }

    #[test]
    fn notify_entered_operator() {
        let s = format_notify_entered(1_700_000_000, &PlanModeReason::OperatorRequested);
        assert!(s.ends_with("reason: operator"));
    }

    #[test]
    fn notify_entered_auto_destructive_carries_check() {
        let s = format_notify_entered(
            1_700_000_000,
            &PlanModeReason::AutoDestructive {
                tripped_check: "rm -rf $HOME".into(),
            },
        );
        assert!(s.contains("auto-destructive: rm -rf $HOME"));
    }

    #[test]
    fn notify_exited_truncates_long_plan() {
        let plan = "x".repeat(300);
        let s = format_notify_exited(&plan, 7);
        // 200 x's + ellipsis + the suffix.
        assert!(s.contains("xxx"));
        assert!(s.contains('…'));
        assert!(s.ends_with("(full plan in turn log #7)"));
    }

    #[test]
    fn notify_exited_short_plan_no_ellipsis() {
        let s = format_notify_exited("1. read auth\n2. patch", 42);
        assert!(!s.contains('…'));
        assert!(s.ends_with("(full plan in turn log #42)"));
    }

    #[test]
    fn notify_acceptance_pass_and_fail() {
        let p = format_notify_acceptance(AcceptanceOutcome::Pass, "12 tests");
        assert_eq!(p, "[plan-mode] acceptance: pass (12 tests)");
        let f = format_notify_acceptance(AcceptanceOutcome::Fail, "build red");
        assert_eq!(f, "[plan-mode] acceptance: fail (build red)");
    }

    #[test]
    fn notify_refused_renders_tool_and_kind() {
        let refusal = PlanModeRefusal {
            tool_name: "FileEdit".into(),
            tool_kind: ToolKind::FileEdit,
            hint: PlanModeRefusal::HINT,
            entered_at: 1,
            entered_reason: PlanModeReason::ModelRequested { reason: None },
        };
        assert_eq!(
            format_notify_refused(&refusal),
            "[plan-mode] refused tool=FileEdit kind=file_edit"
        );
    }
}
