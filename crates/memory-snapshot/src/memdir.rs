//! Enumerate the human-curated files inside an agent's memdir.
//!
//! These live alongside the `.git/` directory (typically `MEMORY.md` +
//! `<topic>.md` files, but any file the operator drops in is captured
//! verbatim) and round-trip through the bundle as `memory_files/*`
//! tar entries so a forensic reader can `tar -xf <bundle>` and read
//! them directly without git tooling. Despite the original name, the
//! walker is **not** filtered by extension — operators sometimes ship
//! pinned JSON / YAML notes alongside their markdown.

use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Walk `memdir`, returning every regular file outside `.git/` as
/// `(source_path, in_bundle_path)`. The bundle path is rooted at
/// `memory_files/<relative>`.
///
/// Files larger than [`MAX_FILE_BYTES`] are skipped with a warn so a
/// single oversize blob does not balloon the bundle.
pub fn enumerate_memdir_files(memdir: &Path) -> std::io::Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    if !memdir.exists() {
        return Ok(out);
    }
    walk(memdir, memdir, &mut out)?;
    Ok(out)
}

fn walk(
    base: &Path,
    cur: &Path,
    out: &mut Vec<(PathBuf, String)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(cur)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            // Refuse to follow — same defensive choice as git_capture.
            continue;
        }
        if ft.is_dir() {
            // Skip the git store; it is handled by `git_capture` and
            // any nested directory the operator may have created
            // (e.g. `topics/`) is still walked.
            if path.file_name().and_then(|n| n.to_str()) == Some(".git") {
                continue;
            }
            walk(base, &path, out)?;
        } else if ft.is_file() {
            let metadata = entry.metadata()?;
            if metadata.len() > MAX_FILE_BYTES {
                tracing::warn!(
                    path = %path.display(),
                    size_bytes = metadata.len(),
                    cap_bytes = MAX_FILE_BYTES,
                    "memdir file exceeds snapshot cap; skipping"
                );
                continue;
            }
            let rel = path
                .strip_prefix(base)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
            let in_bundle = format!("memory_files/{}", rel.display());
            out.push((path.clone(), in_bundle));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn enumerates_top_level_and_nested_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("MEMORY.md"), "# index\n").unwrap();
        fs::write(tmp.path().join("topic-a.md"), "# a\n").unwrap();
        // Non-markdown sibling: operators sometimes pin JSON/YAML notes.
        fs::write(tmp.path().join("pins.json"), "{}\n").unwrap();
        fs::create_dir(tmp.path().join("topics")).unwrap();
        fs::write(tmp.path().join("topics/b.md"), "# b\n").unwrap();

        let mut found = enumerate_memdir_files(tmp.path()).unwrap();
        found.sort_by(|a, b| a.1.cmp(&b.1));
        let labels: Vec<_> = found.iter().map(|(_, b)| b.clone()).collect();
        assert_eq!(
            labels,
            vec![
                "memory_files/MEMORY.md".to_string(),
                "memory_files/pins.json".to_string(),
                "memory_files/topic-a.md".to_string(),
                "memory_files/topics/b.md".to_string(),
            ]
        );
    }

    #[test]
    fn skips_dot_git_subtree() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::write(tmp.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(tmp.path().join("MEMORY.md"), "# x\n").unwrap();

        let found = enumerate_memdir_files(tmp.path()).unwrap();
        let paths: Vec<_> = found.iter().map(|(_, b)| b.as_str()).collect();
        assert_eq!(paths, vec!["memory_files/MEMORY.md"]);
        assert!(paths.iter().all(|p| !p.contains(".git")));
    }

    #[test]
    fn returns_empty_when_dir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        let found = enumerate_memdir_files(&missing).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn does_not_follow_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target.md");
        fs::write(&target, "# target\n").unwrap();
        let link = tmp.path().join("link.md");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(not(unix))]
        return; // symlink behavior is platform-specific; skip on Windows

        let found = enumerate_memdir_files(tmp.path()).unwrap();
        let labels: Vec<_> = found.iter().map(|(_, b)| b.as_str()).collect();
        assert!(labels.contains(&"memory_files/target.md"));
        assert!(!labels.iter().any(|l| l.ends_with("link.md")));
    }
}
