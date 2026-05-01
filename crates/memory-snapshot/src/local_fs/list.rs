//! `list()` body for [`super::LocalFsSnapshotter`].
//!
//! Scans the per-agent snapshots dir, opens each `*.tar.zst[.age]`
//! enough to read the embedded manifest, and returns a [`SnapshotMeta`]
//! per bundle ordered by `created_at_ms` descending. Bundles whose
//! manifest cannot be parsed are skipped with a `tracing::warn!` so a
//! single corrupt file does not break the whole listing.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::SnapshotError;
use crate::id::AgentId;
use crate::manifest::Manifest;
use crate::meta::SnapshotMeta;
use crate::tenant_path::{bundle_sha256_sibling, snapshots_dir};

use super::snapshotter::LocalFsSnapshotter;

pub(super) async fn run_list(
    s: &LocalFsSnapshotter,
    agent_id: &AgentId,
    tenant: &str,
) -> Result<Vec<SnapshotMeta>, SnapshotError> {
    let dir = snapshots_dir(s.state_root(), tenant, agent_id)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if !is_bundle_path(&path) {
            continue;
        }
        match read_bundle_meta(&path) {
            Ok(meta) => out.push(meta),
            Err(e) => {
                tracing::warn!(
                    bundle = %path.display(),
                    error = %e,
                    "skipping bundle whose manifest cannot be read"
                );
            }
        }
    }
    out.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms));
    Ok(out)
}

/// `true` for `*.tar.zst` and `*.tar.zst.age`. The staging dir is
/// `.staging-<id>/` so it is never matched here.
fn is_bundle_path(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    if name.starts_with(".staging-") {
        return false;
    }
    name.ends_with(".tar.zst") || name.ends_with(".tar.zst.age")
}

/// Materialize a [`SnapshotMeta`] for a bundle by reading its manifest
/// (and the sibling `.sha256` file when present). Encrypted bundles
/// degrade to a stub [`SnapshotMeta`] derived from the filename + on-disk
/// metadata until the decryption layer is wired in.
fn read_bundle_meta(bundle: &Path) -> Result<SnapshotMeta, SnapshotError> {
    let encrypted = bundle
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.eq_ignore_ascii_case("age"))
        .unwrap_or(false);

    if encrypted {
        // Without the identity we cannot read the manifest; surface a
        // best-effort meta from on-disk metadata + filename UUID.
        return stub_meta_from_filename(bundle);
    }

    let manifest = read_manifest_from_bundle(bundle)?;
    let bundle_size_bytes = fs::metadata(bundle)?.len();

    // Prefer the file-level sibling hash when present; fall back to
    // the artifact-list seal that always lives in the manifest.
    let sibling = bundle_sha256_sibling(bundle);
    let bundle_sha256 = if sibling.exists() {
        fs::read_to_string(&sibling)?.trim().to_string()
    } else {
        manifest.bundle_sha256.clone()
    };

    Ok(SnapshotMeta {
        id: manifest.snapshot_id,
        agent_id: manifest.agent_id,
        tenant: manifest.tenant,
        label: manifest.label,
        created_at_ms: manifest.created_at_ms,
        bundle_path: bundle.to_path_buf(),
        bundle_size_bytes,
        bundle_sha256,
        git_oid: Some(manifest.git.head_oid),
        schema_versions: manifest.schema_versions,
        encrypted,
        redactions_applied: manifest.redactions.is_some(),
    })
}

fn read_manifest_from_bundle(bundle: &Path) -> Result<Manifest, SnapshotError> {
    let f = fs::File::open(bundle)?;
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

fn stub_meta_from_filename(bundle: &Path) -> Result<SnapshotMeta, SnapshotError> {
    use crate::id::SnapshotId;
    use crate::manifest::SchemaVersions;

    let bundle_size_bytes = fs::metadata(bundle)?.len();
    let stem = bundle
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.split('.').next())
        .ok_or_else(|| {
            SnapshotError::MissingArtifact(format!("filename UUID: {}", bundle.display()))
        })?;
    let id: SnapshotId = stem
        .parse()
        .map_err(|_| SnapshotError::MissingArtifact("filename UUID".into()))?;

    let sibling = bundle_sha256_sibling(bundle);
    let bundle_sha256 = if sibling.exists() {
        fs::read_to_string(&sibling)?.trim().to_string()
    } else {
        String::new()
    };

    Ok(SnapshotMeta {
        id,
        agent_id: String::new(),
        tenant: String::new(),
        label: None,
        created_at_ms: fs::metadata(bundle)?
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        bundle_path: PathBuf::from(bundle),
        bundle_size_bytes,
        bundle_sha256,
        git_oid: None,
        schema_versions: SchemaVersions::CURRENT,
        encrypted: true,
        redactions_applied: false,
    })
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
    async fn empty_dir_returns_empty_list() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        let metas = s.list(&"ana".into(), "default").await.unwrap();
        assert!(metas.is_empty());
    }

    #[tokio::test]
    async fn lists_multiple_snapshots_ordered_by_created_at_desc() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"));

        let m1 = s.snapshot(SnapshotRequest::cli("ana", "default")).await.unwrap();
        // Force a strict ordering by sleeping past the millisecond
        // boundary the timestamp uses.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let m2 = s.snapshot(SnapshotRequest::cli("ana", "default")).await.unwrap();

        let metas = s.list(&"ana".into(), "default").await.unwrap();
        assert_eq!(metas.len(), 2);
        assert!(metas[0].created_at_ms >= metas[1].created_at_ms);
        let ids: Vec<_> = metas.iter().map(|m| m.id).collect();
        assert!(ids.contains(&m1.id));
        assert!(ids.contains(&m2.id));
    }

    #[tokio::test]
    async fn corrupt_bundle_is_skipped_with_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"));

        // One real snapshot, then a hand-rolled garbage file with a
        // valid suffix to simulate a half-written or truncated bundle.
        let _good = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();
        let dir = snapshots_dir(tmp.path(), "default", "ana").unwrap();
        fs::write(dir.join("garbage.tar.zst"), b"not a real archive").unwrap();

        let metas = s.list(&"ana".into(), "default").await.unwrap();
        assert_eq!(metas.len(), 1, "corrupt bundle must not abort listing");
    }

    #[tokio::test]
    async fn does_not_match_staging_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        let dir = snapshots_dir(tmp.path(), "default", "ana").unwrap();
        fs::create_dir_all(&dir).unwrap();
        // Manually create a `.staging-<uuid>/` like a crashed snapshot
        // would leave behind. It must NOT show up in list output.
        fs::create_dir(dir.join(".staging-abc")).unwrap();
        let metas = s.list(&"ana".into(), "default").await.unwrap();
        assert!(metas.is_empty());
    }

    #[tokio::test]
    async fn validates_tenant_id_before_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        let err = s.list(&"ana".into(), "BAD").await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("tenant") || msg.contains("[a-z0-9_-]"), "{msg}");
    }
}
