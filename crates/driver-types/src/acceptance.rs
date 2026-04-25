//! Objective acceptance criteria evaluated AFTER the CLI claims
//! "done". Phase 67.5 owns the actual evaluator; here we only define
//! the contract.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcceptanceCriterion {
    /// Run a shell command. Pass when exit code matches `expected_exit`
    /// (default 0) AND `stdout_must_match` (if set) is satisfied.
    ShellCommand {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_exit: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_must_match: Option<String>,
        #[serde(default = "default_shell_timeout")]
        timeout_secs: u64,
    },
    /// File at `path` must exist (when `required`) AND match `regex`.
    FileMatches {
        path: String,
        regex: String,
        #[serde(default = "default_true")]
        required: bool,
    },
    /// Custom verifier dispatched by name in 67.5. The arg shape is
    /// agreed between caller and verifier — opaque to this crate.
    Custom {
        name: String,
        args: serde_json::Value,
    },
}

fn default_shell_timeout() -> u64 {
    300
}
fn default_true() -> bool {
    true
}

impl AcceptanceCriterion {
    /// Build a default `ShellCommand` (exit 0, no stdout match, 5 min).
    pub fn shell(command: impl Into<String>) -> Self {
        Self::ShellCommand {
            command: command.into(),
            expected_exit: None,
            stdout_must_match: None,
            timeout_secs: default_shell_timeout(),
        }
    }

    /// Build a `FileMatches` that requires the file to exist.
    pub fn file(path: impl Into<String>, regex: impl Into<String>) -> Self {
        Self::FileMatches {
            path: path.into(),
            regex: regex.into(),
            required: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceVerdict {
    pub met: bool,
    #[serde(default)]
    pub failures: Vec<AcceptanceFailure>,
    pub evaluated_at: DateTime<Utc>,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceFailure {
    /// Index into the goal's `acceptance` vector — pivot for dashboards.
    pub criterion_index: usize,
    pub criterion_label: String,
    pub message: String,
    /// Truncated snippet of stdout/stderr / file slice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_builder_uses_defaults() {
        match AcceptanceCriterion::shell("cargo build") {
            AcceptanceCriterion::ShellCommand {
                command,
                expected_exit,
                stdout_must_match,
                timeout_secs,
            } => {
                assert_eq!(command, "cargo build");
                assert_eq!(expected_exit, None);
                assert_eq!(stdout_must_match, None);
                assert_eq!(timeout_secs, 300);
            }
            other => panic!("expected ShellCommand, got {other:?}"),
        }
    }

    #[test]
    fn file_builder_required_by_default() {
        match AcceptanceCriterion::file("PHASES.md", r"67\.0.*✅") {
            AcceptanceCriterion::FileMatches { required, .. } => assert!(required),
            other => panic!("expected FileMatches, got {other:?}"),
        }
    }
}
