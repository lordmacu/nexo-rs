//! Phase 11.7 — CLI subcommands for inspecting and administering
//! extensions without booting the agent runtime.
//!
//! All commands are pure functions that take a [`CliContext`]; the binary
//! (`src/main.rs`) is a thin adapter around these.

use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub mod commands;
pub mod doctor;
pub mod format;
pub mod install;
pub mod status;
pub mod yaml_edit;

pub use commands::{run_disable, run_doctor, run_enable, run_info, run_list, run_validate};
pub use doctor::{
    run_doctor_runtime, BrokerClientForDoctor, DoctorOptions, Outcome, RuntimeCheckResult,
};
pub use install::{
    run_install, run_uninstall, InstallMode, InstallOptions, InstallOutcome, UninstallOptions,
};
pub use status::CliStatus;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("extension '{0}' not found")]
    NotFound(String),
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    #[error("config write failed: {0}")]
    ConfigWrite(String),
    #[error("invalid id '{0}': {1}")]
    InvalidId(String, String),
    #[error("target already exists: {0} (use --update to replace)")]
    AlreadyExists(PathBuf),
    #[error("id '{id}' collides: found at {found_at} and {other_at}")]
    IdCollision {
        id: String,
        found_at: PathBuf,
        other_at: PathBuf,
    },
    #[error("uninstall requires --yes to confirm")]
    MissingConfirmation,
    #[error("copy failed: {0}")]
    CopyFailed(String),
    #[error("invalid source: {0}")]
    InvalidSource(String),
    #[error("--link requires an absolute source path")]
    LinkRequiresAbsolute,
    #[error("--update requires extension '{0}' already installed")]
    UpdateTargetMissing(String),
    #[error("{0} runtime check(s) failed")]
    RuntimeCheckFailed(usize),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl CliError {
    /// Stable exit code mapping — scripts rely on these.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotFound(_) => 1,
            Self::UpdateTargetMissing(_) => 1,
            Self::InvalidManifest(_) => 2,
            Self::InvalidSource(_) => 2,
            Self::LinkRequiresAbsolute => 2,
            Self::ConfigWrite(_) => 3,
            Self::InvalidId(..) => 4,
            Self::AlreadyExists(_) => 5,
            Self::IdCollision { .. } => 6,
            Self::MissingConfirmation => 7,
            Self::CopyFailed(_) => 8,
            Self::RuntimeCheckFailed(_) => 9,
            Self::Io(_) => 1,
        }
    }
}

pub struct CliContext<'a> {
    pub config_dir: PathBuf,
    pub extensions: agent_config::ExtensionsConfig,
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

/// One-liner help printed from `agent ext` or `agent ext --help`.
pub fn print_help(out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(out, "agent ext — extension administration")?;
    writeln!(out)?;
    writeln!(out, "Usage:")?;
    writeln!(out, "  agent ext list [--json]")?;
    writeln!(out, "  agent ext info <id> [--json]")?;
    writeln!(out, "  agent ext enable <id>")?;
    writeln!(out, "  agent ext disable <id>")?;
    writeln!(out, "  agent ext validate <path>")?;
    writeln!(out, "  agent ext doctor [--runtime] [--json]")?;
    writeln!(out, "  agent ext install <path> [--update] [--enable] [--dry-run] [--link] [--json]")?;
    writeln!(out, "  agent ext uninstall <id> --yes [--json]")?;
    writeln!(out)?;
    writeln!(out, "Exit codes:")?;
    writeln!(out, "  0  success")?;
    writeln!(out, "  1  extension not found / --update target missing")?;
    writeln!(out, "  2  invalid manifest / invalid source / --link needs absolute path")?;
    writeln!(out, "  3  config write failed")?;
    writeln!(out, "  4  invalid id (empty or reserved)")?;
    writeln!(out, "  5  target already exists (use --update)")?;
    writeln!(out, "  6  id collision across search paths")?;
    writeln!(out, "  7  uninstall missing --yes confirmation")?;
    writeln!(out, "  8  copy or atomic swap failed")?;
    writeln!(out, "  9  one or more runtime checks failed (--runtime)")?;
    writeln!(out)?;
    writeln!(out, "Note: `enable`/`disable` rewrite `config/extensions.yaml`.")?;
    writeln!(out, "Inline comments in that file are not preserved.")?;
    Ok(())
}

/// Compute the absolute path to `<config_dir>/extensions.yaml`.
pub(crate) fn extensions_yaml_path(config_dir: &Path) -> PathBuf {
    config_dir.join("extensions.yaml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_spec() {
        assert_eq!(CliError::NotFound("x".into()).exit_code(), 1);
        assert_eq!(CliError::UpdateTargetMissing("x".into()).exit_code(), 1);
        assert_eq!(CliError::InvalidManifest("x".into()).exit_code(), 2);
        assert_eq!(CliError::InvalidSource("x".into()).exit_code(), 2);
        assert_eq!(CliError::LinkRequiresAbsolute.exit_code(), 2);
        assert_eq!(CliError::ConfigWrite("x".into()).exit_code(), 3);
        assert_eq!(CliError::InvalidId("x".into(), "y".into()).exit_code(), 4);
        assert_eq!(
            CliError::AlreadyExists(PathBuf::from("/x")).exit_code(),
            5
        );
        assert_eq!(
            CliError::IdCollision {
                id: "a".into(),
                found_at: PathBuf::from("/a"),
                other_at: PathBuf::from("/b"),
            }
            .exit_code(),
            6
        );
        assert_eq!(CliError::MissingConfirmation.exit_code(), 7);
        assert_eq!(CliError::CopyFailed("x".into()).exit_code(), 8);
    }

    #[test]
    fn help_mentions_subcommands() {
        let mut out = Vec::new();
        print_help(&mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("list"));
        assert!(s.contains("enable"));
        assert!(s.contains("validate"));
        assert!(s.contains("doctor"));
    }
}
