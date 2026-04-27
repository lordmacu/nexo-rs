use std::path::{Path, PathBuf};

/// Canonical state directory. Mirrors the nexo-tunnel convention:
/// `$NEXO_HOME/state/` or `~/.nexo/state/` when NEXO_HOME is unset.
pub fn nexo_state_dir() -> PathBuf {
    let home = std::env::var_os("NEXO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".nexo")
        });
    home.join("state")
}

/// Write the active workspace path to `state_dir/active_workspace_path`.
/// Uses temp-file + rename so a concurrent read never sees a torn write.
pub fn write_active_workspace_to(state_dir: &Path, path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let target = state_dir.join("active_workspace_path");
    let tmp = state_dir.join("active_workspace_path.tmp");
    std::fs::write(&tmp, path.to_string_lossy().as_bytes())?;
    std::fs::rename(&tmp, &target)
}

/// Read back the saved workspace path from `state_dir/active_workspace_path`.
/// Returns `None` when the file is missing, the path no longer exists on disk,
/// or the content is empty/unreadable.
pub fn read_active_workspace_from(state_dir: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(state_dir.join("active_workspace_path")).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let p = PathBuf::from(trimmed);
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// Convenience wrappers that use the canonical `$NEXO_HOME/state/` directory.
pub fn write_active_workspace(path: &Path) -> std::io::Result<()> {
    write_active_workspace_to(&nexo_state_dir(), path)
}

pub fn read_active_workspace() -> Option<PathBuf> {
    read_active_workspace_from(&nexo_state_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_then_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("my_project");
        std::fs::create_dir_all(&workspace).unwrap();

        write_active_workspace_to(dir.path(), &workspace).unwrap();
        let recovered = read_active_workspace_from(dir.path()).unwrap();
        assert_eq!(recovered, workspace);
    }

    #[test]
    fn read_missing_file_returns_none() {
        let dir = TempDir::new().unwrap();
        assert!(read_active_workspace_from(dir.path()).is_none());
    }

    #[test]
    fn read_nonexistent_path_returns_none() {
        let dir = TempDir::new().unwrap();
        // Write a path that doesn't exist on disk — must return None.
        let ghost = dir.path().join("ghost_project");
        write_active_workspace_to(dir.path(), &ghost).unwrap();
        assert!(read_active_workspace_from(dir.path()).is_none());
    }
}
