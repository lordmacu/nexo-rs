//! `export()` body for [`super::LocalFsSnapshotter`].
//!
//! Plain copy of a bundle (and its sibling `.sha256`) to an
//! operator-specified destination so the bundle can move between hosts
//! / cold storage. Re-redaction (unpack → SecretGuard → re-pack) is
//! left to a follow-up pass; until that lands a caller asking for
//! redaction on an unredacted bundle gets a clear error rather than
//! silently shipping unredacted bytes.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::SnapshotError;
use crate::id::{AgentId, SnapshotId};
use crate::tenant_path::{
    bundle_sha256_sibling, snapshot_bundle_path, validate_agent_id, validate_tenant,
};

use super::snapshotter::LocalFsSnapshotter;

pub(super) async fn run_export(
    s: &LocalFsSnapshotter,
    agent_id: &AgentId,
    tenant: &str,
    id: SnapshotId,
    target: &Path,
) -> Result<PathBuf, SnapshotError> {
    let agent_id_v = validate_agent_id(agent_id)?.to_string();
    let tenant_v = validate_tenant(tenant)?.to_string();

    let plain = snapshot_bundle_path(s.state_root(), &tenant_v, &agent_id_v, id, false)?;
    let bundle = if plain.exists() {
        plain
    } else {
        let enc = snapshot_bundle_path(s.state_root(), &tenant_v, &agent_id_v, id, true)?;
        if !enc.exists() {
            return Err(SnapshotError::NotFound(id));
        }
        enc
    };

    let target_path = if target.is_dir() {
        let name = bundle
            .file_name()
            .ok_or_else(|| SnapshotError::Io(std::io::Error::other("bundle filename missing")))?;
        target.join(name)
    } else {
        target.to_path_buf()
    };

    if let Some(parent) = target_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    fs::copy(&bundle, &target_path)?;

    // Always carry the sibling hash with the bundle. A bundle copied
    // without its sibling cannot be verify-checked at the file level,
    // and we always want that signal at rest.
    let sibling = bundle_sha256_sibling(&bundle);
    if sibling.exists() {
        fs::copy(sibling, bundle_sha256_sibling(&target_path))?;
    }

    Ok(target_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::SnapshotRequest;
    use crate::snapshotter::MemorySnapshotter;
    use git2::{IndexAddOption, Repository, Signature};

    fn seed_memdir(memdir: &Path) {
        fs::create_dir_all(memdir).unwrap();
        let repo = Repository::init(memdir).unwrap();
        fs::write(memdir.join("MEMORY.md"), b"# x\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("operator", "ops@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "seed", &tree, &[])
            .unwrap();
    }

    fn build_snapshotter(state_root: &Path) -> LocalFsSnapshotter {
        LocalFsSnapshotter::builder()
            .state_root(state_root)
            .memdir_root(state_root.join("memdir"))
            .sqlite_root(state_root.join("sqlite"))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn copies_bundle_and_sibling_hash_to_target_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"));

        let m = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();
        let dest = tempfile::tempdir().unwrap();
        let exported = s
            .export(&"ana".into(), "default", m.id, dest.path())
            .await
            .unwrap();
        assert!(exported.exists());
        assert!(bundle_sha256_sibling(&exported).exists());

        // Bytes match the original.
        let original = fs::read(&m.bundle_path).unwrap();
        let copied = fs::read(&exported).unwrap();
        assert_eq!(original, copied);
    }

    #[tokio::test]
    async fn copies_to_explicit_target_path() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"));

        let m = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();
        let dest = tempfile::tempdir().unwrap();
        let target = dest.path().join("renamed.tar.zst");
        let exported = s
            .export(&"ana".into(), "default", m.id, &target)
            .await
            .unwrap();
        assert_eq!(exported, target);
        assert!(target.exists());
    }

    #[tokio::test]
    async fn returns_not_found_for_missing_id() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"));
        let _m = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();

        let dest = tempfile::tempdir().unwrap();
        let err = s
            .export(
                &"ana".into(),
                "default",
                crate::id::SnapshotId::new(),
                dest.path(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SnapshotError::NotFound(_)));
    }
}
