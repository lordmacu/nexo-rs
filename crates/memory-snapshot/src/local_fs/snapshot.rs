//! `snapshot()` body for [`super::LocalFsSnapshotter`].
//!
//! The flow on the happy path:
//!
//! 1. Validate `(tenant, agent_id)` and resolve the snapshots dir
//!    under `<state_root>/tenants/<tenant>/snapshots/<agent_id>/`.
//! 2. Acquire the per-agent lock (timeout → [`SnapshotError::Concurrent`]).
//! 3. Read the memdir's HEAD via `git_capture` (best-effort: agents
//!    without a memdir still snapshot, just with no git artifact).
//! 4. SQLite backups for `long_term`, `vector`, `concepts`, `compactions`
//!    via `VACUUM INTO` into a staging dir.
//! 5. State provider capture — extractor cursor + last dream-run row.
//! 6. Enumerate markdown files in the memdir + the `.git/` tree.
//! 7. Compute per-artifact SHA-256, lay them into [`Manifest::artifacts`]
//!    in declared order, seal the manifest with `bundle_sha256` =
//!    SHA-256 of the concatenated per-artifact hex digests.
//! 8. Stream every artifact + manifest into a `tar.zst.partial`,
//!    hashing the bytes as they leave so a sibling `<id>.tar.zst.sha256`
//!    file gets the whole-bundle hash.
//! 9. Atomic rename `.partial → final`, drop the staging dir, build
//!    [`SnapshotMeta`].
//!
//! Encryption (`age`) and redaction (`SecretGuard`) are layered on
//! top of this body in dedicated modules; what lives here is the
//! unencrypted, unredacted happy path.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::codec::sha256_stream::{sha256_hex, HashingWriter};
use crate::codec::tar_zst::{pack_files, PackEntry};
use crate::error::SnapshotError;
use crate::git_capture::{enumerate_git_files, read_head_meta};
use crate::id::SnapshotId;
use crate::manifest::{
    ArtifactKind, ArtifactMeta, GitMeta, Manifest, SchemaVersions, ToolVersions, BUNDLE_FORMAT,
    MANIFEST_VERSION,
};
use crate::memdir::enumerate_memdir_files;
use crate::redaction::{redact_staging_dir, DefaultRedactionPolicy};
use crate::meta::SnapshotMeta;
use crate::request::SnapshotRequest;
use crate::sqlite_backup::backup_named;
use crate::tenant_path::{
    bundle_sha256_sibling, snapshot_bundle_path, snapshots_dir, validate_agent_id, validate_tenant,
};

use super::snapshotter::LocalFsSnapshotter;

const SQLITE_DBS: &[(&str, ArtifactKind)] = &[
    ("long_term", ArtifactKind::SqliteLongTerm),
    ("vector", ArtifactKind::SqliteVector),
    ("concepts", ArtifactKind::SqliteConcepts),
    ("compactions", ArtifactKind::SqliteCompactions),
];

pub(super) async fn run_snapshot(
    s: &LocalFsSnapshotter,
    req: SnapshotRequest,
) -> Result<SnapshotMeta, SnapshotError> {
    let agent_id = validate_agent_id(&req.agent_id)?.to_string();
    let tenant = validate_tenant(&req.tenant)?.to_string();

    let _lock = s.locks().acquire(&agent_id, s.lock_timeout()).await?;

    let snapshots_dir_path = snapshots_dir(s.state_root(), &tenant, &agent_id)?;
    fs::create_dir_all(&snapshots_dir_path)?;

    let id = SnapshotId::new();
    let encrypted = req.encrypt.is_some();
    let bundle_path =
        snapshot_bundle_path(s.state_root(), &tenant, &agent_id, id, encrypted)?;
    let staging_dir = snapshots_dir_path.join(format!(".staging-{}", id.as_filename()));
    fs::create_dir_all(&staging_dir)?;

    let result = build_bundle(
        s,
        &agent_id,
        &tenant,
        &req,
        id,
        &bundle_path,
        &staging_dir,
    )
    .await;

    // Best-effort cleanup whether we shipped a bundle or not.
    let _ = fs::remove_dir_all(&staging_dir);

    result
}

async fn build_bundle(
    s: &LocalFsSnapshotter,
    agent_id: &str,
    tenant: &str,
    req: &SnapshotRequest,
    id: SnapshotId,
    bundle_path: &Path,
    staging_dir: &Path,
) -> Result<SnapshotMeta, SnapshotError> {
    let encrypted = req.encrypt.is_some();
    let memdir = s.path_resolver().memdir(agent_id, tenant);
    let sqlite_dir = s.path_resolver().sqlite_dir(agent_id, tenant);

    let git_meta = read_head_meta_or_placeholder(&memdir);

    fs::create_dir_all(staging_dir.join("sqlite"))?;
    fs::create_dir_all(staging_dir.join("state"))?;

    let mut staged: Vec<StagedArtifact> = Vec::new();

    // SQLite backups — one VACUUM INTO per DB. A missing DB is treated
    // as "agent never wrote that table" and skipped: the manifest
    // simply reflects which artifacts the bundle actually carries.
    for (name, kind) in SQLITE_DBS {
        let src = sqlite_dir.join(format!("{name}.sqlite"));
        if !src.exists() {
            continue;
        }
        let (dst, _size) = backup_named(&src, &staging_dir.join("sqlite"), name).await?;
        staged.push(StagedArtifact {
            on_disk: dst,
            in_bundle: format!("sqlite/{name}.sqlite"),
            kind: *kind,
        });
    }

    // State provider — extractor cursor + dream-run row.
    let extract_cursor = s
        .state_provider()
        .capture_extract_cursor(&agent_id.to_string())
        .await?;
    if let Some(value) = extract_cursor {
        let path = staging_dir.join("state/extract_cursor.json");
        fs::write(&path, serde_json::to_vec_pretty(&value)?)?;
        staged.push(StagedArtifact {
            on_disk: path,
            in_bundle: "state/extract_cursor.json".into(),
            kind: ArtifactKind::StateExtractCursor,
        });
    }
    let dream_run = s
        .state_provider()
        .capture_last_dream_run(&agent_id.to_string())
        .await?;
    if let Some(value) = dream_run {
        let path = staging_dir.join("state/dream_run.json");
        fs::write(&path, serde_json::to_vec_pretty(&value)?)?;
        staged.push(StagedArtifact {
            on_disk: path,
            in_bundle: "state/dream_run.json".into(),
            kind: ArtifactKind::StateDreamRun,
        });
    }

    // Memdir contents (memory_files/<rel>) + git tree (git/<rel>).
    for (src, in_bundle) in enumerate_memdir_files(&memdir)? {
        staged.push(StagedArtifact {
            on_disk: src,
            in_bundle,
            kind: ArtifactKind::MemoryFile,
        });
    }
    for (src, in_bundle) in enumerate_git_files(&memdir)? {
        staged.push(StagedArtifact {
            on_disk: src,
            in_bundle,
            kind: ArtifactKind::GitBundle,
        });
    }

    // Optional redaction pass over text artifacts before per-artifact
    // hashing so the manifest reflects the redacted bytes that will
    // actually ship in the bundle.
    let redaction_report = if req.redact_secrets {
        let policy = DefaultRedactionPolicy::new();
        redact_staging_dir(staging_dir, &policy)?
    } else {
        None
    };

    // Per-artifact SHA-256.
    let mut artifacts = Vec::with_capacity(staged.len());
    for art in &staged {
        let bytes = fs::read(&art.on_disk)?;
        artifacts.push(ArtifactMeta {
            path_in_bundle: art.in_bundle.clone(),
            kind: art.kind,
            size_bytes: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
        });
    }

    // Seal the manifest. `bundle_sha256` here is the SHA-256 of the
    // concatenated per-artifact hex digests in declared order: it
    // commits to the artifact set without recursing on the tar bytes.
    // The whole-file hash of the resulting `.tar.zst` lives in a
    // sibling `.sha256` file written below.
    let mut concat = String::with_capacity(artifacts.len() * 64);
    for a in &artifacts {
        concat.push_str(&a.sha256);
    }
    let bundle_sha256 = sha256_hex(concat.as_bytes());

    let encryption_meta = build_encryption_meta(&req.encrypt)?;

    let manifest = Manifest {
        manifest_version: MANIFEST_VERSION,
        bundle_format: BUNDLE_FORMAT.into(),
        snapshot_id: id,
        agent_id: agent_id.to_string(),
        tenant: tenant.to_string(),
        label: req.label.clone(),
        created_at_ms: Utc::now().timestamp_millis(),
        created_by: req.created_by.clone(),
        schema_versions: SchemaVersions::CURRENT,
        git: git_meta,
        artifacts,
        redactions: redaction_report.clone(),
        encryption: encryption_meta,
        tool_versions: ToolVersions::current(),
        bundle_sha256,
    };

    // Write the manifest into staging so it ships as a tar entry.
    let manifest_path = staging_dir.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;

    let mut entries: Vec<PackEntry> = Vec::with_capacity(staged.len() + 1);
    entries.push(PackEntry {
        path_in_bundle: "manifest.json",
        source: &manifest_path,
    });
    for art in &staged {
        entries.push(PackEntry {
            path_in_bundle: &art.in_bundle,
            source: &art.on_disk,
        });
    }

    // Stream pack into `<bundle>.partial`, hashing the bytes that
    // actually land on disk (post-encryption) as we go.
    let partial_name = format!(
        "{}.partial",
        bundle_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("bundle")
    );
    let partial = bundle_path.with_file_name(partial_name);
    {
        let f = fs::File::create(&partial)?;
        let hashing = HashingWriter::new(f);
        let file_digest = pack_pipeline(&entries, hashing, &req.encrypt)?;
        fs::write(bundle_sha256_sibling(bundle_path), &file_digest)?;
    }
    fs::rename(&partial, bundle_path)?;

    let bundle_size_bytes = fs::metadata(bundle_path)?.len();

    Ok(SnapshotMeta {
        id,
        agent_id: agent_id.to_string(),
        tenant: tenant.to_string(),
        label: req.label.clone(),
        created_at_ms: manifest.created_at_ms,
        bundle_path: bundle_path.to_path_buf(),
        bundle_size_bytes,
        bundle_sha256: manifest.bundle_sha256.clone(),
        git_oid: Some(manifest.git.head_oid.clone()),
        schema_versions: SchemaVersions::CURRENT,
        encrypted,
        redactions_applied: redaction_report.is_some(),
    })
}

/// Build the manifest's `EncryptionMeta` block when an `EncryptionKey`
/// was supplied. Without the `snapshot-encryption` feature any non-None
/// key is rejected eagerly so an operator does not get a silently
/// unencrypted bundle.
fn build_encryption_meta(
    key: &Option<crate::request::EncryptionKey>,
) -> Result<Option<crate::manifest::EncryptionMeta>, SnapshotError> {
    let Some(key) = key else { return Ok(None) };
    match key {
        crate::request::EncryptionKey::AgePublicKey(s) => {
            #[cfg(feature = "snapshot-encryption")]
            {
                let recipient = crate::codec::age_codec::parse_recipient(s)?;
                Ok(Some(crate::manifest::EncryptionMeta {
                    scheme: "age".to_string(),
                    recipients_fingerprint: vec![
                        crate::codec::age_codec::fingerprint(&recipient),
                    ],
                }))
            }
            #[cfg(not(feature = "snapshot-encryption"))]
            {
                let _ = s;
                Err(SnapshotError::Encryption(
                    "AgePublicKey supplied but `snapshot-encryption` feature is disabled".into(),
                ))
            }
        }
    }
}

/// Drive `pack_files` through the configured pipeline:
/// `HashingWriter<File>` → optional `EncryptingWriter` → tar+zstd. The
/// writer order keeps the file-level SHA-256 over the bytes that land
/// on disk, which is the same shape verify recomputes.
fn pack_pipeline(
    entries: &[PackEntry<'_>],
    hashing: HashingWriter<fs::File>,
    encrypt: &Option<crate::request::EncryptionKey>,
) -> Result<String, SnapshotError> {
    if encrypt.is_none() {
        let hashing = pack_files(entries, hashing)
            .map_err(|e| SnapshotError::Io(std::io::Error::other(format!("pack: {e}"))))?;
        let (_inner, file_digest, _bytes) = hashing.finalize_hex();
        return Ok(file_digest);
    }

    #[cfg(feature = "snapshot-encryption")]
    {
        let crate::request::EncryptionKey::AgePublicKey(s) = encrypt.as_ref().unwrap();
        let recipient = crate::codec::age_codec::parse_recipient(s)?;
        let enc_writer = crate::codec::age_codec::encrypt_writer(hashing, vec![recipient])?;
        let enc_writer = pack_files(entries, enc_writer)
            .map_err(|e| SnapshotError::Io(std::io::Error::other(format!("pack: {e}"))))?;
        let hashing_back = enc_writer.finish()?;
        let (_inner, file_digest, _bytes) = hashing_back.finalize_hex();
        Ok(file_digest)
    }

    #[cfg(not(feature = "snapshot-encryption"))]
    Err(SnapshotError::Encryption(
        "encryption requested but `snapshot-encryption` feature is disabled".into(),
    ))
}

/// Read the memdir HEAD when the agent has a real memdir; otherwise
/// stamp a placeholder so the manifest stays well-formed for fresh
/// agents that haven't committed anything yet.
fn read_head_meta_or_placeholder(memdir: &Path) -> GitMeta {
    match read_head_meta(memdir) {
        Ok(m) => m,
        Err(_) => GitMeta {
            head_oid: "0".repeat(40),
            head_subject: "(no memdir)".into(),
            head_author: "nexo-memory-snapshot <ops@example.com>".into(),
            head_ts_ms: 0,
        },
    }
}

struct StagedArtifact {
    on_disk: PathBuf,
    in_bundle: String,
    kind: ArtifactKind,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshotter::MemorySnapshotter;
    use crate::tenant_path::snapshots_dir;
    use git2::{IndexAddOption, Repository, Signature};
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{ConnectOptions, Connection};
    use std::str::FromStr;

    async fn seed_sqlite(path: &Path, rows: i64) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let opts = SqliteConnectOptions::from_str(&format!("sqlite:{}", path.display()))
            .unwrap()
            .create_if_missing(true);
        let mut conn = opts.connect().await.unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
            .execute(&mut conn)
            .await
            .unwrap();
        for i in 0..rows {
            sqlx::query("INSERT INTO t (id, v) VALUES (?, ?)")
                .bind(i)
                .bind(format!("row-{i}"))
                .execute(&mut conn)
                .await
                .unwrap();
        }
        conn.close().await.unwrap();
    }

    fn seed_memdir(memdir: &Path) {
        fs::create_dir_all(memdir).unwrap();
        let repo = Repository::init(memdir).unwrap();
        fs::write(memdir.join("MEMORY.md"), b"# index\n- topic-a\n").unwrap();
        fs::write(memdir.join("topic-a.md"), b"# a\nseed\n").unwrap();
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
            "snapshot:seed",
            &tree,
            &[],
        )
        .unwrap();
    }

    fn build_snapshotter(state_root: &Path) -> LocalFsSnapshotter {
        LocalFsSnapshotter::builder()
            .state_root(state_root)
            .memdir_root(state_root.join("agents-memdir"))
            .sqlite_root(state_root.join("agents-sqlite"))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn happy_path_produces_bundle_and_sibling_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());

        let memdir = tmp.path().join("agents-memdir/ana");
        seed_memdir(&memdir);
        seed_sqlite(
            &tmp.path().join("agents-sqlite/ana/long_term.sqlite"),
            10,
        )
        .await;

        let req = SnapshotRequest::cli("ana", "default");
        let meta = s.snapshot(req).await.unwrap();

        assert!(meta.bundle_path.exists(), "{}", meta.bundle_path.display());
        assert!(meta.bundle_path.to_string_lossy().ends_with(".tar.zst"));
        assert!(meta.bundle_size_bytes > 0);
        assert_eq!(meta.bundle_sha256.len(), 64);
        assert!(meta.git_oid.is_some());

        // Sibling hash file exists and contains the whole-file SHA-256.
        let sib = bundle_sha256_sibling(&meta.bundle_path);
        let body = fs::read_to_string(&sib).unwrap();
        assert_eq!(body.trim().len(), 64);
    }

    #[tokio::test]
    async fn snapshot_path_lives_under_tenant_root() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("agents-memdir/ana"));

        let req = SnapshotRequest::cli("ana", "acme");
        let meta = s.snapshot(req).await.unwrap();

        let dir = snapshots_dir(tmp.path(), "acme", "ana").unwrap();
        assert!(meta.bundle_path.starts_with(&dir));
    }

    #[tokio::test]
    async fn second_snapshot_with_held_lock_returns_concurrent() {
        use crate::local_fs::lock::AgentLockMap;
        // Drive the lock primitive directly so the test doesn't race
        // against the snapshot fast path completing before the second
        // call queues. This is the same lock the snapshotter uses.
        let map = AgentLockMap::new();
        let agent: crate::id::AgentId = "ana".into();
        let _g = map
            .acquire(&agent, std::time::Duration::from_millis(50))
            .await
            .unwrap();
        let err = map
            .acquire(&agent, std::time::Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(err, SnapshotError::Concurrent(ref a) if a == &agent));
    }

    #[tokio::test]
    async fn rejects_invalid_tenant_id() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("agents-memdir/ana"));

        let mut req = SnapshotRequest::cli("ana", "default");
        req.tenant = "BAD-Tenant".into(); // uppercase rejected
        let err = s.snapshot(req).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("tenant") || msg.contains("[a-z0-9_-]"), "{msg}");
    }
}
