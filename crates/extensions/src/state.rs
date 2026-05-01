//! Phase 82.6 — per-extension state directory convention.
//!
//! Extensions need a stable place to put SQLite databases,
//! vault files, and per-tenant artifacts. The convention
//! existed informally; this module formalises it so:
//! - the daemon creates the directory idempotently at first
//!   spawn and passes it through `initialize` env block;
//! - operators inspect it via `nexo extension state-dir <id>`;
//! - microapp authors point their on-disk state at it without
//!   reinventing the path layout.
//!
//! Layout (mirrors the existing `nexo_state_dir()` convention
//! used by the audit DB + agent registry):
//!
//! ```text
//! $NEXO_HOME/extensions/<id>/state/
//! ```
//!
//! `NEXO_HOME` falls back to `$HOME/.nexo` when unset, then to
//! the current working directory if even `HOME` is missing
//! (rare; covers minimal CI containers).

use std::path::PathBuf;

/// Env var the daemon stamps onto the extension's `initialize`
/// env block. Microapp boot reads this to locate its own state
/// directory without reimplementing the path resolution.
pub const EXTENSION_STATE_ROOT_ENV: &str = "NEXO_EXTENSION_STATE_ROOT";

/// Resolve the parent directory that owns every extension's
/// state subtree (`$NEXO_HOME/extensions`). Internal helper —
/// callers usually want [`state_dir_for`] for one specific
/// extension.
pub fn extension_state_root() -> PathBuf {
    nexo_home().join("extensions")
}

/// Resolve the canonical state directory for `extension_id`.
/// Pure path computation — does NOT touch the filesystem.
/// Pair with [`ensure_state_dir`] to materialise it.
pub fn state_dir_for(extension_id: &str) -> PathBuf {
    extension_state_root()
        .join(extension_id)
        .join("state")
}

/// Build [`state_dir_for`] AND `mkdir -p` it. Idempotent. Any
/// caller can run this safely on every extension start without
/// special-casing first-run vs subsequent-run.
///
/// Errors propagate — typically permission issues that the
/// operator must fix before the extension can persist
/// anything. The daemon should mark the extension `unhealthy`
/// on failure rather than swallow.
pub fn ensure_state_dir(extension_id: &str) -> std::io::Result<PathBuf> {
    let path = state_dir_for(extension_id);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn nexo_home() -> PathBuf {
    if let Some(custom) = std::env::var_os("NEXO_HOME") {
        return PathBuf::from(custom);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".nexo");
    }
    PathBuf::from(".nexo")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `NEXO_HOME` is a process-global. Tests that mutate it
    /// must serialise — running `cargo test` with the default
    /// thread pool would otherwise race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn state_dir_for_uses_nexo_home_override() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_HOME", dir.path());
        let resolved = state_dir_for("agent-creator");
        assert_eq!(
            resolved,
            dir.path()
                .join("extensions")
                .join("agent-creator")
                .join("state"),
        );
        std::env::remove_var("NEXO_HOME");
    }

    #[test]
    fn ensure_state_dir_is_idempotent() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_HOME", dir.path());
        let first = ensure_state_dir("foo").unwrap();
        assert!(first.exists());
        let second = ensure_state_dir("foo").unwrap();
        assert_eq!(first, second);
        assert!(second.exists());
        std::env::remove_var("NEXO_HOME");
    }

    #[test]
    fn state_dir_for_does_not_touch_filesystem() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_HOME", dir.path());
        let path = state_dir_for("never-created");
        // Pure path computation — directory must NOT exist.
        assert!(!path.exists());
        std::env::remove_var("NEXO_HOME");
    }

    #[test]
    fn extension_state_root_default_falls_back_to_home() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("NEXO_HOME");
        // Spoof HOME to keep the test hermetic.
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());
        let root = extension_state_root();
        assert!(
            root.starts_with(dir.path()),
            "root {} should start with HOME {}",
            root.display(),
            dir.path().display(),
        );
        assert_eq!(root.file_name().unwrap(), "extensions");
    }
}
