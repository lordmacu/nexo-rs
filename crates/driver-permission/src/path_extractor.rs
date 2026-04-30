//! Phase 77.9 — path extraction for common filesystem commands.
//!
//! Ported from `claude-code-leak/src/tools/BashTool/pathValidation.ts:27-509`.
//! Extracts file paths from command arguments so callers can validate
//! them against allowlists. Handles POSIX `--` end-of-options delimiter.

use std::collections::BTreeSet;

/// Commands whose arguments include filesystem paths.
/// Ported from `pathValidation.ts:27-64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PathCommand {
    Cd,
    Ls,
    Find,
    Mkdir,
    Touch,
    Rm,
    Rmdir,
    Mv,
    Cp,
    Cat,
    Head,
    Tail,
    Sort,
    Uniq,
    Wc,
    Cut,
    Paste,
    Column,
    Tr,
    File,
    Stat,
    Diff,
    Awk,
    Strings,
    Hexdump,
    Od,
    Base64,
    Nl,
    Grep,
    Rg,
    Sed,
    Git,
    Jq,
    Sha256Sum,
    Sha1Sum,
    Md5Sum,
}

impl PathCommand {
    /// Human-readable action verb for permission messages.
    pub fn action_verb(self) -> &'static str {
        match self {
            Self::Cd => "change directories to",
            Self::Ls => "list files in",
            Self::Find => "search files in",
            Self::Mkdir => "create directories in",
            Self::Touch => "create or modify files in",
            Self::Rm => "remove files from",
            Self::Rmdir => "remove directories from",
            Self::Mv => "move files to/from",
            Self::Cp => "copy files to/from",
            Self::Cat => "concatenate files from",
            Self::Head => "read the beginning of files from",
            Self::Tail => "read the end of files from",
            Self::Sort => "sort contents of files from",
            Self::Uniq => "filter duplicate lines from files in",
            Self::Wc => "count lines/words/bytes in files from",
            Self::Cut => "extract columns from files in",
            Self::Paste => "merge files from",
            Self::Column => "format files from",
            Self::Tr => "transform text from files in",
            Self::File => "examine file types in",
            Self::Stat => "read file stats from",
            Self::Diff => "compare files from",
            Self::Awk => "process text from files in",
            Self::Strings => "extract strings from files in",
            Self::Hexdump => "display hex dump of files from",
            Self::Od => "display octal dump of files from",
            Self::Base64 => "encode/decode files from",
            Self::Nl => "number lines in files from",
            Self::Grep => "search for patterns in files from",
            Self::Rg => "search for patterns in files from",
            Self::Sed => "edit files in",
            Self::Git => "access files with git from",
            Self::Jq => "process JSON from files in",
            Self::Sha256Sum => "compute SHA-256 checksums for files in",
            Self::Sha1Sum => "compute SHA-1 checksums for files in",
            Self::Md5Sum => "compute MD5 checksums for files in",
        }
    }

    pub fn is_write(self) -> bool {
        matches!(self, Self::Rm | Self::Rmdir | Self::Mv | Self::Cp | Self::Sed)
    }
}

/// Identify a base command name as a `PathCommand`.
pub fn classify_command(base_cmd: &str) -> Option<PathCommand> {
    match base_cmd {
        "cd" => Some(PathCommand::Cd),
        "ls" => Some(PathCommand::Ls),
        "find" => Some(PathCommand::Find),
        "mkdir" => Some(PathCommand::Mkdir),
        "touch" => Some(PathCommand::Touch),
        "rm" => Some(PathCommand::Rm),
        "rmdir" => Some(PathCommand::Rmdir),
        "mv" => Some(PathCommand::Mv),
        "cp" => Some(PathCommand::Cp),
        "cat" => Some(PathCommand::Cat),
        "head" => Some(PathCommand::Head),
        "tail" => Some(PathCommand::Tail),
        "sort" => Some(PathCommand::Sort),
        "uniq" => Some(PathCommand::Uniq),
        "wc" => Some(PathCommand::Wc),
        "cut" => Some(PathCommand::Cut),
        "paste" => Some(PathCommand::Paste),
        "column" => Some(PathCommand::Column),
        "tr" => Some(PathCommand::Tr),
        "file" => Some(PathCommand::File),
        "stat" => Some(PathCommand::Stat),
        "diff" => Some(PathCommand::Diff),
        "awk" => Some(PathCommand::Awk),
        "strings" => Some(PathCommand::Strings),
        "hexdump" => Some(PathCommand::Hexdump),
        "od" => Some(PathCommand::Od),
        "base64" => Some(PathCommand::Base64),
        "nl" => Some(PathCommand::Nl),
        "grep" => Some(PathCommand::Grep),
        "rg" => Some(PathCommand::Rg),
        "sed" => Some(PathCommand::Sed),
        "git" => Some(PathCommand::Git),
        "jq" => Some(PathCommand::Jq),
        "sha256sum" => Some(PathCommand::Sha256Sum),
        "sha1sum" => Some(PathCommand::Sha1Sum),
        "md5sum" => Some(PathCommand::Md5Sum),
        _ => None,
    }
}

/// SECURITY: Extract positional (non-flag) arguments, correctly handling
/// the POSIX `--` end-of-options delimiter. After `--`, ALL subsequent
/// arguments are positional even if they start with `-`.
///
/// Ported from `pathValidation.ts:126-139`.
pub fn filter_out_flags(args: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut after_double_dash = false;
    for arg in args {
        if after_double_dash {
            result.push(arg.clone());
        } else if arg == "--" {
            after_double_dash = true;
        } else if !arg.starts_with('-') {
            result.push(arg.clone());
        }
    }
    result
}

/// Extract paths from command arguments based on the command type.
/// Each command has specific logic for how it handles paths and flags.
///
/// Ported from `pathValidation.ts:190-509`.
pub fn extract_paths(cmd: PathCommand, args: &[String]) -> Vec<String> {
    match cmd {
        PathCommand::Cd => {
            if args.is_empty() {
                vec!["~".into()]
            } else {
                vec![args.join(" ")]
            }
        }
        PathCommand::Ls => {
            let paths = filter_out_flags(args);
            if paths.is_empty() {
                vec![".".into()]
            } else {
                paths
            }
        }
        PathCommand::Find => extract_find_paths(args),
        PathCommand::Grep => extract_pattern_command_paths(args, &grep_flags_with_args(), &[]),
        PathCommand::Rg => extract_pattern_command_paths(args, &rg_flags_with_args(), &[".".into()]),
        PathCommand::Sed => extract_sed_paths(args),
        PathCommand::Git => extract_git_paths(args),
        PathCommand::Jq => extract_jq_paths(args),
        PathCommand::Tr => extract_tr_paths(args),
        // Simple commands: just filter flags
        _ => filter_out_flags(args),
    }
}

/// Parse a simple command string into arguments, handling quotes.
/// Simplified shell-quote parser — splits on whitespace while respecting
/// single-quoted and double-quoted strings.
pub fn parse_command_args(cmd: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = cmd.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                // Backslash escape in double quotes or unquoted
                if let Some(&next) = chars.peek() {
                    if in_double && matches!(next, '"' | '\\' | '$' | '`' | '\n') {
                        current.push(next);
                        chars.next();
                    } else if !in_double {
                        current.push(next);
                        chars.next();
                    } else {
                        current.push(ch);
                    }
                } else {
                    current.push(ch);
                }
            }
            ch if ch.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

// ── Per-command extractors ──

fn extract_find_paths(args: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let path_flags: BTreeSet<&str> = [
        "-newer", "-anewer", "-cnewer", "-mnewer", "-samefile", "-path", "-wholename",
        "-ilname", "-lname", "-ipath", "-iwholename",
    ]
    .into_iter()
    .collect();
    let global_flags: BTreeSet<&str> = ["-H", "-L", "-P"].into_iter().collect();
    let mut found_non_global_flag = false;
    let mut after_double_dash = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        if after_double_dash {
            paths.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            if global_flags.contains(arg.as_str()) {
                i += 1;
                continue;
            }
            found_non_global_flag = true;
            if path_flags.contains(arg.as_str())
                || arg.starts_with("-newer")
            {
                if let Some(next) = args.get(i + 1) {
                    paths.push(next.clone());
                    i += 1;
                }
            }
            i += 1;
            continue;
        }
        if !found_non_global_flag {
            paths.push(arg.clone());
        }
        i += 1;
    }
    if paths.is_empty() {
        vec![".".into()]
    } else {
        paths
    }
}

fn extract_pattern_command_paths(
    args: &[String],
    flags_with_args: &BTreeSet<&str>,
    defaults: &[String],
) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let mut pattern_found = false;
    let mut after_double_dash = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            let flag = arg.split('=').next().unwrap_or("");
            if ["-e", "--regexp", "-f", "--file"].contains(&flag) {
                pattern_found = true;
            }
            if flags_with_args.contains(flag) && !arg.contains('=') {
                i += 1; // skip flag value
            }
            i += 1;
            continue;
        }
        if !pattern_found {
            pattern_found = true;
            i += 1;
            continue;
        }
        paths.push(arg.clone());
        i += 1;
    }
    if paths.is_empty() {
        defaults.to_vec()
    } else {
        paths
    }
}

fn grep_flags_with_args() -> BTreeSet<&'static str> {
    [
        "-e", "--regexp", "-f", "--file", "--exclude", "--include",
        "--exclude-dir", "--include-dir", "-m", "--max-count",
        "-A", "--after-context", "-B", "--before-context",
        "-C", "--context",
    ]
    .into_iter()
    .collect()
}

fn rg_flags_with_args() -> BTreeSet<&'static str> {
    [
        "-e", "--regexp", "-f", "--file", "-t", "--type",
        "-T", "--type-not", "-g", "--glob", "-m", "--max-count",
        "--max-depth", "-r", "--replace", "-A", "--after-context",
        "-B", "--before-context", "-C", "--context",
    ]
    .into_iter()
    .collect()
}

fn extract_sed_paths(args: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let mut skip_next = false;
    let mut script_found = false;
    let mut after_double_dash = false;

    for (i, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            if ["-f", "--file"].contains(&arg.as_str()) {
                if let Some(script_file) = args.get(i + 1) {
                    paths.push(script_file.clone());
                    skip_next = true;
                }
                script_found = true;
            } else if ["-e", "--expression"].contains(&arg.as_str()) {
                skip_next = true;
                script_found = true;
            } else if arg.contains('e') || arg.contains('f') {
                script_found = true;
            }
            continue;
        }
        if !script_found {
            script_found = true;
            continue;
        }
        paths.push(arg.clone());
    }
    paths
}

fn extract_git_paths(args: &[String]) -> Vec<String> {
    // git diff --no-index explicitly compares arbitrary files
    if args.first().map(|s| s.as_str()) == Some("diff") && args.contains(&"--no-index".into()) {
        let file_paths = filter_out_flags(&args[1..].to_vec());
        return file_paths.into_iter().take(2).collect();
    }
    vec![]
}

fn extract_jq_paths(args: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let flags_with_args = jq_flags_with_args();
    let mut filter_found = false;
    let mut after_double_dash = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            let flag = arg.split('=').next().unwrap_or("");
            if ["-e", "--expression"].contains(&flag) {
                filter_found = true;
            }
            if flags_with_args.contains(flag) && !arg.contains('=') {
                i += 1;
            }
            i += 1;
            continue;
        }
        if !filter_found {
            filter_found = true;
            i += 1;
            continue;
        }
        paths.push(arg.clone());
        i += 1;
    }
    paths
}

fn jq_flags_with_args() -> BTreeSet<&'static str> {
    [
        "-e", "--expression", "-f", "--from-file", "--arg", "--argjson",
        "--slurpfile", "--rawfile", "--args", "--jsonargs",
        "-L", "--library-path", "--indent", "--tab",
    ]
    .into_iter()
    .collect()
}

fn extract_tr_paths(args: &[String]) -> Vec<String> {
    let has_delete = args.iter().any(|a| {
        a == "-d" || a == "--delete" || (a.starts_with('-') && a.contains('d'))
    });
    let non_flags = filter_out_flags(args);
    let skip = if has_delete { 1 } else { 2 };
    non_flags.into_iter().skip(skip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_known_commands() {
        assert_eq!(classify_command("rm"), Some(PathCommand::Rm));
        assert_eq!(classify_command("grep"), Some(PathCommand::Grep));
        assert_eq!(classify_command("sed"), Some(PathCommand::Sed));
        assert_eq!(classify_command("git"), Some(PathCommand::Git));
        assert_eq!(classify_command("unknown_cmd"), None);
    }

    #[test]
    fn filter_out_flags_basic() {
        let args: Vec<String> = ["-r", "-f", "file.txt", "--", "-bad.txt"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let paths = filter_out_flags(&args);
        assert_eq!(paths, vec!["file.txt", "-bad.txt"]);
    }

    #[test]
    fn filter_out_flags_all_positional_after_double_dash() {
        let args: Vec<String> = ["--", "-/../.secret"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let paths = filter_out_flags(&args);
        assert_eq!(paths, vec!["-/../.secret"]);
    }

    #[test]
    fn extract_paths_rm() {
        let args: Vec<String> = ["-rf", "node_modules/", "cache/"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let paths = extract_paths(PathCommand::Rm, &args);
        assert_eq!(paths, vec!["node_modules/", "cache/"]);
    }

    #[test]
    fn extract_paths_cd() {
        let args: Vec<String> = ["/tmp"].iter().map(|s| s.to_string()).collect();
        let paths = extract_paths(PathCommand::Cd, &args);
        assert_eq!(paths, vec!["/tmp"]);
    }

    #[test]
    fn extract_paths_cd_empty_is_home() {
        let paths = extract_paths(PathCommand::Cd, &[]);
        assert_eq!(paths, vec!["~"]);
    }

    #[test]
    fn extract_paths_grep() {
        let args: Vec<String> = ["-r", "pattern", "src/", "tests/"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let paths = extract_paths(PathCommand::Grep, &args);
        assert_eq!(paths, vec!["src/", "tests/"]);
    }

    #[test]
    fn extract_paths_find() {
        let args: Vec<String> = [".", "-name", "*.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let paths = extract_paths(PathCommand::Find, &args);
        assert_eq!(paths, vec!["."]);
    }

    #[test]
    fn parse_simple_command() {
        let args = parse_command_args("rm -rf 'my file.txt' \"other dir\"");
        assert_eq!(args, vec!["rm", "-rf", "my file.txt", "other dir"]);
    }

    #[test]
    fn parse_command_with_escaped_quotes() {
        let args = parse_command_args(r#"echo "hello \"world\"""#);
        assert_eq!(args, vec!["echo", "hello \"world\""]);
    }

    #[test]
    fn parse_empty_string() {
        let args = parse_command_args("");
        assert!(args.is_empty());
    }

    #[test]
    fn is_write_commands() {
        assert!(PathCommand::Rm.is_write());
        assert!(PathCommand::Sed.is_write());
        assert!(!PathCommand::Cat.is_write());
        assert!(!PathCommand::Ls.is_write());
    }
}
