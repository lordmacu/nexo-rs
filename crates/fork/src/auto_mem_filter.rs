//! `AutoMemFilter` — tool whitelist for forked memory-consolidation work.
//! Phase 80.20.
//!
//! Verbatim port of
//! `claude-code-leak/src/services/extractMemories/extractMemories.ts:165-222`
//! (`createAutoMemCanUseTool`).
//!
//! # Provider-agnostic
//!
//! Operates on tool name + JSON args — no [`nexo_llm::LlmClient`]
//! coupling. Works under any provider impl (Anthropic / OpenAI /
//! MiniMax / Gemini / DeepSeek). Tool names are canonical nexo
//! strings; provider clients translate to/from native wire formats
//! before / after dispatch.
//!
//! # Defense layers
//!
//! 1. Whitelist allow-list: only `REPL`, `FileRead`, `Glob`, `Grep`,
//!    `Bash`, `FileEdit`, `FileWrite`. Everything else denied.
//! 2. `Bash` requires `nexo_driver_permission::is_read_only`
//!    (Phase 80.20 step 1) — composes Phase 77.8/77.9 classifiers
//!    + a positive whitelist of read-only utilities.
//! 3. `FileEdit` / `FileWrite` require `file_path` post-canonicalize
//!    to start with `memory_dir` (Phase 80.20 step 3). Symlink +
//!    traversal defense via canonical resolution at construction
//!    AND per-call.
//! 4. (Defense in depth — ships in 80.1) post-fork audit of
//!    `files_touched` paths in `crates/dream/auto_dream`.

use std::fmt::Debug;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;

use crate::tool_filter::ToolFilter;

/// Canonical nexo tool names. Match the strings each tool registers
/// in `nexo_core::agent::tool_registry`. Single source of truth so a
/// rename only touches this module.
pub mod tool_names {
    pub const FILE_READ: &str = "FileRead";
    pub const FILE_EDIT: &str = "FileEdit";
    pub const FILE_WRITE: &str = "FileWrite";
    pub const GLOB: &str = "Glob";
    pub const GREP: &str = "Grep";
    pub const BASH: &str = "Bash";
    /// Only present when nexo is built with the `repl-tool` feature.
    /// When absent, no LLM produces this tool name and the filter's
    /// `REPL` branch is unreachable — benign.
    pub const REPL: &str = "REPL";
}

/// Errors from constructing an `AutoMemFilter`.
#[derive(Debug, Error)]
pub enum AutoMemFilterError {
    #[error("memory_dir does not exist: {0}")]
    MissingDir(PathBuf),
    #[error("memory_dir cannot be canonicalized: {0}")]
    Canonicalize(#[from] io::Error),
}

/// Per-fork tool whitelist for auto-memory work.
///
/// See module docstring for the allow-list semantics. Caller must
/// `mkdir -p memory_dir` before constructing — the filter fails
/// fast on a missing directory rather than denying every Edit/Write
/// at runtime.
#[derive(Debug, Clone)]
pub struct AutoMemFilter {
    /// Canonical absolute path to the memory directory. Resolved
    /// once at construction so a later symlink swap can't redirect
    /// allowed writes to an attacker's path.
    memory_dir: PathBuf,
}

impl AutoMemFilter {
    /// Construct an `AutoMemFilter` rooted at `memory_dir`.
    /// `memory_dir` MUST exist; canonicalized at construction.
    /// Caller responsible for `mkdir -p`.
    pub fn new(memory_dir: impl AsRef<Path>) -> Result<Self, AutoMemFilterError> {
        let p = memory_dir.as_ref();
        if !p.exists() {
            return Err(AutoMemFilterError::MissingDir(p.to_path_buf()));
        }
        let canonical = p.canonicalize()?;
        Ok(Self {
            memory_dir: canonical,
        })
    }

    /// Return the canonical memory dir for use in denial messages.
    pub fn memory_dir(&self) -> &Path {
        &self.memory_dir
    }

    /// Resolve a tool's `file_path` arg to a canonical path AND verify
    /// it lives under `memory_dir`. Returns `false` on:
    /// - missing or non-string `file_path`
    /// - relative paths (forces LLM to use absolute paths)
    /// - paths outside memory_dir (post-canonicalize)
    /// - non-existent target whose parent canonicalizes outside
    ///   memory_dir (or whose parent doesn't exist)
    fn file_path_inside_memdir(&self, args: &Value) -> bool {
        let raw = match args.get("file_path").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s,
            _ => return false,
        };
        let p = Path::new(raw);
        if !p.is_absolute() {
            return false;
        }
        // For Edit on existing file → canonicalize directly.
        // For Write on new file → canonicalize parent + check parent
        // is inside memory_dir, then re-join the filename.
        let resolved = if p.exists() {
            match p.canonicalize() {
                Ok(c) => c,
                Err(_) => return false,
            }
        } else {
            let parent = match p.parent() {
                Some(par) => par,
                None => return false,
            };
            let canonical_parent = match parent.canonicalize() {
                Ok(c) => c,
                Err(_) => return false,
            };
            match p.file_name() {
                Some(name) => canonical_parent.join(name),
                None => return false,
            }
        };
        resolved.starts_with(&self.memory_dir)
    }
}

impl ToolFilter for AutoMemFilter {
    fn allows(&self, tool_name: &str, args: &Value) -> bool {
        match tool_name {
            // REPL allowed unrestricted. Per leak `extractMemories.ts:171-180`,
            // when REPL mode is enabled the inner primitives (Read / Bash /
            // Edit / Write) are hidden from the tool list, but REPL's VM
            // re-invokes this filter for each inner primitive — so the
            // checks below still gate the actual operations. Giving the
            // fork a different tool list would break prompt-cache sharing
            // (tools are part of the cache key — see
            // `nexo_fork::CacheSafeParams`).
            tool_names::REPL => true,
            tool_names::FILE_READ | tool_names::GREP | tool_names::GLOB => true,
            tool_names::BASH => {
                let cmd = args
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                nexo_driver_permission::bash_destructive::is_read_only(cmd)
            }
            tool_names::FILE_EDIT | tool_names::FILE_WRITE => {
                self.file_path_inside_memdir(args)
            }
            _ => false,
        }
    }

    fn denial_message(&self, tool_name: &str) -> String {
        match tool_name {
            tool_names::BASH => {
                "Only read-only shell commands are permitted in this context \
                 (ls, find, grep, cat, stat, wc, head, tail, and similar)"
                    .to_string()
            }
            tool_names::FILE_EDIT | tool_names::FILE_WRITE => format!(
                "Path must be inside the memory directory ({}). \
                 Edit/Write outside that path is denied.",
                self.memory_dir.display()
            ),
            _ => format!(
                "only FileRead, Grep, Glob, read-only Bash, and \
                 FileEdit/FileWrite within {} are allowed",
                self.memory_dir.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    fn mk_tmpdir() -> TempDir {
        tempfile::tempdir().expect("tmp dir")
    }

    // ── Construction ──

    #[test]
    fn new_creates_filter_with_canonical_dir() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert_eq!(f.memory_dir(), tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn new_missing_dir_errors() {
        let result = AutoMemFilter::new("/this/path/should/not/exist/__nx__");
        assert!(matches!(result, Err(AutoMemFilterError::MissingDir(_))));
    }

    // ── Allow / deny by tool name ──

    #[test]
    fn allows_repl_unrestricted() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert!(f.allows(tool_names::REPL, &json!({"code": "anything"})));
    }

    #[test]
    fn allows_file_read_grep_glob() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert!(f.allows(tool_names::FILE_READ, &json!({"file_path": "/anywhere"})));
        assert!(f.allows(tool_names::GREP, &json!({"pattern": "foo"})));
        assert!(f.allows(tool_names::GLOB, &json!({"pattern": "**/*.md"})));
    }

    #[test]
    fn allows_bash_read_only() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert!(f.allows(tool_names::BASH, &json!({"command": "ls -la /tmp"})));
        assert!(f.allows(tool_names::BASH, &json!({"command": "grep foo bar.txt | head"})));
    }

    #[test]
    fn denies_bash_destructive() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert!(!f.allows(tool_names::BASH, &json!({"command": "rm -rf /"})));
        assert!(!f.allows(tool_names::BASH, &json!({"command": "git push --force"})));
    }

    #[test]
    fn denies_bash_subshell() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert!(!f.allows(tool_names::BASH, &json!({"command": "echo $(rm -rf /)"})));
    }

    #[test]
    fn denies_bash_missing_command_arg() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert!(!f.allows(tool_names::BASH, &json!({})));
    }

    #[test]
    fn denies_unknown_tool() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert!(!f.allows("UnknownTool", &json!({})));
        assert!(!f.allows("Web", &json!({})));
    }

    // ── FileEdit / FileWrite path checks ──

    #[test]
    fn allows_edit_existing_inside_memdir() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        let inside = tmp.path().join("foo.md");
        fs::write(&inside, "x").unwrap();
        let path_str = inside.canonicalize().unwrap().to_string_lossy().into_owned();
        assert!(f.allows(
            tool_names::FILE_EDIT,
            &json!({"file_path": path_str})
        ));
    }

    #[test]
    fn allows_write_new_file_inside_memdir() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        // File doesn't exist yet — parent does.
        let new_file = tmp.path().canonicalize().unwrap().join("new.md");
        let path_str = new_file.to_string_lossy().into_owned();
        assert!(f.allows(
            tool_names::FILE_WRITE,
            &json!({"file_path": path_str})
        ));
    }

    #[test]
    fn denies_edit_outside_memdir() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert!(!f.allows(
            tool_names::FILE_EDIT,
            &json!({"file_path": "/etc/hosts"})
        ));
    }

    #[test]
    fn denies_relative_path() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert!(!f.allows(
            tool_names::FILE_EDIT,
            &json!({"file_path": "relative/foo.md"})
        ));
        assert!(!f.allows(
            tool_names::FILE_WRITE,
            &json!({"file_path": "./bar.md"})
        ));
    }

    #[test]
    fn denies_traversal_dotdot_escape() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        // Build a path that would escape memory_dir after canonicalize.
        let escape = tmp.path().join("..").join("..").join("etc").join("passwd");
        let path_str = escape.to_string_lossy().into_owned();
        assert!(!f.allows(
            tool_names::FILE_EDIT,
            &json!({"file_path": path_str})
        ));
    }

    #[cfg(unix)]
    #[test]
    fn denies_symlink_pointing_outside_memdir() {
        use std::os::unix::fs::symlink;
        let tmp = mk_tmpdir();
        let outside = tmp.path().parent().unwrap().join("outside_target.md");
        fs::write(&outside, "secret").unwrap();
        let symlink_path = tmp.path().join("escape.md");
        symlink(&outside, &symlink_path).unwrap();

        let f = AutoMemFilter::new(tmp.path()).unwrap();
        let path_str = symlink_path.to_string_lossy().into_owned();
        // Symlink target lives OUTSIDE memory_dir. Filter must deny.
        assert!(!f.allows(
            tool_names::FILE_EDIT,
            &json!({"file_path": path_str})
        ));

        // Cleanup
        fs::remove_file(&outside).ok();
    }

    #[test]
    fn denies_file_path_missing() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert!(!f.allows(tool_names::FILE_EDIT, &json!({})));
        assert!(!f.allows(tool_names::FILE_WRITE, &json!({"other": "x"})));
    }

    #[test]
    fn denies_file_path_non_string() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        assert!(!f.allows(
            tool_names::FILE_EDIT,
            &json!({"file_path": 123})
        ));
        assert!(!f.allows(
            tool_names::FILE_EDIT,
            &json!({"file_path": null})
        ));
    }

    #[test]
    fn denies_write_when_parent_missing() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        let no_parent = tmp.path().join("nonexistent_subdir").join("foo.md");
        let path_str = no_parent.to_string_lossy().into_owned();
        assert!(!f.allows(
            tool_names::FILE_WRITE,
            &json!({"file_path": path_str})
        ));
    }

    // ── Denial messages ──

    #[test]
    fn denial_message_bash_describes_read_only() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        let msg = f.denial_message(tool_names::BASH);
        assert!(msg.contains("read-only"));
        assert!(msg.contains("ls"));
    }

    #[test]
    fn denial_message_edit_includes_memory_dir() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        let msg = f.denial_message(tool_names::FILE_EDIT);
        let canonical = tmp.path().canonicalize().unwrap();
        assert!(msg.contains(&canonical.display().to_string()));
        assert!(msg.contains("memory directory"));
    }

    #[test]
    fn denial_message_unknown_tool_is_generic() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        let msg = f.denial_message("RandomTool");
        assert!(msg.contains("FileRead"));
        assert!(msg.contains("Grep"));
        assert!(msg.contains("Glob"));
    }

    // ── Provider-shape transversality ──
    //
    // Filter must work identically across LlmClient impls. Tool args
    // arrive as canonical nexo-flat JSON regardless of provider wire
    // format (Anthropic flat, OpenAI function-calling, MiniMax, Gemini,
    // DeepSeek). Provider clients unwrap any nesting BEFORE handing args
    // to the dispatcher — the filter contract is "flat top-level keys".

    #[test]
    fn flat_args_anthropic_shape() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        let inside = tmp.path().canonicalize().unwrap().join("foo.md");
        fs::write(&inside, "x").unwrap();
        // Anthropic ships args flat.
        assert!(f.allows(
            tool_names::FILE_EDIT,
            &json!({"file_path": inside.to_string_lossy()})
        ));
    }

    #[test]
    fn extra_metadata_keys_ignored() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        // Some providers include trace IDs, chain-of-thought hints, etc.
        // Filter inspects only the keys it cares about.
        assert!(f.allows(
            tool_names::BASH,
            &json!({
                "command": "ls",
                "_trace_id": "abc",
                "_provider_meta": {"x": 1}
            })
        ));
    }

    #[test]
    fn nested_args_unsupported_explicit() {
        let tmp = mk_tmpdir();
        let f = AutoMemFilter::new(tmp.path()).unwrap();
        // Provider client MUST unwrap before dispatch. If the client
        // forgot to unwrap and passed nested args, filter denies (no
        // `command` at top level).
        assert!(!f.allows(
            tool_names::BASH,
            &json!({"arguments": {"command": "ls"}})
        ));
    }
}
