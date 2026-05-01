//! Git memdir capture + restore via libgit2.
//!
//! The snapshot bundle ships the repository's `.git/` directory as plain
//! tar entries (zstd handles the packfile compression efficiently) plus
//! a HEAD-oriented [`GitMeta`] in the manifest. On restore the staged
//! `.git/` is copied over the live one after a `pre-restore-<id>` tag is
//! laid down on the existing HEAD so the operation can be rolled back
//! via `git reflog`.
//!
//! Defensive: no shell-out to `git`. libgit2 is already a workspace
//! dependency through `workspace_git.rs` (Phase 10.9 memdir + Phase
//! 80.1.g checkpointer).

use std::path::{Path, PathBuf};

use git2::{Oid, Repository, RepositoryOpenFlags};

use crate::manifest::GitMeta;

/// Open the memdir repo without auto-discovering parent repos. The
/// memdir lives at a fixed path the caller chose; we never want to
/// stumble onto an outer repo (the operator's home, the agent
/// monorepo, …) by accident.
fn open_memdir(memdir: &Path) -> Result<Repository, git2::Error> {
    Repository::open_ext(
        memdir,
        RepositoryOpenFlags::NO_SEARCH,
        std::iter::empty::<&Path>(),
    )
}

/// Read the HEAD commit's identity from the memdir repo.
pub fn read_head_meta(memdir: &Path) -> Result<GitMeta, git2::Error> {
    let repo = open_memdir(memdir)?;
    let head = repo.head()?.peel_to_commit()?;
    let author = head.author();
    let summary = head.summary().unwrap_or("").to_string();
    let name = author.name().unwrap_or("unknown").to_string();
    let email = author.email().unwrap_or("").to_string();
    let when = author.when();
    // libgit2 reports `seconds()` as i64 since epoch; convert to ms for
    // wire-shape consistency with everything else in the manifest.
    let head_ts_ms = when.seconds().saturating_mul(1000);
    Ok(GitMeta {
        head_oid: head.id().to_string(),
        head_subject: summary,
        head_author: format!("{name} <{email}>"),
        head_ts_ms,
    })
}

/// Drop a lightweight tag on the current HEAD. Used pre-restore so the
/// existing state is recoverable via `git reflog show <tag>`.
pub fn tag_pre_restore(memdir: &Path, tag_name: &str) -> Result<Oid, git2::Error> {
    let repo = open_memdir(memdir)?;
    let head = repo.head()?.peel_to_commit()?;
    let oid = repo.tag_lightweight(tag_name, head.as_object(), true)?;
    Ok(oid)
}

/// Walk the `.git` dir of `memdir` and return every file path along with
/// its desired in-bundle path (`git/<relative>`). Used by the snapshot
/// orchestration to feed `tar` entries.
pub fn enumerate_git_files(memdir: &Path) -> std::io::Result<Vec<(PathBuf, String)>> {
    let git_dir = memdir.join(".git");
    if !git_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    walk(&git_dir, &git_dir, &mut out)?;
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
            // Memdir repos never ship symlinks; refuse to follow.
            // Operators inspecting a hostile bundle later get a clean
            // signal rather than a silent mis-pack.
            continue;
        }
        if ft.is_dir() {
            walk(base, &path, out)?;
        } else if ft.is_file() {
            let rel = path
                .strip_prefix(base)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
            let in_bundle = format!("git/{}", rel.display());
            out.push((path.clone(), in_bundle));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{IndexAddOption, Signature};

    fn init_repo_with_commit(dir: &Path) -> Repository {
        let repo = Repository::init(dir).unwrap();
        std::fs::write(dir.join("MEMORY.md"), b"# index\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("operator", "ops@example.com").unwrap();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "snapshot:smoke initial",
            &tree,
            &[],
        )
        .unwrap();
        drop(tree);
        repo
    }

    #[test]
    fn reads_head_meta_after_one_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let _repo = init_repo_with_commit(tmp.path());

        let meta = read_head_meta(tmp.path()).unwrap();
        assert!(meta.head_subject.contains("snapshot:smoke initial"));
        assert!(meta.head_author.contains("operator"));
        assert!(meta.head_author.contains("ops@example.com"));
        assert!(meta.head_oid.len() == 40);
        assert!(meta.head_ts_ms > 0);
    }

    #[test]
    fn enumerate_git_files_returns_relative_paths_under_git_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let _repo = init_repo_with_commit(tmp.path());

        let files = enumerate_git_files(tmp.path()).unwrap();
        assert!(!files.is_empty());
        for (src, in_bundle) in &files {
            assert!(src.exists(), "{}", src.display());
            assert!(in_bundle.starts_with("git/"));
            assert!(!in_bundle.contains(".."));
        }
    }

    #[test]
    fn enumerate_git_files_returns_empty_when_no_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let files = enumerate_git_files(tmp.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn tag_pre_restore_creates_lightweight_tag() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(tmp.path());
        let head_oid = repo.head().unwrap().target().unwrap();
        let tag_oid = tag_pre_restore(tmp.path(), "pre-restore-test").unwrap();
        assert_eq!(tag_oid, head_oid);
        // The tag must resolve back to the same commit.
        let resolved = repo
            .revparse_single("refs/tags/pre-restore-test")
            .unwrap()
            .id();
        assert_eq!(resolved, head_oid);
    }

    #[test]
    fn open_memdir_refuses_unrelated_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // Plain dir, no `.git` initialized.
        let err = read_head_meta(tmp.path()).unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("repository"));
    }
}
