//! M9 — `expose_tools` typo regression guard.
//!
//! Maintains a hardcoded snapshot of every canonical tool name that
//! has ever appeared in `EXPOSABLE_TOOLS` and asserts bidirectional
//! sync with the live catalog:
//!
//! 1. Every name in the snapshot still resolves via
//!    [`lookup_exposable`] — catches silent renames or removals
//!    that would leave operator YAML
//!    (`mcp_server.expose_tools: [...]`) referencing names the
//!    catalog no longer recognises. The runtime currently warns at
//!    `src/main.rs:9261-9269` and moves on; the snapshot makes
//!    that warn impossible to ship accidentally from CODE-side.
//! 2. Every entry in [`EXPOSABLE_TOOLS`] is in the snapshot —
//!    forces the developer to extend the snapshot when adding a
//!    new exposable tool, so the source of truth stays
//!    bidirectional.
//!
//! ## Difference vs `exposable_catalog_test.rs`
//!
//! The sibling `exposable_catalog_test.rs` is the **conformance
//! suite**: walks `EXPOSABLE_TOOLS` and asserts the boot dispatcher
//! has a wired arm for each entry (`Registered` /
//! `SkippedInfraMissing` / `SkippedDenied` shape). That covers the
//! direction "catalog entry → dispatch arm wired".
//!
//! M9 covers the inverse: "every name we have ever exposed still
//! resolves AND every catalog entry is acknowledged". The two tests
//! are siblings, not duplicates.
//!
//! ## Workflow
//!
//! - **Adding a new tool to `EXPOSABLE_TOOLS`** — append the
//!   canonical name to `KNOWN_CANONICAL_NAMES_SNAPSHOT` in this
//!   file. Test #2 fails until you do.
//! - **Renaming a canonical name** (e.g. `TeamCreate` →
//!   `team_create`) — three options, each surfaced in the test #1
//!   fail message:
//!   1. Restore the catalog entry (rename was accidental).
//!   2. Drop the snapshot entry (rename intentional; operators
//!      with old YAML will see a boot warn from
//!      `src/main.rs:9261-9269`; document the breaking change in
//!      the PR body).
//!   3. Add a deprecated-alias mapping (M9.b follow-up — not yet
//!      shipped).
//! - **Removing a tool** — drop both the catalog entry and the
//!   snapshot entry. Conscious decision; document in commit body.
//!
//! ## Provider-agnostic
//!
//! `EXPOSABLE_TOOLS` is the wire-spec MCP catalog — independent of
//! which LLM client (Claude Desktop / Cursor / Continue / Cody /
//! Aider) or backing provider (Anthropic / MiniMax / OpenAI /
//! Gemini / DeepSeek / xAI / Mistral) drives the call. The
//! snapshot inherits that property.
//!
//! ## Prior art
//!
//! - `research/src/channels/ids.test.ts:48-50` — OpenClaw uses the
//!   same snapshot-vs-catalog assertion
//!   (`expect(CHAT_CHANNEL_ALIASES).toEqual(collectBundledChatChannelAliases())`)
//!   for its alias map. We extend the pattern to the full
//!   canonical-name set.
//! - `claude-code-leak/src/tools.ts:193-251` — `getAllBaseTools()`
//!   returns a hardcoded array WITHOUT a snapshot test; the leak
//!   has no protection against silent renames. The pattern we
//!   intentionally do NOT follow.

use std::collections::HashSet;

use nexo_config::types::mcp_exposable::{lookup_exposable, EXPOSABLE_TOOLS};

/// Hardcoded snapshot of every canonical tool name that has ever
/// appeared in `EXPOSABLE_TOOLS`. Initial baseline = 33 names from
/// the `EXPOSABLE_TOOLS` const at 2026-04-30.
///
/// **Any change here is a conscious decision documented in the PR
/// body.** Organised by phase comment so the audit trail is visible
/// in git blame.
const KNOWN_CANONICAL_NAMES_SNAPSHOT: &[&str] = &[
    // Plan-mode (Phase 79.1)
    "EnterPlanMode",
    "ExitPlanMode",
    "plan_mode_resolve",
    // Discovery / scratch (Phase 79.2 / 79.3 / 79.4)
    "ToolSearch",
    "SyntheticOutput",
    "TodoWrite",
    // LSP (Phase 79.5)
    "Lsp",
    // Team* (Phase 79.6)
    "TeamCreate",
    "TeamDelete",
    "TeamSendMessage",
    "TeamList",
    "TeamStatus",
    // Cron (Phase 79.7)
    "cron_create",
    "cron_list",
    "cron_delete",
    "cron_pause",
    "cron_resume",
    // RemoteTrigger (Phase 79.8)
    "RemoteTrigger",
    // ConfigTool + audit tail (Phase 79.10)
    "Config",
    "config_changes_tail",
    // MCP resource router (Phase 79.11)
    "ListMcpResources",
    "ReadMcpResource",
    // NotebookEdit (Phase 79.13)
    "NotebookEdit",
    // Web (Phase 21 / 25)
    "web_fetch",
    "web_search",
    // Memory (Phase 5 / 10)
    "forge_memory_checkpoint",
    "memory_history",
    "memory_snapshot",
    // TaskFlow (Phase 14)
    "taskflow",
    // Followup tools (Phase 14 follow-up)
    "start_followup",
    "check_followup",
    "cancel_followup",
    // Delegation + heartbeat (Phase 7 / 8)
    "delegate",
    "Heartbeat",
];

/// Test #1 — Every snapshot name MUST still resolve via the catalog
/// lookup. Catches silent renames and removals.
#[test]
fn every_snapshot_name_resolves_via_lookup() {
    let mut missing: Vec<&'static str> = KNOWN_CANONICAL_NAMES_SNAPSHOT
        .iter()
        .copied()
        .filter(|name| lookup_exposable(name).is_none())
        .collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "M9 regression: the following canonical names are in \
         KNOWN_CANONICAL_NAMES_SNAPSHOT but no longer resolve via \
         `lookup_exposable`: {missing:?}.\n\nFix paths:\n\
         1. RESTORE the catalog entry in EXPOSABLE_TOOLS (if the \
         removal was accidental).\n\
         2. DROP the entry from KNOWN_CANONICAL_NAMES_SNAPSHOT in \
         this file (if the removal is intentional and operators \
         with old YAML will get a boot warn — document the \
         breaking change in the PR body).\n\
         3. ADD a deprecated-alias entry mapping the old name to \
         the new canonical (M9.b follow-up — not yet shipped)."
    );
}

/// Test #2 — Every catalog entry MUST be in the snapshot. Forces
/// the developer to extend the snapshot when adding a new exposable
/// tool, keeping the bidirectional source-of-truth invariant.
#[test]
fn every_catalog_name_in_snapshot() {
    let snapshot: HashSet<&'static str> =
        KNOWN_CANONICAL_NAMES_SNAPSHOT.iter().copied().collect();
    let mut missing: Vec<&'static str> = EXPOSABLE_TOOLS
        .iter()
        .map(|e| e.name)
        .filter(|name| !snapshot.contains(name))
        .collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "M9 regression: EXPOSABLE_TOOLS contains the following \
         canonical names that are NOT in \
         KNOWN_CANONICAL_NAMES_SNAPSHOT: {missing:?}.\n\nFix: \
         append each to `KNOWN_CANONICAL_NAMES_SNAPSHOT` in \
         `crates/core/tests/expose_tools_typo_regression_test.rs`. \
         The snapshot is the audit log of every name we have ever \
         exposed — keeping it in sync with the catalog enables the \
         reverse regression guard (test #1)."
    );
}

/// Sanity — the snapshot has zero duplicates. Catches accidental
/// double-add when extending the list, typically a merge-conflict
/// resolution mistake.
#[test]
fn snapshot_has_no_duplicates() {
    let unique: HashSet<&'static str> =
        KNOWN_CANONICAL_NAMES_SNAPSHOT.iter().copied().collect();
    assert_eq!(
        unique.len(),
        KNOWN_CANONICAL_NAMES_SNAPSHOT.len(),
        "KNOWN_CANONICAL_NAMES_SNAPSHOT has duplicate entries; \
         dedup before merging."
    );
}
