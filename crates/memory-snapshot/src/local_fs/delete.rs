//! `delete()` body for [`super::LocalFsSnapshotter`].
//!
//! Deletes both the bundle and the sibling `.sha256` file. Refuses to
//! drop the agent's last remaining snapshot — operators almost always
//! mean "trim retention", and a `delete` that empties the directory is
//! more often a typo than an intent.

use std::fs;

use crate::error::SnapshotError;
use crate::id::{AgentId, SnapshotId};
use crate::tenant_path::{
    bundle_sha256_sibling, snapshot_bundle_path, validate_agent_id, validate_tenant,
};

use super::list::run_list;
use super::snapshotter::LocalFsSnapshotter;

pub(super) async fn run_delete(
    s: &LocalFsSnapshotter,
    agent_id: &AgentId,
    tenant: &str,
    id: SnapshotId,
) -> Result<(), SnapshotError> {
    let agent_id_v = validate_agent_id(agent_id)?.to_string();
    let tenant_v = validate_tenant(tenant)?.to_string();

    let metas = run_list(s, &agent_id_v, &tenant_v).await?;
    let target = metas
        .iter()
        .find(|m| m.id == id)
        .ok_or(SnapshotError::NotFound(id))?;

    if metas.len() <= 1 {
        return Err(SnapshotError::Retention(format!(
            "refusing to delete last snapshot for agent {agent_id_v} (tenant {tenant_v})"
        )));
    }

    // Try plaintext first, fall back to .age form if that's what
    // exists. Either ext is fine; both live alongside their .sha256
    // sibling.
    let bundle = if target.bundle_path.exists() {
        target.bundle_path.clone()
    } else {
        let plain = snapshot_bundle_path(s.state_root(), &tenant_v, &agent_id_v, id, false)?;
        if plain.exists() {
            plain
        } else {
            snapshot_bundle_path(s.state_root(), &tenant_v, &agent_id_v, id, true)?
        }
    };

    if !bundle.exists() {
        return Err(SnapshotError::NotFound(id));
    }
    let sibling = bundle_sha256_sibling(&bundle);
    fs::remove_file(&bundle)?;
    if sibling.exists() {
        fs::remove_file(&sibling)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::SnapshotRequest;
    use crate::snapshotter::MemorySnapshotter;
    use git2::{IndexAddOption, Repository, Signature};

    fn seed_memdir(memdir: &std::path::Path) {
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

    fn build_snapshotter(state_root: &std::path::Path) -> LocalFsSnapshotter {
        LocalFsSnapshotter::builder()
            .state_root(state_root)
            .memdir_root(state_root.join("memdir"))
            .sqlite_root(state_root.join("sqlite"))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn refuses_to_delete_when_only_one_snapshot_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"));

        let m = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();
        let err = s.delete(&"ana".into(), "default", m.id).await.unwrap_err();
        match err {
            SnapshotError::Retention(msg) => assert!(msg.contains("last snapshot")),
            other => panic!("unexpected err: {other:?}"),
        }
        // Bundle must still exist.
        assert!(m.bundle_path.exists());
    }

    #[tokio::test]
    async fn deletes_bundle_and_sibling_when_more_than_one_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"));

        let m1 = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let _m2 = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();

        s.delete(&"ana".into(), "default", m1.id).await.unwrap();
        assert!(!m1.bundle_path.exists());
        assert!(!bundle_sha256_sibling(&m1.bundle_path).exists());
    }

    #[tokio::test]
    async fn missing_id_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"));
        let _m = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();

        let err = s
            .delete(&"ana".into(), "default", crate::id::SnapshotId::new())
            .await
            .unwrap_err();
        assert!(matches!(err, SnapshotError::NotFound(_)));
    }
}
