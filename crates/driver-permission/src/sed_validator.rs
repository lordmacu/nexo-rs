//! Phase 77.9 — sed command validation.
//!
//! Ported from `claude-code-leak/src/tools/BashTool/sedValidation.ts`.
//! Validates sed commands against an allowlist (line-printing, substitution)
//! and a denylist (write/execute commands, backslash tricks, etc.).

use std::sync::LazyLock;
use regex::Regex;

use crate::path_extractor::parse_command_args;

/// Check if a sed expression is a valid print command.
/// STRICT ALLOWLIST: only `p`, `Np`, or `N,Mp` where N, M are digits.
///
/// Ported from `sedValidation.ts:128-133`.
fn is_print_command(cmd: &str) -> bool {
    let re = LazyLock::new(|| Regex::new(r"^(?:\d+|\d+,\d+)?p$").unwrap());
    re.is_match(cmd)
}

/// Validate flags against an allowlist. Handles both single flags and
/// combined flags (e.g., -nE).
///
/// Ported from `sedValidation.ts:13-35`.
fn validate_flags_against_allowlist(flags: &[String], allowed: &[&str]) -> bool {
    for flag in flags {
        if flag.starts_with('-') && !flag.starts_with("--") && flag.len() > 2 {
            // Combined flags like -nE or -Er — check each char
            for ch in flag[1..].chars() {
                let single = format!("-{ch}");
                if !allowed.contains(&single.as_str()) {
                    return false;
                }
            }
        } else if !allowed.contains(&flag.as_str()) {
            return false;
        }
    }
    true
}

/// Pattern 1: Check if this is a line-printing command with -n flag.
/// Allows: sed -n 'N' | sed -n 'N,M' with optional -E, -r, -z flags.
/// File arguments are ALLOWED for this pattern.
///
/// Ported from `sedValidation.ts:44-117`.
fn is_line_printing_command(command: &str, expressions: &[String]) -> bool {
    let args = parse_command_args(command);
    // Find "sed" and take args after it
    let sed_pos = args.iter().position(|a| a == "sed");
    let sed_pos = match sed_pos {
        Some(p) => p,
        None => return false,
    };
    let sed_args: Vec<&str> = args[sed_pos + 1..].iter().map(|s| s.as_str()).collect();
    if sed_args.is_empty() {
        return false;
    }

    // Extract flags
    let flags: Vec<String> = sed_args
        .iter()
        .filter(|a| a.starts_with('-') && **a != "--")
        .map(|s| s.to_string())
        .collect();

    let allowed_flags = [
        "-n", "--quiet", "--silent", "-E", "--regexp-extended",
        "-r", "-z", "--zero-terminated", "--posix",
    ];

    if !validate_flags_against_allowlist(&flags, &allowed_flags) {
        return false;
    }

    // Must have -n flag
    let has_n = flags.iter().any(|f| {
        f == "-n" || f == "--quiet" || f == "--silent"
            || (f.starts_with('-') && !f.starts_with("--") && f.contains('n'))
    });
    if !has_n {
        return false;
    }

    if expressions.is_empty() {
        return false;
    }

    // All expressions must be print commands (allow ;-separated)
    for expr in expressions {
        for cmd in expr.split(';') {
            if !is_print_command(cmd.trim()) {
                return false;
            }
        }
    }
    true
}

/// Pattern 2: Check if this is a substitution command.
/// Allows: sed 's/pattern/replacement/flags' with safe flags only.
///
/// Ported from `sedValidation.ts:142-238`.
fn is_substitution_command(
    command: &str,
    expressions: &[String],
    has_file_args: bool,
    allow_file_writes: bool,
) -> bool {
    if !allow_file_writes && has_file_args {
        return false;
    }

    let args = parse_command_args(command);
    let sed_pos = args.iter().position(|a| a == "sed");
    let sed_pos = match sed_pos {
        Some(p) => p,
        None => return false,
    };
    let sed_args: Vec<&str> = args[sed_pos + 1..].iter().map(|s| s.as_str()).collect();

    let flags: Vec<String> = sed_args
        .iter()
        .filter(|a| a.starts_with('-') && **a != "--")
        .map(|s| s.to_string())
        .collect();

    let mut allowed_flags = vec!["-E", "--regexp-extended", "-r", "--posix"];
    if allow_file_writes {
        allowed_flags.push("-i");
        allowed_flags.push("--in-place");
    }

    if !validate_flags_against_allowlist(&flags, &allowed_flags) {
        return false;
    }

    if expressions.len() != 1 {
        return false;
    }

    let expr = expressions[0].trim();
    if !expr.starts_with('s') {
        return false;
    }

    // Parse s/pattern/replacement/flags
    let rest = &expr[1..]; // after 's'
    if rest.is_empty() {
        return false;
    }
    let delim = rest.chars().next().unwrap();

    // Find delimiters, tracking backslash escapes
    let mut delim_count = 0u32;
    let mut last_delim_pos = 0usize;
    let chars: Vec<char> = rest.chars().collect();
    let mut i = 1; // skip first delimiter

    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == delim {
            delim_count += 1;
            last_delim_pos = i;
        }
        i += 1;
    }

    if delim_count != 2 {
        return false;
    }

    // Extract flags (after last delimiter)
    let expr_flags: String = chars[last_delim_pos + 1..].iter().collect();

    // Validate flags: only g, p, i, I, m, M, and optionally one digit 1-9
    let safe_flags_re = LazyLock::new(|| Regex::new(r"^[gpimIM]*[1-9]?[gpimIM]*$").unwrap());
    if !safe_flags_re.is_match(&expr_flags) {
        return false;
    }

    true
}

// ── Denylist: contains_dangerous_operations ──

/// Denylist patterns for dangerous sed operations.
/// Ported from `sedValidation.ts:473-629`.
fn contains_dangerous_operations(expression: &str) -> bool {
    let cmd = expression.trim();
    if cmd.is_empty() {
        return false;
    }

    // Non-ASCII characters (homoglyph attack)
    if cmd.chars().any(|c| c as u32 > 0x7F) {
        return true;
    }

    // Curly braces (blocks)
    if cmd.contains('{') || cmd.contains('}') {
        return true;
    }

    // Newlines
    if cmd.contains('\n') {
        return true;
    }

    // Comments (# not immediately after s command)
    if let Some(hash_pos) = cmd.find('#') {
        if hash_pos == 0 || cmd.as_bytes().get(hash_pos.saturating_sub(1)) != Some(&b's') {
            return true;
        }
    }

    // Negation operator (!)
    if cmd.starts_with('!')
        || Regex::new(r"[/\d$]!").map_or(false, |re| re.is_match(cmd))
    {
        return true;
    }

    // Step address (~)
    if Regex::new(r"\d\s*~\s*\d|,\s*~\s*\d|\$\s*~\s*\d")
        .map_or(false, |re| re.is_match(cmd))
    {
        return true;
    }

    // Comma at start
    if cmd.starts_with(',') {
        return true;
    }

    // Comma followed by +/-
    if Regex::new(r",\s*[+-]").map_or(false, |re| re.is_match(cmd)) {
        return true;
    }

    // Backslash tricks
    if cmd.contains("s\\")
        || Regex::new(r"\\[|#%@]").map_or(false, |re| re.is_match(cmd))
    {
        return true;
    }

    // Escaped slashes followed by w/W
    if Regex::new(r"\\\/.*[wW]").map_or(false, |re| re.is_match(cmd)) {
        return true;
    }

    // Malformed: /pattern w file, /pattern e cmd
    if Regex::new(r"/[^/]*\s+[wWeE]").map_or(false, |re| re.is_match(cmd)) {
        return true;
    }

    // Malformed substitution
    if cmd.starts_with("s/") && !Regex::new(r"^s/[^/]*/[^/]*/[^/]*$")
        .map_or(false, |re| re.is_match(cmd))
    {
        return true;
    }

    // s commands ending with w/W/e/E with non-slash delimiters
    if cmd.starts_with('s') && cmd.chars().last().map_or(false, |c| "wWeE".contains(c)) {
        if !has_consistent_s_delimiters(cmd) {
            return true;
        }
    }

    // Write commands (w/W)
    if has_write_command(cmd) {
        return true;
    }

    // Execute commands (e/E)
    if has_execute_command(cmd) {
        return true;
    }

    // Dangerous substitution flags (w/W/e/E)
    let s_flags = extract_s_substitution_flags(cmd);
    if let Some(f) = s_flags {
        if f.contains('w') || f.contains('W') || f.contains('e') || f.contains('E') {
            return true;
        }
    }

    // y command with w/W/e/E
    if Regex::new(r"y([^\\\n])").map_or(false, |re| re.is_match(cmd)) {
        if cmd.chars().any(|c| "wWeE".contains(c)) {
            return true;
        }
    }

    false
}

/// Check that an `s` command has consistent delimiter chars.
/// E.g. `s/foo/bar/` has 3 `/` delimiters; `s|foo|bar|` has 3 `|`.
/// Returns false if the delimiter count is wrong or the structure is
/// malformed. Replaces a backreference-based regex that the `regex`
/// crate does not support.
fn has_consistent_s_delimiters(cmd: &str) -> bool {
    let rest = &cmd[1..]; // after 's'
    if rest.is_empty() {
        return false;
    }
    let delim = rest.chars().next().unwrap();
    if delim == '\\' || delim == '\n' {
        return false;
    }
    let mut count = 0u32;
    let mut escaped = false;
    for ch in rest.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == delim {
            count += 1;
        }
    }
    // A valid s command has 3 delimiters: s/pat/rep/flags
    count == 3
}

/// Extract substitution flags from an `s` command using manual
/// delimiter tracking. Returns the flags string (after the 3rd
/// delimiter), or `None` if the command is not a well-formed `s`
/// substitution. Replaces a backreference-based regex.
fn extract_s_substitution_flags(cmd: &str) -> Option<String> {
    if !cmd.starts_with('s') {
        return None;
    }
    let rest = &cmd[1..];
    if rest.is_empty() {
        return None;
    }
    let delim = rest.chars().next().unwrap();
    if delim == '\\' || delim == '\n' {
        return None;
    }
    let mut delim_count = 0u32;
    let mut last_delim_pos = 0usize;
    let mut escaped = false;
    for (i, ch) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == delim {
            delim_count += 1;
            if delim_count == 3 {
                last_delim_pos = i;
                break;
            }
        }
    }
    if delim_count < 3 {
        return None;
    }
    Some(rest[last_delim_pos + 1..].to_string())
}

fn has_write_command(cmd: &str) -> bool {
    [
        r"^[wW]\s*\S+",
        r"^\d+\s*[wW]\s*\S+",
        r"^\$\s*[wW]\s*\S+",
        r"^/[^/]*/[IMim]*\s*[wW]\s*\S+",
        r"^\d+,\d+\s*[wW]\s*\S+",
        r"^\d+,\$\s*[wW]\s*\S+",
        r"^/[^/]*/[IMim]*,/[^/]*/[IMim]*\s*[wW]\s*\S+",
    ]
    .iter()
    .any(|pat| Regex::new(pat).map_or(false, |re| re.is_match(cmd)))
}

fn has_execute_command(cmd: &str) -> bool {
    [
        r"^e",
        r"^\d+\s*e",
        r"^\$\s*e",
        r"^/[^/]*/[IMim]*\s*e",
        r"^\d+,\d+\s*e",
        r"^\d+,\$\s*e",
        r"^/[^/]*/[IMim]*,/[^/]*/[IMim]*\s*e",
    ]
    .iter()
    .any(|pat| Regex::new(pat).map_or(false, |re| re.is_match(cmd)))
}

// ── Public API ──

/// Extract sed expressions from a command string.
/// Ported from `sedValidation.ts:388-466`.
pub fn extract_sed_expressions(command: &str) -> Vec<String> {
    let mut expressions: Vec<String> = Vec::new();
    let args = parse_command_args(command);

    let sed_pos = match args.iter().position(|a| a == "sed") {
        Some(p) => p,
        None => return expressions,
    };

    let sed_args: Vec<&str> = args[sed_pos + 1..].iter().map(|s| s.as_str()).collect();

    // Reject dangerous flag combinations
    for arg in &sed_args {
        if arg.starts_with('-') && !arg.starts_with("--") {
            if (arg.contains('e') && arg.contains('w'))
                || (arg.contains('e') && arg.contains('W'))
                || (arg.contains('w') && arg.contains('e'))
                || (arg.contains('w') && arg.contains('E'))
            {
                return expressions; // dangerous combo — bail
            }
        }
    }

    let mut found_e_flag = false;
    let mut found_expression = false;
    let mut i = 0;

    while i < sed_args.len() {
        let arg = sed_args[i];

        if (arg == "-e" || arg == "--expression") && i + 1 < sed_args.len() {
            found_e_flag = true;
            expressions.push(sed_args[i + 1].to_string());
            i += 2;
            continue;
        }

        if arg.starts_with("--expression=") {
            found_e_flag = true;
            expressions.push(arg["--expression=".len()..].to_string());
            i += 1;
            continue;
        }

        if arg.starts_with("-e=") {
            found_e_flag = true;
            expressions.push(arg["-e=".len()..].to_string());
            i += 1;
            continue;
        }

        if arg.starts_with('-') {
            i += 1;
            continue;
        }

        if !found_e_flag && !found_expression {
            expressions.push(arg.to_string());
            found_expression = true;
            i += 1;
            continue;
        }

        break; // remaining are filenames
    }

    expressions
}

/// Check if a sed command has file arguments (not just stdin).
/// Ported from `sedValidation.ts:307-379`.
pub fn has_sed_file_args(command: &str) -> bool {
    let args = parse_command_args(command);
    let sed_pos = match args.iter().position(|a| a == "sed") {
        Some(p) => p,
        None => return false,
    };
    let sed_args: Vec<&str> = args[sed_pos + 1..].iter().map(|s| s.as_str()).collect();

    let mut arg_count = 0u32;
    let mut has_e_flag = false;
    let mut i = 0;

    while i < sed_args.len() {
        let arg = sed_args[i];

        if (arg == "-e" || arg == "--expression") && i + 1 < sed_args.len() {
            has_e_flag = true;
            i += 2;
            continue;
        }
        if arg.starts_with("--expression=") || arg.starts_with("-e=") {
            has_e_flag = true;
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            i += 1;
            continue;
        }

        arg_count += 1;
        if has_e_flag || arg_count > 1 {
            return true;
        }
        i += 1;
    }
    false
}

/// Main entry point: checks if a sed command is allowed by the allowlist
/// and does NOT contain dangerous operations.
///
/// Ported from `sedValidation.ts:247-301`.
pub fn sed_command_is_allowed(command: &str, allow_file_writes: bool) -> bool {
    let expressions = extract_sed_expressions(command);
    let has_files = has_sed_file_args(command);

    let pattern1 = is_line_printing_command(command, &expressions);
    let pattern2 = is_substitution_command(command, &expressions, has_files, allow_file_writes);

    if !pattern1 && !pattern2 {
        return false;
    }

    // Pattern 2 does not allow semicolons
    if pattern2 {
        for expr in &expressions {
            if expr.contains(';') {
                return false;
            }
        }
    }

    // Defense-in-depth: check denylist
    for expr in &expressions {
        if contains_dangerous_operations(expr) {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Print commands ──
    #[test]
    fn print_commands() {
        assert!(is_print_command("p"));
        assert!(is_print_command("1p"));
        assert!(is_print_command("1,10p"));
        assert!(is_print_command("123,456p"));
    }

    #[test]
    fn non_print_commands_rejected() {
        assert!(!is_print_command("d"));
        assert!(!is_print_command("w file"));
        assert!(!is_print_command("e echo hi"));
        assert!(!is_print_command("s/foo/bar/"));
    }

    // ── Line printing pattern ──
    #[test]
    fn line_printing_allowed() {
        assert!(sed_command_is_allowed("sed -n '1,10p' file.txt", false));
        assert!(sed_command_is_allowed("sed -n '1p;5p;10p' file.txt", false));
        assert!(sed_command_is_allowed("sed -nE '1,10p' file.txt", false));
    }

    #[test]
    fn line_printing_no_n_flag_rejected() {
        assert!(!sed_command_is_allowed("sed '1,10p' file.txt", false));
    }

    // ── Substitution pattern ──
    #[test]
    fn substitution_allowed_read_only() {
        // stdin only — no file args, read-only mode
        assert!(sed_command_is_allowed("sed 's/foo/bar/'", false));
        assert!(sed_command_is_allowed("sed 's/foo/bar/g'", false));
        assert!(sed_command_is_allowed("sed 's/foo/bar/gi'", false));
    }

    #[test]
    fn substitution_with_files_blocked_read_only() {
        // has file args + read-only mode → blocked
        assert!(!sed_command_is_allowed("sed 's/foo/bar/' file.txt", false));
    }

    #[test]
    fn substitution_with_files_allowed_in_edit_mode() {
        assert!(sed_command_is_allowed("sed 's/foo/bar/' file.txt", true));
        assert!(sed_command_is_allowed("sed -i 's/foo/bar/g' file.txt", true));
    }

    #[test]
    fn substitution_with_dangerous_flags_rejected() {
        assert!(!sed_command_is_allowed("sed 's/foo/bar/e' file.txt", true));
        assert!(!sed_command_is_allowed("sed 's/foo/bar/w out.txt' file.txt", true));
    }

    // ── Expressions ──
    #[test]
    fn extract_simple_expression() {
        let exprs = extract_sed_expressions("sed 's/foo/bar/g' file.txt");
        assert_eq!(exprs, vec!["s/foo/bar/g"]);
    }

    #[test]
    fn extract_e_flag_expression() {
        let exprs = extract_sed_expressions("sed -e 's/foo/bar/' file.txt");
        assert_eq!(exprs, vec!["s/foo/bar/"]);
    }

    #[test]
    fn no_expressions_for_non_sed() {
        let exprs = extract_sed_expressions("grep pattern file.txt");
        assert!(exprs.is_empty());
    }

    // ── File args ──
    #[test]
    fn sed_has_file_args() {
        assert!(has_sed_file_args("sed 's/foo/bar/' file.txt"));
        assert!(has_sed_file_args("sed -e 's/foo/bar/' file.txt"));
    }

    #[test]
    fn sed_stdin_no_file_args() {
        assert!(!has_sed_file_args("sed 's/foo/bar/'"));
        assert!(!has_sed_file_args("sed -e 's/foo/bar/'"));
    }

    // ── Dangerous operations ──
    #[test]
    fn dangerous_write_command() {
        assert!(contains_dangerous_operations("1w /tmp/out"));
        assert!(contains_dangerous_operations("w /tmp/out"));
        assert!(contains_dangerous_operations("/pattern/w /tmp/out"));
    }

    #[test]
    fn dangerous_execute_command() {
        assert!(contains_dangerous_operations("e echo pwned"));
        assert!(contains_dangerous_operations("1e echo pwned"));
    }

    #[test]
    fn non_ascii_rejected() {
        // Fullwidth 'w'
        assert!(contains_dangerous_operations("ｗ /tmp/out"));
    }

    #[test]
    fn negation_rejected() {
        assert!(contains_dangerous_operations("!/pattern/d"));
        assert!(contains_dangerous_operations("/pattern/!d"));
    }

    #[test]
    fn safe_substitution_passes() {
        assert!(!contains_dangerous_operations("s/foo/bar/g"));
        assert!(!contains_dangerous_operations("s/foo/bar/gi"));
    }

    // ── Integration: sed read-only detection for path validation ──
    #[test]
    fn read_only_sed_is_allowed() {
        // sed -n with print commands is always read-only
        assert!(sed_command_is_allowed("sed -n '1,10p' file.txt", false));
    }

    #[test]
    fn in_place_sed_without_edit_mode_blocked() {
        assert!(!sed_command_is_allowed("sed -i 's/foo/bar/' file.txt", false));
    }

    #[test]
    fn in_place_sed_with_edit_mode_allowed() {
        assert!(sed_command_is_allowed("sed -i 's/foo/bar/' file.txt", true));
    }
}
