//! Phase 77.8 — bash destructive-command warning.
//!
//! Ported from `claude-code-leak/src/tools/BashTool/destructiveCommandWarning.ts`.
//! 16 compiled regex patterns detect known destructive git / rm / SQL / infra
//! commands. First match wins. Pure function, no alloc, no async.

use std::sync::LazyLock;
use regex::Regex;

type DestructivePattern = (Regex, &'static str);

static DESTRUCTIVE_PATTERNS: LazyLock<Vec<DestructivePattern>> = LazyLock::new(|| {
    vec![
        // ── Git — data loss / hard to reverse ──
        (
            Regex::new(r"\bgit\s+reset\s+--hard\b").unwrap(),
            "may discard uncommitted changes",
        ),
        (
            Regex::new(r"\bgit\s+push\b[^;&|\n]*[ \t](--force|--force-with-lease|-f)\b").unwrap(),
            "may overwrite remote history",
        ),
        // git clean -f, excluding --dry-run / -n (handled in check fn)
        (
            Regex::new(r"\bgit\s+clean\b[^;&|\n]*-[a-zA-Z]*f").unwrap(),
            "may permanently delete untracked files",
        ),
        (
            Regex::new(r"\bgit\s+checkout\s+(--\s+)?\.[ \t]*($|[;&|\n])").unwrap(),
            "may discard all working tree changes",
        ),
        (
            Regex::new(r"\bgit\s+restore\s+(--\s+)?\.[ \t]*($|[;&|\n])").unwrap(),
            "may discard all working tree changes",
        ),
        (
            Regex::new(r"\bgit\s+stash[ \t]+(drop|clear)\b").unwrap(),
            "may permanently remove stashed changes",
        ),
        (
            Regex::new(r"\bgit\s+branch\s+(-D[ \t]|--delete\s+--force|--force\s+--delete)\b").unwrap(),
            "may force-delete a branch",
        ),
        // ── Git — safety bypass ──
        (
            Regex::new(r"\bgit\s+(commit|push|merge)\b[^;&|\n]*--no-verify\b").unwrap(),
            "may skip safety hooks",
        ),
        (
            Regex::new(r"\bgit\s+commit\b[^;&|\n]*--amend\b").unwrap(),
            "may rewrite the last commit",
        ),
        // ── File deletion ──
        (
            Regex::new(r"(^|[;&|\n]\s*)rm\s+-[a-zA-Z]*[rR][a-zA-Z]*f|(^|[;&|\n]\s*)rm\s+-[a-zA-Z]*f[a-zA-Z]*[rR]").unwrap(),
            "may recursively force-remove files",
        ),
        (
            Regex::new(r"(^|[;&|\n]\s*)rm\s+-[a-zA-Z]*[rR]").unwrap(),
            "may recursively remove files",
        ),
        (
            Regex::new(r"(^|[;&|\n]\s*)rm\s+-[a-zA-Z]*f").unwrap(),
            "may force-remove files",
        ),
        // ── Database ──
        (
            Regex::new(r"(?i)\b(DROP|TRUNCATE)\s+(TABLE|DATABASE|SCHEMA)\b").unwrap(),
            "may drop or truncate database objects",
        ),
        (
            Regex::new(r#"(?i)\bDELETE\s+FROM\s+\w+[ \t]*(;|"|'|\n|$)"#).unwrap(),
            "may delete all rows from a database table",
        ),
        // ── Infrastructure ──
        (
            Regex::new(r"\bkubectl\s+delete\b").unwrap(),
            "may delete Kubernetes resources",
        ),
        (
            Regex::new(r"\bterraform\s+destroy\b").unwrap(),
            "may destroy Terraform infrastructure",
        ),
    ]
});

/// Second-stage check for `git clean` — if `--dry-run` or `-n` flag
/// (standalone or combined like `-nfd`) is present, the command is a
/// no-op despite `-f`.
static GIT_CLEAN_DRY_RUN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"git\s+clean\b[^;&|\n]*\s-[a-zA-Z]*n[a-zA-Z]*").unwrap()
});

/// Checks a bash command string against known destructive patterns.
/// Returns a human-readable warning, or `None` if no pattern matches.
/// The first match wins; subsequent patterns are not checked.
///
/// Ported from `claude-code-leak/src/tools/BashTool/destructiveCommandWarning.ts:95-102`.
pub fn check_destructive_command(command: &str) -> Option<&'static str> {
    for (pattern, warning) in DESTRUCTIVE_PATTERNS.iter() {
        if !pattern.is_match(command) {
            continue;
        }
        // git clean — skip when `--dry-run` or `-n` (possibly
        // combined, e.g. `-nfd`) is present.
        if warning.contains("untracked files") {
            if command.contains("--dry-run") || GIT_CLEAN_DRY_RUN.is_match(command) {
                continue;
            }
        }
        return Some(warning);
    }
    None
}

// ── Phase 80.20 — read-only command classifier ──
//
// Composes Phase 77.8 (`check_destructive_command`) + Phase 77.9
// (`check_sed_in_place`) with an explicit positive whitelist so the
// classifier stays *conservative*: when in doubt it returns `false`.
//
// Used by `nexo_fork::AutoMemFilter` (Phase 80.20) to gate the
// `Bash` tool inside forked memory-consolidation runs (autoDream
// Phase 80.1, AWAY_SUMMARY Phase 80.14, eval harness Phase 51).
//
// NOT a security boundary by itself — pair with
// `should_use_sandbox` (Phase 77.10) for defense-in-depth. Trust
// the whitelist only when the call site has its own audit (e.g.
// post-fork `files_touched` check in 80.1).

/// Static set of commands considered read-only — they do not mutate
/// the filesystem, do not perform network egress that affects external
/// state, and do not change privileges.
///
/// `tee` and `awk` are intentionally **excluded** even though they
/// can be used safely: `tee FILE` writes a file without a redirect
/// marker, and `awk 'BEGIN { system("rm -rf /") }'` shells out via
/// `system()`. Conservative default — operators can add them per-call
/// with explicit `>` redirect to `/dev/null` if needed.
const READ_ONLY_COMMANDS: &[&str] = &[
    // Inspection
    "ls", "find", "stat", "file", "du", "df", "tree", "realpath",
    "readlink", "basename", "dirname",
    // Read content
    "cat", "head", "tail", "less", "more", "tac",
    "hexdump", "xxd", "od", "strings",
    // Search / filter / transform (output to stdout only)
    "grep", "egrep", "fgrep", "rg", "ag",
    "cut", "sort", "uniq", "rev", "wc",
    "comm", "cmp", "diff", "tr",
    // Identity / env
    "pwd", "echo", "printf", "true", "false", "test", "[",
    "which", "type", "command",
    "hostname", "id", "whoami", "groups",
    "env", "printenv", "uname", "date",
    // Process / network inspection (read-only)
    "ps", "top", "htop", "pgrep",
    "ip", "netstat", "ss", "lsof",
];

/// Pattern markers that imply state mutation regardless of the leading
/// command name. Substring match — accepts false positives (overly
/// conservative) on quoted strings like `echo "rm -rf /"`. Trade-off
/// favours under-permissive over under-deny: the fork's `tool_result`
/// surface lets the model retry with a different shape.
const MUTATING_MARKERS: &[&str] = &[
    "$(", "`",          // subshells
    "<<",               // heredoc / here-string (covers `<<` + `<<<`)
    "<(", ">(",         // process substitution
    "rm ", "mv ", "cp ", "chmod ", "chown ",
    "mkdir ", "rmdir ", "ln ", "touch ",
    "git push", "git commit", "git reset", "git checkout",
    "git restore", "git stash", "git branch", "git merge",
    "curl ", "wget ", "scp ", "rsync ",
    "sudo ", "su ",
    "tee ", "awk ", "perl ", "python ", "node ", "ruby ",
];

/// True when `command` is composed entirely of read-only operations.
///
/// Composition (in order — first failure wins):
/// 1. Empty / whitespace → `false` (defensive default).
/// 2. [`check_destructive_command`] hit → `false`.
/// 3. [`check_sed_in_place`] hit → `false`.
/// 4. Redirects to non-/dev/null → `false`.
/// 5. Any [`MUTATING_MARKERS`] substring (subshell / heredoc / known
///    mutating utility / privilege escalation / network egress) → `false`.
/// 6. Split on `|`, `&&`, `||`, `;` — every clause's first whitespace
///    word must appear in [`READ_ONLY_COMMANDS`]. Unknown command →
///    `false`.
///
/// Reference port: `claude-code-leak/src/tools/BashTool/bashSecurity.ts`
/// `isReadOnly` heuristic, generalised to compose existing nexo
/// classifiers from Phase 77.8 / 77.9.
///
/// ## Examples
///
/// ```
/// use nexo_driver_permission::bash_destructive::is_read_only;
/// assert!(is_read_only("ls -la /tmp"));
/// assert!(is_read_only("grep foo bar.txt | wc -l"));
/// assert!(is_read_only("ls > /dev/null"));
/// assert!(!is_read_only("rm -rf /"));
/// assert!(!is_read_only("echo $(rm -rf /)"));
/// assert!(!is_read_only("ls && rm foo"));
/// assert!(!is_read_only(""));
/// ```
pub fn is_read_only(command: &str) -> bool {
    if command.trim().is_empty() {
        return false;
    }
    if check_destructive_command(command).is_some() {
        return false;
    }
    if check_sed_in_place(command).is_some() {
        return false;
    }
    if has_non_devnull_redirect(command) {
        return false;
    }
    for marker in MUTATING_MARKERS {
        if command.contains(marker) {
            return false;
        }
    }
    let clauses = split_command_clauses(command);
    if clauses.is_empty() {
        return false;
    }
    clauses.iter().all(|c| {
        let first_word = c.trim_start().split_whitespace().next().unwrap_or("");
        // Strip leading env var assignments: `FOO=bar ls` → `ls`. Conservative:
        // if the leading word contains `=` we assume env-var assignment and
        // walk past it.
        let first_real = if first_word.contains('=') {
            c.split_whitespace()
                .find(|w| !w.contains('='))
                .unwrap_or("")
        } else {
            first_word
        };
        READ_ONLY_COMMANDS.contains(&first_real)
    })
}

/// Detect a redirect that targets something other than `/dev/null`.
/// Allows the common pattern `cmd > /dev/null 2>&1` while flagging any
/// real file write.
fn has_non_devnull_redirect(command: &str) -> bool {
    let bytes = command.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        // Skip `>&` (fd duplication) like `2>&1`.
        if c == '>' && i + 1 < bytes.len() && bytes[i + 1] as char == '&' {
            i += 2;
            continue;
        }
        if c == '>' {
            // Could be `>` or `>>`. Skip the marker byte(s).
            let mut j = i + 1;
            if j < bytes.len() && bytes[j] as char == '>' {
                j += 1;
            }
            // Skip whitespace to the target token.
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            // Read target token until next whitespace or shell control char.
            let start = j;
            while j < bytes.len() {
                let ch = bytes[j] as char;
                if ch.is_whitespace() || matches!(ch, '|' | '&' | ';' | '<' | '>') {
                    break;
                }
                j += 1;
            }
            let target = &command[start..j];
            if target != "/dev/null" {
                return true;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    false
}

/// Split `command` into top-level clauses on `|`, `&&`, `||`, `;`.
/// Conservative — does not honour quoting, so a clause inside a quoted
/// string may be split incorrectly. Acceptable: classifier denies on
/// such ambiguity (callers fall back to a clearer command shape).
fn split_command_clauses(command: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut last = 0;
    let bytes = command.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() {
            let two = &command[i..i + 2];
            if two == "&&" || two == "||" {
                out.push(&command[last..i]);
                last = i + 2;
                i += 2;
                continue;
            }
        }
        let c = bytes[i] as char;
        // `|` not part of `||` is pipe; `;` is sequence.
        if c == '|' || c == ';' {
            out.push(&command[last..i]);
            last = i + 1;
        }
        i += 1;
    }
    if last < command.len() {
        out.push(&command[last..]);
    }
    out
}

// ── Phase 77.9 — sed in-place warning ──

/// Regex for detecting `sed -i` / `sed --in-place` / `sed -i.bak`.
/// Matches the sed command followed by the in-place edit flag anywhere
/// before a pipe, semicolon, or newline.
static SED_IN_PLACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bsed\b[^;&|\n]*\s(-i\b|--in-place\b)").unwrap()
});

/// Detects `sed -i` (or `--in-place`, or `-i.bak`) in a bash command
/// and returns a warning that the file will be modified in place.
///
/// Ported from `claude-code-leak/src/tools/BashTool/sedValidation.ts`
/// and `sedEditParser.ts`.
pub fn check_sed_in_place(command: &str) -> Option<&'static str> {
    if SED_IN_PLACE.is_match(command) {
        Some("Note: sed -i modifies files in place")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helpers
    fn assert_warns(cmd: &str, expected: &str) {
        let got = check_destructive_command(cmd).expect("expected a warning");
        assert!(
            got.contains(expected),
            "warning mismatch for '{cmd}': got '{got}', expected to contain '{expected}'"
        );
    }

    fn assert_safe(cmd: &str) {
        assert!(
            check_destructive_command(cmd).is_none(),
            "expected no warning for '{cmd}', got: {:?}",
            check_destructive_command(cmd)
        );
    }

    // ── Git destructive ──
    #[test]
    fn git_reset_hard() {
        assert_warns("git reset --hard HEAD~1", "discard uncommitted changes");
        assert_warns("git reset --hard", "discard uncommitted changes");
    }

    #[test]
    fn git_reset_soft_is_safe() {
        assert_safe("git reset --soft HEAD~1");
        assert_safe("git reset HEAD");
    }

    #[test]
    fn git_push_force() {
        assert_warns("git push --force origin main", "overwrite remote history");
        assert_warns("git push --force-with-lease origin main", "overwrite remote history");
        assert_warns("git push -f origin main", "overwrite remote history");
    }

    #[test]
    fn git_push_normal_is_safe() {
        assert_safe("git push origin main");
        assert_safe("git push");
    }

    #[test]
    fn git_clean_force() {
        assert_warns("git clean -fd", "untracked files");
        assert_warns("git clean -f", "untracked files");
        assert_warns("git clean -xdf", "untracked files");
    }

    #[test]
    fn git_clean_dry_run_is_safe() {
        assert_safe("git clean -nfd");
        assert_safe("git clean --dry-run -fd");
        assert_safe("git clean -fd --dry-run");
        assert_safe("git clean -n");
    }

    #[test]
    fn git_checkout_dot() {
        assert_warns("git checkout .", "discard all working tree changes");
        assert_warns("git checkout -- .", "discard all working tree changes");
    }

    #[test]
    fn git_checkout_branch_is_safe() {
        assert_safe("git checkout main");
        assert_safe("git checkout -b new-branch");
    }

    #[test]
    fn git_restore_dot() {
        assert_warns("git restore .", "discard all working tree changes");
        assert_warns("git restore -- .", "discard all working tree changes");
    }

    #[test]
    fn git_restore_file_is_safe() {
        assert_safe("git restore src/lib.rs");
        assert_safe("git restore --staged src/lib.rs");
    }

    #[test]
    fn git_stash_drop_clear() {
        assert_warns("git stash drop", "stashed changes");
        assert_warns("git stash clear", "stashed changes");
        assert_warns("git stash drop stash@{0}", "stashed changes");
    }

    #[test]
    fn git_stash_push_is_safe() {
        assert_safe("git stash");
        assert_safe("git stash push -m 'wip'");
        assert_safe("git stash pop");
    }

    #[test]
    fn git_branch_force_delete() {
        assert_warns("git branch -D old-branch", "force-delete a branch");
        assert_warns("git branch --delete --force old-branch", "force-delete a branch");
        assert_warns("git branch --force --delete old-branch", "force-delete a branch");
    }

    #[test]
    fn git_branch_delete_is_safe() {
        assert_safe("git branch -d merged-branch");
        assert_safe("git branch --delete merged-branch");
        assert_safe("git branch");
    }

    // ── Git safety bypass ──
    #[test]
    fn git_no_verify() {
        assert_warns("git commit --no-verify -m 'skip'", "safety hooks");
        assert_warns("git push --no-verify", "safety hooks");
        assert_warns("git merge --no-verify feature", "safety hooks");
    }

    #[test]
    fn git_commit_amend() {
        assert_warns("git commit --amend", "rewrite the last commit");
        assert_warns("git commit --amend -m 'new msg'", "rewrite the last commit");
    }

    // ── File deletion ──
    #[test]
    fn rm_recursive_force() {
        assert_warns("rm -rf node_modules/", "recursively force-remove");
        assert_warns("rm -fr node_modules/", "recursively force-remove");
        assert_warns("rm -rdf /tmp/cache", "recursively force-remove");
    }

    #[test]
    fn rm_recursive() {
        assert_warns("rm -r node_modules/", "recursively remove");
        assert_warns("rm -R dir/", "recursively remove");
    }

    #[test]
    fn rm_force() {
        assert_warns("rm -f file.txt", "force-remove");
    }

    #[test]
    fn rm_plain_is_safe() {
        assert_safe("rm file.txt");
        assert_safe("rm *.log");
    }

    #[test]
    fn rm_in_pipeline() {
        // rm at start of pipe commands should warn
        assert_warns("rm -rf cache/ && echo done", "recursively force-remove");
    }

    #[test]
    fn rm_in_quoted_string_not_flagged() {
        // rm patterns anchor to command-start boundaries,
        // so quoted strings are not false positives.
        assert_safe("echo \"rm -rf /\"");
        assert_safe("echo 'rm -rf /'");
    }

    // ── Database ──
    #[test]
    fn sql_drop_truncate() {
        assert_warns("DROP TABLE users", "drop or truncate");
        assert_warns("drop table users", "drop or truncate");
        assert_warns("TRUNCATE TABLE logs", "drop or truncate");
        assert_warns("DROP DATABASE prod", "drop or truncate");
        assert_warns("DROP SCHEMA public CASCADE", "drop or truncate");
    }

    #[test]
    fn sql_delete_from() {
        assert_warns("DELETE FROM users;", "delete all rows");
        assert_warns("delete from users", "delete all rows");
    }

    #[test]
    fn sql_delete_with_where_not_flagged() {
        // Regex requires end-of-statement (;|\"|'|\n|$) after table
        // name, so WHERE clauses are correctly not flagged.
        assert_safe("DELETE FROM users WHERE id=5");
        assert_safe("delete from logs where ts < now()");
    }

    // ── Infrastructure ──
    #[test]
    fn kubectl_delete() {
        assert_warns("kubectl delete pod my-pod", "Kubernetes resources");
    }

    #[test]
    fn kubectl_get_is_safe() {
        assert_safe("kubectl get pods");
        assert_safe("kubectl describe pod my-pod");
    }

    #[test]
    fn terraform_destroy() {
        assert_warns("terraform destroy", "Terraform infrastructure");
        assert_warns("terraform destroy -auto-approve", "Terraform infrastructure");
    }

    #[test]
    fn terraform_plan_is_safe() {
        assert_safe("terraform plan");
        assert_safe("terraform apply");
        assert_safe("terraform validate");
    }

    // ── Safe commands (negative) ──
    #[test]
    fn safe_commands_return_none() {
        assert_safe("ls -la");
        assert_safe("cargo build --workspace");
        assert_safe("echo hello");
        assert_safe("cat file.txt");
        assert_safe("grep -r pattern src/");
        assert_safe("mkdir -p /tmp/foo");
        assert_safe("docker ps");
        assert_safe("systemctl status nginx");
    }

    // ── Idempotency ──
    #[test]
    fn same_input_same_output() {
        let cmd = "git reset --hard HEAD~5";
        let w1 = check_destructive_command(cmd);
        let w2 = check_destructive_command(cmd);
        assert_eq!(w1, w2);
    }

    #[test]
    fn first_match_wins() {
        // "git reset --hard" matches before any hypothetical later pattern
        let w = check_destructive_command("git reset --hard && rm -rf /tmp/x");
        assert!(w.unwrap().contains("discard uncommitted"));
    }

    // ── Phase 77.9 — sed in-place warning ──
    #[test]
    fn sed_in_place_detected() {
        assert!(check_sed_in_place("sed -i 's/foo/bar/' file.txt").is_some());
        assert!(check_sed_in_place("sed --in-place 's/foo/bar/' file.txt").is_some());
        assert!(check_sed_in_place("sed -i.bak 's/foo/bar/' file.txt").is_some());
    }

    #[test]
    fn sed_read_only_safe() {
        assert!(check_sed_in_place("sed 's/foo/bar/' file.txt").is_none());
        assert!(check_sed_in_place("sed -n '1,10p' file.txt").is_none());
        assert!(check_sed_in_place("sed -e 's/foo/bar/' file.txt").is_none());
    }

    #[test]
    fn sed_in_place_in_pipeline() {
        // sed -i before a pipe should still warn
        assert!(check_sed_in_place("sed -i 's/foo/bar/' file.txt | cat").is_some());
    }

    // ── Phase 80.20 — is_read_only classifier ──
    //
    // Provider-agnostic: this classifier is consumed by
    // `nexo_fork::AutoMemFilter` which works under any LlmClient
    // (Anthropic / OpenAI / MiniMax / Gemini / DeepSeek). The
    // command string comes from the `Bash` tool's `command` arg,
    // which is canonical nexo regardless of provider wire format.

    #[test]
    fn ro_simple_ls() {
        assert!(is_read_only("ls -la /tmp"));
    }

    #[test]
    fn ro_simple_grep() {
        assert!(is_read_only("grep foo bar.txt"));
        assert!(is_read_only("rg --files"));
    }

    #[test]
    fn ro_pipe_all_read_only() {
        assert!(is_read_only("ls /tmp | grep foo"));
        assert!(is_read_only("cat foo | wc -l"));
        assert!(is_read_only("ps -ef | grep nexo | head -5"));
    }

    #[test]
    fn ro_redirect_devnull_only_allowed() {
        assert!(is_read_only("ls > /dev/null"));
        // Even with stderr redirection — both go to /dev/null.
        assert!(is_read_only("ls -la > /dev/null 2>&1"));
        // `cmake` is NOT in the read-only whitelist (it can build/install)
        // so the redirect-to-/dev/null does not flip the verdict — the
        // unknown-command rule denies.
        assert!(!is_read_only("cmake --version > /dev/null 2>&1"));
    }

    #[test]
    fn ro_redirect_to_file_denied() {
        assert!(!is_read_only("cat foo > bar"));
        assert!(!is_read_only("ls -la >> /tmp/log"));
    }

    #[test]
    fn ro_destructive_rm() {
        assert!(!is_read_only("rm -rf /"));
        assert!(!is_read_only("rm file.txt"));
    }

    #[test]
    fn ro_sed_in_place_denied() {
        assert!(!is_read_only("sed -i 's/foo/bar/' f"));
        assert!(!is_read_only("sed --in-place 's/x/y/' f"));
    }

    #[test]
    fn ro_subshell_denied() {
        assert!(!is_read_only("echo $(rm -rf /)"));
        assert!(!is_read_only("echo `rm -rf /`"));
    }

    #[test]
    fn ro_heredoc_denied() {
        assert!(!is_read_only("cat <<EOF\nhello\nEOF"));
        assert!(!is_read_only("cat <<<\"hello\""));
    }

    #[test]
    fn ro_process_substitution_denied() {
        assert!(!is_read_only("diff <(ls /a) <(ls /b)"));
        assert!(!is_read_only("tee >(cat) <input"));
    }

    #[test]
    fn ro_compound_and_with_destructive_denied() {
        assert!(!is_read_only("ls && rm foo"));
        assert!(!is_read_only("ls; rm foo"));
    }

    #[test]
    fn ro_compound_or_all_read_only_allowed() {
        assert!(is_read_only("ls || echo done"));
        assert!(is_read_only("test -f foo && echo yes"));
    }

    #[test]
    fn ro_empty_denied() {
        assert!(!is_read_only(""));
        assert!(!is_read_only("   "));
        assert!(!is_read_only("\n"));
    }

    #[test]
    fn ro_unknown_command_denied() {
        assert!(!is_read_only("xyz_unknown_tool"));
        assert!(!is_read_only("custom-script.sh"));
    }

    #[test]
    fn ro_sudo_denied() {
        assert!(!is_read_only("sudo ls"));
        assert!(!is_read_only("su - root"));
    }

    #[test]
    fn ro_curl_wget_denied() {
        // Network egress is conservative-deny even for GETs.
        assert!(!is_read_only("curl https://example.com"));
        assert!(!is_read_only("wget https://example.com"));
    }

    #[test]
    fn ro_tee_awk_denied() {
        // Removed from whitelist per spec — tee writes files,
        // awk can shell out via system().
        assert!(!is_read_only("tee /etc/passwd"));
        assert!(!is_read_only("awk 'BEGIN { system(\"rm /\") }'"));
    }

    #[test]
    fn ro_env_var_assignment_then_read_only() {
        // `FOO=bar ls` is a read-only command with env-var prefix.
        assert!(is_read_only("FOO=bar ls /tmp"));
        assert!(is_read_only("LANG=C grep foo bar.txt"));
    }

    #[test]
    fn ro_idempotent() {
        let cmd = "ls -la /tmp | grep foo";
        assert_eq!(is_read_only(cmd), is_read_only(cmd));
    }
}
