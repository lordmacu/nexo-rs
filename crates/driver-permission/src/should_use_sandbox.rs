//! Phase 77.10 — shouldUseSandbox heuristic.
//!
//! Ported from `claude-code-leak/src/tools/BashTool/shouldUseSandbox.ts`.
//! Decides whether a Bash command should be wrapped in a sandbox
//! (bubblewrap / firejail). Probes for the sandbox backend once at
//! construction time; the decision function is pure and allocation-free.
//!
//! Actual command wrapping is out of scope — this module only answers
//! the boolean "should we sandbox?" question.

use std::process::Command;

/// Config knob for sandbox behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// Probe for bwrap/firejail; sandbox only when a backend is found.
    Auto,
    /// Always sandbox — callers must handle missing-backend errors.
    Always,
    /// Never sandbox.
    Never,
}

/// Detected sandbox backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    Bubblewrap,
    Firejail,
    /// No sandbox backend found on PATH.
    None,
}

/// One-shot probe that scans for `bwrap` and `firejail` on PATH.
/// Cache the result — the probe runs a subprocess, so it's not free.
#[derive(Debug, Clone)]
pub struct SandboxProbe {
    pub backend: SandboxBackend,
}

impl SandboxProbe {
    /// Probe for sandbox backends. Runs `which bwrap` and
    /// `which firejail`; prefers bubblewrap when both are present.
    pub fn new() -> Self {
        let backend = if probe_binary("bwrap") {
            SandboxBackend::Bubblewrap
        } else if probe_binary("firejail") {
            SandboxBackend::Firejail
        } else {
            SandboxBackend::None
        };
        Self { backend }
    }

    pub fn backend(&self) -> SandboxBackend {
        self.backend
    }
}

impl Default for SandboxProbe {
    fn default() -> Self {
        Self::new()
    }
}

/// Check whether a binary is on PATH by running `which <name>`.
fn probe_binary(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Decide whether a Bash command should be sandboxed.
///
/// Ported from `shouldUseSandbox.ts:130-153`.
///
/// Rules (first match wins):
/// 1. `Never` mode → false
/// 2. `dangerously_disable_sandbox` → false
/// 3. No command → false
/// 4. Command matches an excluded pattern → false
/// 5. `Always` mode → true
/// 6. `Auto` mode → true if backend is available
pub fn should_use_sandbox(
    command: Option<&str>,
    mode: SandboxMode,
    backend: SandboxBackend,
    dangerously_disable_sandbox: bool,
    excluded_commands: &[String],
) -> bool {
    if matches!(mode, SandboxMode::Never) {
        return false;
    }

    if dangerously_disable_sandbox {
        return false;
    }

    let cmd = match command {
        Some(c) => c.trim(),
        None => return false,
    };
    if cmd.is_empty() {
        return false;
    }

    // Check excluded commands — prefix or exact match against the
    // first word of the command (after stripping leading env vars).
    if !excluded_commands.is_empty() {
        let first_word = strip_leading_env_vars(cmd)
            .split_whitespace()
            .next()
            .unwrap_or("");
        for pattern in excluded_commands {
            if command_matches_pattern(first_word, pattern) {
                return false;
            }
        }
    }

    match mode {
        SandboxMode::Always => true,
        SandboxMode::Auto => !matches!(backend, SandboxBackend::None),
        SandboxMode::Never => unreachable!(), // handled above
    }
}

/// Strip leading `KEY=value` env-var assignments from a command.
/// Handles simple cases: `FOO=bar cmd args` → `cmd args`.
fn strip_leading_env_vars(cmd: &str) -> &str {
    let bytes = cmd.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        // Skip whitespace
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let key_start = pos;
        // Scan the potential key name
        while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
            pos += 1;
        }
        // If followed by '=', this is a KEY=value env-var assignment
        if pos < bytes.len() && bytes[pos] == b'=' && pos > key_start {
            // Skip '=' and the value up to the next whitespace
            pos += 1;
            while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            // Loop back to check for another env var
        } else {
            // Not an env var — stop here
            return &cmd[key_start..];
        }
    }
    ""
}

/// Match a command word against an excluded-command pattern.
/// Supports:
/// - `cmd` → exact match
/// - `cmd:*` → prefix match (`cmd sub args` matches)
fn command_matches_pattern(first_word: &str, pattern: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(":*") {
        first_word == prefix
    } else if let Some(prefix) = pattern.strip_suffix(':') {
        // "cmd:" without * — treat as prefix
        first_word == prefix
    } else {
        first_word == pattern
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_excluded() -> Vec<String> {
        vec![]
    }

    // ── SandboxProbe ──

    #[test]
    fn probe_default_constructs() {
        let probe = SandboxProbe::default();
        // Probe result depends on host; just check it doesn't panic
        let _ = probe.backend();
    }

    #[test]
    fn probe_is_idempotent() {
        let probe = SandboxProbe::new();
        let b1 = probe.backend();
        let b2 = probe.backend();
        assert_eq!(b1, b2);
    }

    // ── Mode: Never ──

    #[test]
    fn never_mode_always_false() {
        assert!(!should_use_sandbox(
            Some("rm -rf /"),
            SandboxMode::Never,
            SandboxBackend::Bubblewrap,
            false,
            &no_excluded(),
        ));
    }

    // ── Mode: Always ──

    #[test]
    fn always_mode_true_even_without_backend() {
        // Always mode means "I want sandbox" — caller handles
        // the missing-backend error separately.
        assert!(should_use_sandbox(
            Some("rm -rf /"),
            SandboxMode::Always,
            SandboxBackend::None,
            false,
            &no_excluded(),
        ));
    }

    // ── dangerlyDisableSandbox ──

    #[test]
    fn dangerously_disable_sandbox_overrides() {
        assert!(!should_use_sandbox(
            Some("rm -rf /"),
            SandboxMode::Always,
            SandboxBackend::Bubblewrap,
            true,
            &no_excluded(),
        ));
    }

    // ── No command ──

    #[test]
    fn no_command_returns_false() {
        assert!(!should_use_sandbox(
            None,
            SandboxMode::Auto,
            SandboxBackend::Bubblewrap,
            false,
            &no_excluded(),
        ));
    }

    #[test]
    fn empty_command_returns_false() {
        assert!(!should_use_sandbox(
            Some("   "),
            SandboxMode::Auto,
            SandboxBackend::Bubblewrap,
            false,
            &no_excluded(),
        ));
    }

    // ── Auto mode ──

    #[test]
    fn auto_mode_with_backend_returns_true() {
        assert!(should_use_sandbox(
            Some("ls -la"),
            SandboxMode::Auto,
            SandboxBackend::Bubblewrap,
            false,
            &no_excluded(),
        ));
        assert!(should_use_sandbox(
            Some("ls -la"),
            SandboxMode::Auto,
            SandboxBackend::Firejail,
            false,
            &no_excluded(),
        ));
    }

    #[test]
    fn auto_mode_without_backend_returns_false() {
        assert!(!should_use_sandbox(
            Some("ls -la"),
            SandboxMode::Auto,
            SandboxBackend::None,
            false,
            &no_excluded(),
        ));
    }

    // ── Excluded commands ──

    #[test]
    fn excluded_command_exact_match() {
        let excluded = vec!["docker".into()];
        assert!(!should_use_sandbox(
            Some("docker ps"),
            SandboxMode::Auto,
            SandboxBackend::Bubblewrap,
            false,
            &excluded,
        ));
    }

    #[test]
    fn excluded_command_prefix_match() {
        let excluded = vec!["npm:*".into()];
        assert!(!should_use_sandbox(
            Some("npm run test"),
            SandboxMode::Auto,
            SandboxBackend::Bubblewrap,
            false,
            &excluded,
        ));
        // Different command still sandboxed
        assert!(should_use_sandbox(
            Some("pip install"),
            SandboxMode::Auto,
            SandboxBackend::Bubblewrap,
            false,
            &excluded,
        ));
    }

    #[test]
    fn excluded_command_with_env_vars() {
        // FOO=bar npm test → first word after env strip is "npm"
        let excluded = vec!["npm:*".into()];
        assert!(!should_use_sandbox(
            Some("FOO=bar npm test"),
            SandboxMode::Auto,
            SandboxBackend::Bubblewrap,
            false,
            &excluded,
        ));
    }

    #[test]
    fn non_excluded_command_sandboxed() {
        let excluded = vec!["docker".into(), "npm:*".into()];
        assert!(should_use_sandbox(
            Some("pip install requests"),
            SandboxMode::Auto,
            SandboxBackend::Bubblewrap,
            false,
            &excluded,
        ));
    }

    // ── strip_leading_env_vars ──

    #[test]
    fn strip_simple_env_var() {
        assert_eq!(strip_leading_env_vars("FOO=bar ls -la"), "ls -la");
    }

    #[test]
    fn strip_multiple_env_vars() {
        assert_eq!(
            strip_leading_env_vars("A=1 B=2 C=3 cmd arg"),
            "cmd arg"
        );
    }

    #[test]
    fn strip_no_env_vars() {
        assert_eq!(strip_leading_env_vars("ls -la"), "ls -la");
    }

    #[test]
    fn strip_empty_string() {
        assert_eq!(strip_leading_env_vars(""), "");
    }

    // ── command_matches_pattern ──

    #[test]
    fn exact_match() {
        assert!(command_matches_pattern("docker", "docker"));
        assert!(!command_matches_pattern("docker", "npm"));
    }

    #[test]
    fn prefix_match_with_star() {
        assert!(command_matches_pattern("npm", "npm:*"));
        assert!(command_matches_pattern("cargo", "cargo:*"));
        assert!(!command_matches_pattern("pip", "npm:*"));
    }

    #[test]
    fn prefix_match_without_star() {
        assert!(command_matches_pattern("npm", "npm:"));
    }
}
