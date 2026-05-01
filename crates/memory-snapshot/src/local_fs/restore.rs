//! `restore()` body for [`super::LocalFsSnapshotter`].
//!
//! Flow:
//!
//! 1. Validate `(tenant, agent_id)` and verify the bundle (catches
//!    schema-too-new and checksum mismatch up front).
//! 2. When `auto_pre_snapshot` is on (default), snapshot the live
//!    state with label `auto:pre-restore-<orig_id>` so the operation
//!    is reversible.
//! 3. `dry_run = true` → emit a [`RestoreReport`] describing what
//!    would change and exit without mutating disk.
//! 4. Acquire the per-agent lock, unpack the bundle into a staging
//!    dir, then atomically swap each SQLite DB, copy the `.git/`
//!    tree, restore memory files, and replay the state-provider
//!    artifacts.
//! 5. Drop the staging dir and the lock.

use std::fs;
use std::path::{Path, PathBuf};

use crate::codec::tar_zst::unpack_into;
use crate::error::SnapshotError;
use crate::git_capture::tag_pre_restore;
use crate::manifest::{ArtifactKind, Manifest, SchemaVersions};
use crate::meta::RestoreReport;
use crate::request::{RestoreRequest, SnapshotRequest};
#[cfg(feature = "snapshot-encryption")]
use crate::request::DecryptionIdentity;
use crate::snapshotter::MemorySnapshotter;
use crate::tenant_path::{snapshots_dir, validate_agent_id, validate_tenant};

use super::snapshotter::LocalFsSnapshotter;

const SQLITE_DBS: &[(&str, ArtifactKind)] = &[
    ("long_term", ArtifactKind::SqliteLongTerm),
    ("vector", ArtifactKind::SqliteVector),
    ("concepts", ArtifactKind::SqliteConcepts),
    ("compactions", ArtifactKind::SqliteCompactions),
];

pub(super) async fn run_restore(
    s: &LocalFsSnapshotter,
    req: RestoreRequest,
) -> Result<RestoreReport, SnapshotError> {
    let agent_id = validate_agent_id(&req.agent_id)?.to_string();
    let tenant = validate_tenant(&req.tenant)?.to_string();

    if !req.bundle.exists() {
        return Err(SnapshotError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("bundle not found: {}", req.bundle.display()),
        )));
    }

    // Up-front verify catches schema-too-new + checksum mismatch
    // before we ever touch the live agent state.
    let report = super::verify::run_verify(&req.bundle).await?;
    if !report.manifest_ok {
        return Err(SnapshotError::ChecksumMismatch);
    }
    if !report.schema_versions.is_supported_by(&SchemaVersions::CURRENT) {
        return Err(SnapshotError::SchemaTooNew {
            bundle: report.schema_versions.manifest,
            runtime: SchemaVersions::CURRENT.manifest,
        });
    }

    if report.age_protected {
        // The decryption path runs as part of `apply_restore` below;
        // fall through after confirming the operator gave us the
        // identity to use.
        if req.decrypt.is_none() {
            return Err(SnapshotError::Encryption(
                "encrypted bundle: --decrypt-identity required".into(),
            ));
        }
        #[cfg(not(feature = "snapshot-encryption"))]
        {
            return Err(SnapshotError::Encryption(
                "encrypted bundle: rebuild with `snapshot-encryption` feature".into(),
            ));
        }
    }

    let pre_snapshot_id = if req.auto_pre_snapshot && !req.dry_run {
        match s
            .snapshot(SnapshotRequest {
                agent_id: agent_id.clone(),
                tenant: tenant.clone(),
                label: Some(label_pre_restore_for(&req.bundle)),
                redact_secrets: false,
                encrypt: None,
                created_by: "auto-pre-restore".into(),
            })
            .await
        {
            Ok(meta) => Some(meta.id),
            Err(SnapshotError::UnknownAgent(_)) => None,
            // A fresh agent with no live state cannot pre-snapshot;
            // surface as a refusal so the operator decides explicitly
            // whether to proceed (e.g. via `--no-auto-pre-snapshot`).
            Err(e) => {
                return Err(SnapshotError::RestoreRefused(format!(
                    "auto-pre-snapshot failed: {e}"
                )));
            }
        }
    } else {
        None
    };

    // Acquire the per-agent lock for the duration of disk mutation.
    let _lock = s.locks().acquire(&agent_id, s.lock_timeout()).await?;

    let tenant_dir = snapshots_dir(s.state_root(), &tenant, &agent_id)?;
    fs::create_dir_all(&tenant_dir)?;
    let staging = tenant_dir.join(format!(".restore-staging-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&staging)?;

    let result = apply_restore(s, &agent_id, &req, &staging, pre_snapshot_id).await;
    let _ = fs::remove_dir_all(&staging);
    result
}

/// Decrypt + unpack flow for `*.tar.zst.age` bundles. Reads the
/// identity file, runs the decryptor, and pipes the plaintext into
/// `unpack_into`. Without the `snapshot-encryption` feature this is
/// only reachable from a code path that already errored out, so the
/// `cfg(not(...))` arm exists purely to keep the type checker happy.
fn unpack_encrypted(
    src: fs::File,
    staging: &Path,
    decrypt: &Option<crate::request::DecryptionIdentity>,
) -> Result<(), SnapshotError> {
    #[cfg(feature = "snapshot-encryption")]
    {
        let Some(DecryptionIdentity::AgeIdentityFile(path)) = decrypt else {
            return Err(SnapshotError::Encryption(
                "encrypted bundle: identity file required".into(),
            ));
        };
        let ids = crate::codec::age_codec::load_identities(path)?;
        let reader = crate::codec::age_codec::decrypt_reader(src, &ids)?;
        // `decrypt_reader` returns `Box<dyn Read>` which is not `Seek`;
        // buffer through a temp file so `tar::Archive` (which wants
        // `Seek` for some metadata operations) can rewind.
        let tmp = tempfile::tempfile()
            .map_err(|e| SnapshotError::Io(std::io::Error::other(format!("tmpfile: {e}"))))?;
        let mut tmp = tmp;
        let mut reader = reader;
        std::io::copy(&mut reader, &mut tmp)?;
        use std::io::{Seek, SeekFrom};
        tmp.seek(SeekFrom::Start(0))?;
        unpack_into(tmp, staging)?;
        Ok(())
    }
    #[cfg(not(feature = "snapshot-encryption"))]
    {
        let _ = (src, staging, decrypt);
        Err(SnapshotError::Encryption(
            "encrypted bundle: rebuild with `snapshot-encryption` feature".into(),
        ))
    }
}

fn label_pre_restore_for(bundle: &Path) -> String {
    let stem = bundle
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.split('.').next())
        .unwrap_or("unknown");
    format!("auto:pre-restore-{stem}")
}

async fn apply_restore(
    s: &LocalFsSnapshotter,
    agent_id: &str,
    req: &RestoreRequest,
    staging: &Path,
    pre_snapshot_id: Option<crate::id::SnapshotId>,
) -> Result<RestoreReport, SnapshotError> {
    let encrypted = req
        .bundle
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.eq_ignore_ascii_case("age"))
        .unwrap_or(false);

    let f = fs::File::open(&req.bundle)?;
    if encrypted {
        unpack_encrypted(f, staging, &req.decrypt)?;
    } else {
        unpack_into(f, staging)?;
    }

    let manifest_path = staging.join("manifest.json");
    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;

    if manifest.agent_id != agent_id {
        return Err(SnapshotError::RestoreRefused(format!(
            "bundle agent_id `{}` does not match restore target `{}`",
            manifest.agent_id, agent_id
        )));
    }

    let memdir = s.memdir_root().join(agent_id);
    let sqlite_dir = s.sqlite_root().join(agent_id);

    // Plan: which artifacts will move where. Used both to populate
    // the dry-run report and to drive the real swap.
    let plan = build_plan(&manifest, staging, &sqlite_dir);

    if req.dry_run {
        return Ok(RestoreReport {
            agent_id: agent_id.to_string(),
            from: manifest.snapshot_id,
            pre_snapshot: pre_snapshot_id,
            git_reset_oid: Some(manifest.git.head_oid.clone()),
            sqlite_restored_dbs: plan.sqlite_targets.iter().map(|(n, _)| n.clone()).collect(),
            state_files_restored: plan.state_targets.iter().map(|(k, _)| k.clone()).collect(),
            workers_restarted: false,
            dry_run: true,
        });
    }

    // Tag the existing HEAD so the prior state stays reachable via
    // reflog. Best-effort: a fresh agent with no memdir simply has
    // nothing to tag.
    let mut git_reset_oid: Option<String> = None;
    if memdir.join(".git").exists() {
        let tag = format!("pre-restore-{}", manifest.snapshot_id.as_filename());
        if let Err(e) = tag_pre_restore(&memdir, &tag) {
            tracing::warn!(
                memdir = %memdir.display(),
                tag = %tag,
                error = %e,
                "pre-restore tag failed; restore proceeds without reflog anchor"
            );
        }
    }

    // SQLite swap: each staged DB replaces the live one via
    // `<live>.pre-restore.bak` + atomic rename so the move is
    // recoverable up to the moment the staging file overwrites the
    // live path.
    let mut sqlite_restored = Vec::new();
    for (name, src) in &plan.sqlite_targets {
        if let Some(parent) = sqlite_dir.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::create_dir_all(&sqlite_dir)?;
        let live = sqlite_dir.join(format!("{name}.sqlite"));
        if live.exists() {
            let bak = sqlite_dir.join(format!("{name}.sqlite.pre-restore.bak"));
            let _ = fs::remove_file(&bak);
            fs::rename(&live, &bak)?;
        }
        fs::copy(src, &live)?;
        sqlite_restored.push(name.clone());
        if let Some(oid) = manifest.git.head_oid.split_whitespace().next() {
            git_reset_oid.get_or_insert(oid.to_string());
        }
    }

    // Memdir restore: wipe the live `.git/` and any tracked markdown
    // files, then unpack staging copies on top. Skip if the bundle
    // shipped no memdir artifacts (older snapshots of fresh agents).
    if !plan.memdir_artifacts.is_empty() {
        let memdir_backup = backup_memdir(&memdir, manifest.snapshot_id)?;
        if let Err(e) = restore_memdir(&memdir, staging, &plan) {
            // Best-effort rollback: move backup back over partial state.
            if let Some(bak) = memdir_backup {
                let _ = fs::remove_dir_all(&memdir);
                let _ = fs::rename(&bak, &memdir);
            }
            return Err(e);
        }
        if let Some(oid) = manifest.git.head_oid.split_whitespace().next() {
            git_reset_oid.get_or_insert(oid.to_string());
        }
    }

    // State provider replay — extractor cursor + dream-run row.
    let mut state_restored = Vec::new();
    for (key, path) in &plan.state_targets {
        let bytes = fs::read(path)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        match key.as_str() {
            "extract_cursor" => {
                s.state_provider()
                    .restore_extract_cursor(&agent_id.to_string(), value)
                    .await?;
                state_restored.push(key.clone());
            }
            "dream_run" => {
                s.state_provider()
                    .restore_dream_run(&agent_id.to_string(), value)
                    .await?;
                state_restored.push(key.clone());
            }
            _ => {}
        }
    }

    Ok(RestoreReport {
        agent_id: agent_id.to_string(),
        from: manifest.snapshot_id,
        pre_snapshot: pre_snapshot_id,
        git_reset_oid,
        sqlite_restored_dbs: sqlite_restored,
        state_files_restored: state_restored,
        workers_restarted: true,
        dry_run: false,
    })
}

#[derive(Default)]
struct RestorePlan {
    /// `(db-name, source-path)` pairs to swap into `<sqlite_root>/<agent_id>/`.
    sqlite_targets: Vec<(String, PathBuf)>,
    /// `(state-key, source-path)` for `state_provider` replay.
    state_targets: Vec<(String, PathBuf)>,
    /// Every memdir artifact (memory_files/* and git/*).
    memdir_artifacts: Vec<MemdirArtifact>,
}

struct MemdirArtifact {
    in_bundle: String,
    on_disk: PathBuf,
    is_git: bool,
}

fn build_plan(manifest: &Manifest, staging: &Path, _sqlite_dir: &Path) -> RestorePlan {
    let mut plan = RestorePlan::default();
    for (db_name, kind) in SQLITE_DBS {
        if let Some(art) = manifest
            .artifacts
            .iter()
            .find(|a| a.kind == *kind && a.path_in_bundle == format!("sqlite/{db_name}.sqlite"))
        {
            plan.sqlite_targets.push((
                (*db_name).to_string(),
                staging.join(&art.path_in_bundle),
            ));
        }
    }
    for art in &manifest.artifacts {
        match art.kind {
            ArtifactKind::StateExtractCursor => {
                plan.state_targets.push((
                    "extract_cursor".into(),
                    staging.join(&art.path_in_bundle),
                ));
            }
            ArtifactKind::StateDreamRun => {
                plan.state_targets.push((
                    "dream_run".into(),
                    staging.join(&art.path_in_bundle),
                ));
            }
            ArtifactKind::MemoryFile | ArtifactKind::GitBundle => {
                plan.memdir_artifacts.push(MemdirArtifact {
                    in_bundle: art.path_in_bundle.clone(),
                    on_disk: staging.join(&art.path_in_bundle),
                    is_git: matches!(art.kind, ArtifactKind::GitBundle),
                });
            }
            _ => {}
        }
    }
    plan
}

fn backup_memdir(
    memdir: &Path,
    snapshot_id: crate::id::SnapshotId,
) -> Result<Option<PathBuf>, SnapshotError> {
    if !memdir.exists() {
        return Ok(None);
    }
    let bak = memdir.with_file_name(format!(
        "{}-pre-restore-{}",
        memdir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("memdir"),
        snapshot_id.as_filename()
    ));
    fs::rename(memdir, &bak)?;
    Ok(Some(bak))
}

fn restore_memdir(
    memdir: &Path,
    _staging: &Path,
    plan: &RestorePlan,
) -> Result<(), SnapshotError> {
    fs::create_dir_all(memdir)?;
    for art in &plan.memdir_artifacts {
        let rel = if art.is_git {
            art.in_bundle
                .strip_prefix("git/")
                .unwrap_or(&art.in_bundle)
        } else {
            art.in_bundle
                .strip_prefix("memory_files/")
                .unwrap_or(&art.in_bundle)
        };
        let dst = if art.is_git {
            memdir.join(".git").join(rel)
        } else {
            memdir.join(rel)
        };
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&art.on_disk, &dst)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::SnapshotRequest;
    use crate::snapshotter::MemorySnapshotter;
    use git2::{IndexAddOption, Repository, Signature};
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{ConnectOptions, Connection};
    use std::str::FromStr;

    async fn seed_sqlite(path: &Path, rows: i64, marker: &str) {
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
                .bind(format!("{marker}-{i}"))
                .execute(&mut conn)
                .await
                .unwrap();
        }
        conn.close().await.unwrap();
    }

    async fn read_marker(db: &Path) -> String {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=ro", db.display()))
            .unwrap();
        let mut conn = opts.connect().await.unwrap();
        let v: String = sqlx::query_scalar("SELECT v FROM t WHERE id = 0")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        conn.close().await.unwrap();
        v
    }

    fn seed_memdir(memdir: &Path, body: &[u8]) {
        fs::create_dir_all(memdir).unwrap();
        let repo = Repository::init(memdir).unwrap();
        fs::write(memdir.join("MEMORY.md"), body).unwrap();
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
    async fn dry_run_does_not_mutate_live_state() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"), b"# v1\n");
        seed_sqlite(
            &tmp.path().join("sqlite/ana/long_term.sqlite"),
            3,
            "v1",
        )
        .await;

        let m = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();

        // Mutate the live state so we can detect a real restore.
        fs::remove_file(tmp.path().join("sqlite/ana/long_term.sqlite")).unwrap();
        seed_sqlite(
            &tmp.path().join("sqlite/ana/long_term.sqlite"),
            3,
            "v2",
        )
        .await;
        fs::write(tmp.path().join("memdir/ana/MEMORY.md"), b"# v2\n").unwrap();

        let mut req = RestoreRequest::new("ana", "default", &m.bundle_path);
        req.dry_run = true;
        req.auto_pre_snapshot = false;
        let report = s.restore(req).await.unwrap();

        assert!(report.dry_run);
        assert_eq!(report.from, m.id);
        assert!(!report.workers_restarted);
        assert!(!report.sqlite_restored_dbs.is_empty());
        // Live state is still v2.
        assert_eq!(
            read_marker(&tmp.path().join("sqlite/ana/long_term.sqlite")).await,
            "v2-0"
        );
        let live = fs::read_to_string(tmp.path().join("memdir/ana/MEMORY.md")).unwrap();
        assert_eq!(live, "# v2\n");
    }

    #[tokio::test]
    async fn happy_path_round_trip_recovers_sqlite_and_memdir() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"), b"# v1\n");
        seed_sqlite(
            &tmp.path().join("sqlite/ana/long_term.sqlite"),
            3,
            "v1",
        )
        .await;

        let m = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();

        // Mutate live state.
        fs::remove_file(tmp.path().join("sqlite/ana/long_term.sqlite")).unwrap();
        seed_sqlite(
            &tmp.path().join("sqlite/ana/long_term.sqlite"),
            3,
            "v2",
        )
        .await;
        fs::write(tmp.path().join("memdir/ana/MEMORY.md"), b"# v2\n").unwrap();

        let mut req = RestoreRequest::new("ana", "default", &m.bundle_path);
        // Skip auto-pre-snapshot so the test is deterministic — the
        // pre-snapshot path itself is exercised below.
        req.auto_pre_snapshot = false;
        let report = s.restore(req).await.unwrap();

        assert!(!report.dry_run);
        assert!(report.workers_restarted);
        assert!(report.sqlite_restored_dbs.contains(&"long_term".to_string()));
        // SQLite restored.
        assert_eq!(
            read_marker(&tmp.path().join("sqlite/ana/long_term.sqlite")).await,
            "v1-0"
        );
        // Memdir markdown restored.
        let live = fs::read_to_string(tmp.path().join("memdir/ana/MEMORY.md")).unwrap();
        assert_eq!(live, "# v1\n");
        // The pre-restore SQLite backup is preserved.
        assert!(tmp
            .path()
            .join("sqlite/ana/long_term.sqlite.pre-restore.bak")
            .exists());
    }

    #[tokio::test]
    async fn auto_pre_snapshot_creates_reversible_anchor() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"), b"# v1\n");
        seed_sqlite(
            &tmp.path().join("sqlite/ana/long_term.sqlite"),
            3,
            "v1",
        )
        .await;

        let m = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();
        // Live state mutates between snapshot and restore.
        fs::remove_file(tmp.path().join("sqlite/ana/long_term.sqlite")).unwrap();
        seed_sqlite(
            &tmp.path().join("sqlite/ana/long_term.sqlite"),
            3,
            "v2",
        )
        .await;

        let report = s
            .restore(RestoreRequest::new("ana", "default", &m.bundle_path))
            .await
            .unwrap();
        assert!(report.pre_snapshot.is_some(), "auto-pre-snapshot must run");

        // The pre-restore snapshot's label must reference the source bundle.
        let metas = s.list(&"ana".into(), "default").await.unwrap();
        let pre = metas
            .iter()
            .find(|x| x.id == report.pre_snapshot.unwrap())
            .unwrap();
        assert!(pre
            .label
            .as_deref()
            .unwrap_or("")
            .starts_with("auto:pre-restore-"));
    }

    #[tokio::test]
    async fn rejects_bundle_with_mismatched_agent_id() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"), b"# v1\n");
        let m = s
            .snapshot(SnapshotRequest::cli("ana", "default"))
            .await
            .unwrap();

        let mut req = RestoreRequest::new("otro", "default", &m.bundle_path);
        req.auto_pre_snapshot = false;
        let err = s.restore(req).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("agent_id") || msg.contains("does not match"));
    }

    #[cfg(feature = "snapshot-encryption")]
    #[tokio::test]
    async fn encrypted_round_trip_recovers_state() {
        use crate::request::{DecryptionIdentity, EncryptionKey};
        use age::secrecy::ExposeSecret;

        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        seed_memdir(&tmp.path().join("memdir/ana"), b"# v1\n");
        seed_sqlite(
            &tmp.path().join("sqlite/ana/long_term.sqlite"),
            3,
            "v1",
        )
        .await;

        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public().to_string();
        let identity_path = tmp.path().join("identity.txt");
        std::fs::write(&identity_path, identity.to_string().expose_secret()).unwrap();

        let mut snap_req = SnapshotRequest::cli("ana", "default");
        snap_req.encrypt = Some(EncryptionKey::AgePublicKey(recipient));
        let m = s.snapshot(snap_req).await.unwrap();
        assert!(m.encrypted);
        assert!(m.bundle_path.to_string_lossy().ends_with(".tar.zst.age"));

        // Mutate live state so the restore is observable.
        std::fs::remove_file(tmp.path().join("sqlite/ana/long_term.sqlite")).unwrap();
        seed_sqlite(
            &tmp.path().join("sqlite/ana/long_term.sqlite"),
            3,
            "v2",
        )
        .await;

        let mut req = RestoreRequest::new("ana", "default", &m.bundle_path);
        req.auto_pre_snapshot = false;
        req.decrypt = Some(DecryptionIdentity::AgeIdentityFile(identity_path));
        let report = s.restore(req).await.unwrap();
        assert!(!report.dry_run);
        assert!(report.workers_restarted);
        assert_eq!(
            read_marker(&tmp.path().join("sqlite/ana/long_term.sqlite")).await,
            "v1-0"
        );
    }

    #[tokio::test]
    async fn missing_bundle_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let s = build_snapshotter(tmp.path());
        let err = s
            .restore(RestoreRequest::new(
                "ana",
                "default",
                tmp.path().join("ghost.tar.zst"),
            ))
            .await
            .unwrap_err();
        match err {
            SnapshotError::Io(io) => assert_eq!(io.kind(), std::io::ErrorKind::NotFound),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
