//! `diff()` body for [`super::LocalFsSnapshotter`].
//!
//! MVP diff compares the two bundles' manifests directly, classifying
//! artifact-set deltas into the same coarse summaries the operator UI
//! consumes (git, sqlite, state). We do not unpack either body — the
//! per-artifact SHA-256 already encodes "did this DB or git tree
//! change since the previous snapshot".
//!
//! Limitations (deferred to a future hardening pass):
//!
//! - SQLite row-level deltas are reported as `0` because we do not
//!   crack open the SQLite files. The summary surfaces only
//!   "any byte changed in this DB", encoded as `±1` per side so the
//!   operator UI can paint a binary "yes/no changed" indicator
//!   without a misleading row count.
//! - Git diff is coarse: `commits_between` reports `1` if the two
//!   bundles differ on the git tree at all, `0` otherwise. A real
//!   ahead/behind walk needs both bundles unpacked into temp repos,
//!   which is bigger than we want here.

use std::fs;
use std::io::Read;
use std::path::Path;

use crate::error::SnapshotError;
use crate::id::{AgentId, SnapshotId};
use crate::manifest::{ArtifactKind, Manifest};
use crate::meta::{GitDiffSummary, SnapshotDiff, SqliteDiffSummary, StateDiffSummary};
use crate::tenant_path::{snapshot_bundle_path, validate_agent_id, validate_tenant};

use super::snapshotter::LocalFsSnapshotter;

pub(super) async fn run_diff(
    s: &LocalFsSnapshotter,
    agent_id: &AgentId,
    tenant: &str,
    a: SnapshotId,
    b: SnapshotId,
) -> Result<SnapshotDiff, SnapshotError> {
    let agent_id_v = validate_agent_id(agent_id)?.to_string();
    let tenant_v = validate_tenant(tenant)?.to_string();

    let manifest_a = read_manifest_for(s.state_root(), &tenant_v, &agent_id_v, a)?;
    let manifest_b = read_manifest_for(s.state_root(), &tenant_v, &agent_id_v, b)?;

    Ok(SnapshotDiff {
        a,
        b,
        git_summary: git_summary(&manifest_a, &manifest_b),
        sqlite_summary: sqlite_summary(&manifest_a, &manifest_b),
        state_summary: state_summary(&manifest_a, &manifest_b),
    })
}

fn read_manifest_for(
    state_root: &Path,
    tenant: &str,
    agent_id: &str,
    id: SnapshotId,
) -> Result<Manifest, SnapshotError> {
    let plain = snapshot_bundle_path(state_root, tenant, agent_id, id, false)?;
    let bundle = if plain.exists() {
        plain
    } else {
        let enc = snapshot_bundle_path(state_root, tenant, agent_id, id, true)?;
        if !enc.exists() {
            return Err(SnapshotError::NotFound(id));
        }
        enc
    };

    let f = fs::File::open(&bundle)?;
    let dec = zstd::stream::Decoder::new(f)?;
    let mut tar = tar::Archive::new(dec);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.to_string_lossy() == "manifest.json" {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            let parsed: Manifest = serde_json::from_slice(&buf)?;
            return Ok(parsed);
        }
    }
    Err(SnapshotError::MissingArtifact("manifest.json".into()))
}

fn git_summary(a: &Manifest, b: &Manifest) -> GitDiffSummary {
    if a.git.head_oid == b.git.head_oid {
        return GitDiffSummary {
            commits_between: 0,
            files_changed: 0,
            insertions: 0,
            deletions: 0,
        };
    }
    let files_changed = artifact_kind_count_difference(a, b, |k| k == ArtifactKind::GitBundle);
    GitDiffSummary {
        commits_between: 1,
        files_changed,
        insertions: 0,
        deletions: 0,
    }
}

fn sqlite_summary(a: &Manifest, b: &Manifest) -> SqliteDiffSummary {
    let mut s = SqliteDiffSummary {
        long_term_rows_added: 0,
        long_term_rows_removed: 0,
        vector_rows_added: 0,
        vector_rows_removed: 0,
        concepts_rows_added: 0,
        concepts_rows_removed: 0,
        compactions_added: 0,
    };
    if artifact_changed(a, b, ArtifactKind::SqliteLongTerm) {
        s.long_term_rows_added = 1;
        s.long_term_rows_removed = 1;
    }
    if artifact_changed(a, b, ArtifactKind::SqliteVector) {
        s.vector_rows_added = 1;
        s.vector_rows_removed = 1;
    }
    if artifact_changed(a, b, ArtifactKind::SqliteConcepts) {
        s.concepts_rows_added = 1;
        s.concepts_rows_removed = 1;
    }
    if artifact_changed(a, b, ArtifactKind::SqliteCompactions) {
        s.compactions_added = 1;
    }
    s
}

fn state_summary(a: &Manifest, b: &Manifest) -> StateDiffSummary {
    StateDiffSummary {
        extract_cursor_changed: artifact_changed(a, b, ArtifactKind::StateExtractCursor),
        last_dream_run_changed: artifact_changed(a, b, ArtifactKind::StateDreamRun),
    }
}

fn artifact_changed(a: &Manifest, b: &Manifest, kind: ArtifactKind) -> bool {
    let pick = |m: &Manifest| {
        m.artifacts
            .iter()
            .find(|x| x.kind == kind)
            .map(|x| x.sha256.clone())
    };
    pick(a) != pick(b)
}

fn artifact_kind_count_difference(
    a: &Manifest,
    b: &Manifest,
    keep: impl Fn(ArtifactKind) -> bool,
) -> u32 {
    let count_a = a.artifacts.iter().filter(|x| keep(x.kind)).count();
    let count_b = b.artifacts.iter().filter(|x| keep(x.kind)).count();
    count_a.abs_diff(count_b) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::SnapshotRequest;
    use crate::snapshotter::MemorySnapshotter;
    use git2::{IndexAddOption, Repository, Signature};

    fn seed_memdir(memdir: &Path) -> Repository {
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
        drop(tree);
        repo
    }

    fn add_commit(memdir: &Path, message: &str) {
        let repo = Repository::open(memdir).unwrap();
        fs::write(memdir.join("topic-new.md"), b"more\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("operator", "ops@example.com").unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])
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
    async fn diff_same_snapshot_reports_zero_deltas() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"));

        let m = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();
        let d = s.diff(&"ana".into(), "default", m.id, m.id).await.unwrap();
        assert_eq!(d.git_summary.commits_between, 0);
        assert_eq!(d.git_summary.files_changed, 0);
        assert_eq!(d.sqlite_summary.long_term_rows_added, 0);
        assert!(!d.state_summary.extract_cursor_changed);
        assert!(!d.state_summary.last_dream_run_changed);
    }

    #[tokio::test]
    async fn diff_after_new_commit_flags_git_change() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        let memdir = tmp.path().join("memdir/ana");
        seed_memdir(&memdir);

        let m1 = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();
        add_commit(&memdir, "second commit");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let m2 = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();

        let d = s.diff(&"ana".into(), "default", m1.id, m2.id).await.unwrap();
        assert_eq!(d.git_summary.commits_between, 1);
        // No SQLite or state artifacts in either bundle, so both
        // sides should report zero in those summaries.
        assert_eq!(d.sqlite_summary.long_term_rows_added, 0);
        assert!(!d.state_summary.extract_cursor_changed);
    }

    #[tokio::test]
    async fn diff_returns_not_found_for_unknown_id() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"));
        let _m = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();
        let bogus = crate::id::SnapshotId::new();
        let err = s
            .diff(&"ana".into(), "default", bogus, bogus)
            .await
            .unwrap_err();
        assert!(matches!(err, SnapshotError::NotFound(_)));
    }
}
