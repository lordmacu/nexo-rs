//! Phase M8 — canonical list of built-in tools that ship deferred
//! by default. Deferred tools are excluded from
//! `ToolRegistry::to_tool_defs_non_deferred()` (the slice every
//! provider shim — Anthropic / MiniMax / OpenAI / Gemini / DeepSeek
//! / xAI / Mistral — emits in the request body) and instead surface
//! through `ToolSearch` discovery + the
//! `<available-deferred-tools>` synthetic block. The model fetches
//! a deferred tool's full schema on demand via
//! `ToolSearch(select:<name>)`.
//!
//! Adding a tool to [`BUILT_IN_DEFERRED_TOOLS`] is the only step
//! required for it to participate in the `ToolSearch` budget — no
//! per-call-site change needed. The sweep
//! [`mark_built_in_deferred`] runs at agent boot, idempotent vs
//! gated tools (entries not registered in this boot are silently
//! skipped because [`ToolRegistry::set_meta`] only writes the
//! side-channel meta map).
//!
//! Provider-agnostic: deferral lives at the registry layer, not in
//! any provider shim. Switching providers does not change which
//! tools are deferred.
//!
//! IRROMPIBLE refs:
//! - `claude-code-leak/src/Tool.ts:438-449` — `shouldDefer` /
//!   `alwaysLoad` semantics. Deferred tools are sent with
//!   `defer_loading: true`; `alwaysLoad: true` is the per-tool
//!   opt-out (we don't need it today, no built-in requires turn-1
//!   appearance).
//! - `claude-code-leak/src/tools/ToolSearchTool/prompt.ts:62-108`
//!   — `isDeferredTool` decision tree the consumer uses to pick
//!   the deferred subset. Carve-outs (`alwaysLoad`, `isMcp`,
//!   `name == TOOL_SEARCH`, KAIROS-mode Brief / SendUserFile,
//!   FORK_SUBAGENT-mode Agent) live there; we mirror only the
//!   `name == TOOL_SEARCH` carve-out today (ToolSearch itself
//!   must always load — the model needs it to discover the rest).
//! - `claude-code-leak/src/services/api/claude.ts:1136-1253` —
//!   token-budget rationale: deferred schemas omitted from the
//!   request, `<available-deferred-tools>` block injects names +
//!   1-line descriptions instead. Big surfaces (e.g. ~30 MCP
//!   tools) save thousands of tokens per turn.
//! - Per-tool `shouldDefer: true` precedents in leak:
//!   * `src/tools/TodoWriteTool/TodoWriteTool.ts:51`
//!   * `src/tools/NotebookEditTool/NotebookEditTool.ts:94`
//!   * `src/tools/RemoteTriggerTool/RemoteTriggerTool.ts:50`
//!   * `src/tools/LSPTool/LSPTool.ts:136`
//!   * `src/tools/TeamCreateTool/TeamCreateTool.ts:78`
//!   * `src/tools/TeamDeleteTool/TeamDeleteTool.ts:36`
//!   * `src/tools/TaskListTool/TaskListTool.ts:52` — precedent for
//!     list/status read-only tools (we apply it to `TeamList` /
//!     `TeamStatus`).
//!   * `src/tools/SendMessageTool/SendMessageTool.ts:533` —
//!     precedent for messaging tools (we apply it to
//!     `TeamSendMessage`).
//!   * `src/tools/ListMcpResourcesTool/ListMcpResourcesTool.ts:50`
//!   * `src/tools/ReadMcpResourceTool/ReadMcpResourceTool.ts:59`
//! - `research/`: no relevant prior art — OpenClaw is channel-side
//!   and has no `ToolSearch` / deferred-tool concept.

use super::tool_registry::{ToolMeta, ToolRegistry};

/// Canonical list of `(tool_name, search_hint)` for built-in
/// tools that ship deferred. The hint feeds `ToolSearch` keyword
/// ranking — when present it scores higher than the verbose
/// description (mirrors leak's `searchHint:` field on the tool
/// definition, e.g. `TaskListTool.ts:35`).
///
/// Out of scope (deferred to follow-up slices):
/// - `EnterPlanMode` / `ExitPlanMode` (M8.b — plan-mode flow
///   control mid-turn warrants separate UX consideration).
/// - 5 cron tools (M8.c — surface differs from leak's 3-tool
///   shape; defer until cron UX settles post-Phase 80.2-80.6).
/// - `WebSearch` / `WebFetch` (M8.d — Phase 21/25 surface still
///   in flux).
pub const BUILT_IN_DEFERRED_TOOLS: &[(&str, &str)] = &[
    ("TodoWrite", "todo, tasks, in-progress checklist"),
    ("NotebookEdit", "jupyter, ipynb, notebook cell edit"),
    ("RemoteTrigger", "webhook, external publish, http POST"),
    ("Lsp", "language server, go-to-def, hover, references"),
    ("TeamCreate", "team, parallel agents, fan-out"),
    ("TeamDelete", "team, teardown"),
    ("TeamSendMessage", "team, dm, broadcast"),
    ("TeamList", "team, list active members"),
    ("TeamStatus", "team, status, member health"),
    ("Repl", "python, node, bash, REPL, code execution"),
    ("ListMcpResources", "mcp, resources, discovery"),
    ("ReadMcpResource", "mcp, resource, fetch"),
];

/// Apply `ToolMeta::deferred_with_hint(hint)` to every tool in
/// [`BUILT_IN_DEFERRED_TOOLS`] that is registered on `registry`.
///
/// Idempotent in two senses:
/// 1. Tools that aren't registered in this boot (gated off via
///    `agent.team.enabled = false`, `agent.lsp.enabled = false`,
///    etc.) are silently skipped — `set_meta` only writes the
///    side-channel meta map and doesn't require a handler.
/// 2. Calling N times has the same effect as calling once — the
///    last write wins and all writes carry identical content.
///
/// Call once at agent boot, AFTER all `tools.register(...)` calls
/// and BEFORE the registry is handed to the runtime. Calling
/// before registration still works (meta lands in the side
/// channel) but can be surprising — the documented call site is
/// post-registration.
pub fn mark_built_in_deferred(registry: &ToolRegistry) {
    for (name, hint) in BUILT_IN_DEFERRED_TOOLS.iter() {
        registry.set_meta(name, ToolMeta::deferred_with_hint(*hint));
    }
}
