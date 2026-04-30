//! 4-phase consolidation prompt builder. Phase 80.1.
//!
//! Verbatim port of
//! `claude-code-leak/src/services/autoDream/consolidationPrompt.ts:1-65`.
//!
//! Constants ported from
//! `claude-code-leak/src/memdir/memdir.ts:34,35,116`:
//! - `ENTRYPOINT_NAME = "MEMORY.md"`
//! - `MAX_ENTRYPOINT_LINES = 200`
//! - `DIR_EXISTS_GUIDANCE` text

use std::path::PathBuf;

/// Memory entrypoint filename. Mirror leak `memdir.ts:34`.
pub const ENTRYPOINT_NAME: &str = "MEMORY.md";

/// Soft cap on entrypoint length. Mirror leak `memdir.ts:35`.
pub const MAX_ENTRYPOINT_LINES: usize = 200;

/// Guidance string for the prompt. Mirror leak `memdir.ts:116-117`.
pub const DIR_EXISTS_GUIDANCE: &str =
    "This directory already exists — write to it directly with the Write tool (do not run mkdir or check for its existence).";

/// Build the consolidation prompt. Verbatim port of leak `:10-65`.
///
/// `extra` is the per-run context block (Bash read-only constraints +
/// session list) — see `auto_dream::build_extra` for the canonical
/// shape. When empty, the trailing "## Additional context" section is
/// omitted (mirror leak `:64`).
pub struct ConsolidationPromptBuilder {
    memory_root: PathBuf,
    transcript_dir: PathBuf,
    max_entrypoint_lines: usize,
}

impl ConsolidationPromptBuilder {
    pub fn new(memory_root: PathBuf, transcript_dir: PathBuf) -> Self {
        Self {
            memory_root,
            transcript_dir,
            max_entrypoint_lines: MAX_ENTRYPOINT_LINES,
        }
    }

    /// Override `MAX_ENTRYPOINT_LINES` (default 200). Tests use this
    /// to confirm constant is rendered into the output.
    pub fn with_max_entrypoint_lines(mut self, n: usize) -> Self {
        self.max_entrypoint_lines = n;
        self
    }

    /// Render the prompt. Mirror leak `:14-64`.
    pub fn build(&self, extra: &str) -> String {
        let memory_root = self.memory_root.display();
        let transcript_dir = self.transcript_dir.display();
        let max_lines = self.max_entrypoint_lines;
        let entry = ENTRYPOINT_NAME;
        let guidance = DIR_EXISTS_GUIDANCE;

        let mut out = format!(
            "# Dream: Memory Consolidation\n\n\
             You are performing a dream — a reflective pass over your memory files. Synthesize what you've learned recently into durable, well-organized memories so that future sessions can orient quickly.\n\n\
             Memory directory: `{memory_root}`\n\
             {guidance}\n\n\
             Session transcripts: `{transcript_dir}` (large JSONL files — grep narrowly, don't read whole files)\n\n\
             ---\n\n\
             ## Phase 1 — Orient\n\n\
             - `ls` the memory directory to see what already exists\n\
             - Read `{entry}` to understand the current index\n\
             - Skim existing topic files so you improve them rather than creating duplicates\n\
             - If `logs/` or `sessions/` subdirectories exist (assistant-mode layout), review recent entries there\n\n\
             ## Phase 2 — Gather recent signal\n\n\
             Look for new information worth persisting. Sources in rough priority order:\n\n\
             1. **Daily logs** (`logs/YYYY/MM/YYYY-MM-DD.md`) if present — these are the append-only stream\n\
             2. **Existing memories that drifted** — facts that contradict something you see in the codebase now\n\
             3. **Transcript search** — if you need specific context (e.g., \"what was the error message from yesterday's build failure?\"), grep the JSONL transcripts for narrow terms:\n   \
             `grep -rn \"<narrow term>\" {transcript_dir}/ --include=\"*.jsonl\" | tail -50`\n\n\
             Don't exhaustively read transcripts. Look only for things you already suspect matter.\n\n\
             ## Phase 3 — Consolidate\n\n\
             For each thing worth remembering, write or update a memory file at the top level of the memory directory. Use the memory file format and type conventions from your system prompt's auto-memory section — it's the source of truth for what to save, how to structure it, and what NOT to save.\n\n\
             Focus on:\n\
             - Merging new signal into existing topic files rather than creating near-duplicates\n\
             - Converting relative dates (\"yesterday\", \"last week\") to absolute dates so they remain interpretable after time passes\n\
             - Deleting contradicted facts — if today's investigation disproves an old memory, fix it at the source\n\n\
             ## Phase 4 — Prune and index\n\n\
             Update `{entry}` so it stays under {max_lines} lines AND under ~25KB. It's an **index**, not a dump — each entry should be one line under ~150 characters: `- [Title](file.md) — one-line hook`. Never write memory content directly into it.\n\n\
             - Remove pointers to memories that are now stale, wrong, or superseded\n\
             - Demote verbose entries: if an index line is over ~200 chars, it's carrying content that belongs in the topic file — shorten the line, move the detail\n\
             - Add pointers to newly important memories\n\
             - Resolve contradictions — if two files disagree, fix the wrong one\n\n\
             ---\n\n\
             Return a brief summary of what you consolidated, updated, or pruned. If nothing changed (memories are already tight), say so."
        );

        if !extra.is_empty() {
            out.push_str("\n\n## Additional context\n\n");
            out.push_str(extra);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_builder() -> ConsolidationPromptBuilder {
        ConsolidationPromptBuilder::new(
            PathBuf::from("/var/memory"),
            PathBuf::from("/var/transcripts"),
        )
    }

    #[test]
    fn renders_4_phases() {
        let p = mk_builder().build("");
        assert!(p.contains("## Phase 1 — Orient"));
        assert!(p.contains("## Phase 2 — Gather recent signal"));
        assert!(p.contains("## Phase 3 — Consolidate"));
        assert!(p.contains("## Phase 4 — Prune and index"));
    }

    #[test]
    fn substitutes_paths() {
        let p = mk_builder().build("");
        assert!(p.contains("`/var/memory`"));
        assert!(p.contains("`/var/transcripts`"));
        assert!(p.contains("/var/transcripts/ --include=\"*.jsonl\""));
    }

    #[test]
    fn substitutes_entrypoint_name_and_max_lines() {
        let p = mk_builder().build("");
        assert!(p.contains("`MEMORY.md`"));
        assert!(p.contains("under 200 lines"));
    }

    #[test]
    fn includes_dir_exists_guidance() {
        let p = mk_builder().build("");
        assert!(p.contains("This directory already exists"));
        assert!(p.contains("write to it directly with the Write tool"));
    }

    #[test]
    fn extra_block_appended_when_present() {
        let p = mk_builder().build("session-1\nsession-2");
        assert!(p.contains("## Additional context"));
        assert!(p.contains("session-1"));
    }

    #[test]
    fn extra_block_omitted_when_empty() {
        let p = mk_builder().build("");
        assert!(!p.contains("## Additional context"));
    }

    #[test]
    fn override_max_entrypoint_lines() {
        let p = mk_builder().with_max_entrypoint_lines(150).build("");
        assert!(p.contains("under 150 lines"));
        assert!(!p.contains("under 200 lines"));
    }

    #[test]
    fn ends_with_summary_request() {
        let p = mk_builder().build("");
        assert!(p.contains("Return a brief summary"));
    }
}
