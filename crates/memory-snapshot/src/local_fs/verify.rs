//! `verify()` body for [`super::LocalFsSnapshotter`].
//!
//! Verification is a stand-alone read of a bundle on disk: it does not
//! require the snapshotter's lock map, only the codec primitives.
//! Three independent checks, all of which must pass for a `Ok` report:
//!
//! 1. **Whole-bundle SHA-256** — recompute the byte hash of the bundle
//!    file and compare it against the sibling `<bundle>.sha256` text
//!    file written at snapshot time.
//! 2. **Manifest integrity** — open the bundle, locate `manifest.json`,
//!    and confirm `bundle_sha256` equals SHA-256 over the concatenated
//!    per-artifact digests in the order the manifest declares them.
//! 3. **Per-artifact SHA-256** — recompute the digest of each tar
//!    entry and compare against the matching manifest row.
//!
//! Schema check: the manifest is rejected with
//! [`SnapshotError::SchemaTooNew`] when any field of `schema_versions`
//! is newer than what this runtime understands.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use crate::codec::sha256_stream::sha256_hex;
use crate::error::SnapshotError;
use crate::manifest::{Manifest, SchemaVersions};
use crate::meta::VerifyReport;
use crate::tenant_path::bundle_sha256_sibling;

const AGE_EXTENSION: &str = "age";
const MANIFEST_ENTRY: &str = "manifest.json";

pub(super) async fn run_verify(bundle: &Path) -> Result<VerifyReport, SnapshotError> {
    if !bundle.exists() {
        return Err(SnapshotError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("bundle not found: {}", bundle.display()),
        )));
    }

    let age_protected = bundle
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.eq_ignore_ascii_case(AGE_EXTENSION))
        .unwrap_or(false);

    if age_protected {
        // Threat model for age-wrapped bundles, until the decryption
        // layer is wired in:
        //
        // - The sibling `<bundle>.sha256` covers the encrypted bytes,
        //   so a transit/storage flip is still detectable here.
        // - The manifest seal (per-artifact SHA-256) lives **inside**
        //   the body and cannot be checked without the identity.
        //   `manifest_ok` and `per_artifact_ok` are therefore
        //   reported as `true` by convention — the report's
        //   `age_protected` flag tells the operator that those
        //   booleans were not actually exercised.
        // - Operators who must verify the manifest of an encrypted
        //   bundle should run `nexo memory verify` after a real
        //   decrypt + restore round-trip.
        //
        // The operator-facing version of this contract lives in
        // `docs/src/ops/memory-snapshot.md`.
        let bundle_sha256_ok = check_sibling_sha256(bundle)?;
        return Ok(VerifyReport {
            bundle: bundle.to_path_buf(),
            manifest_ok: true,
            bundle_sha256_ok,
            per_artifact_ok: true,
            schema_versions: SchemaVersions::CURRENT,
            age_protected,
        });
    }

    let bundle_sha256_ok = check_sibling_sha256(bundle)?;

    let (manifest, per_artifact_ok) = inspect_bundle(bundle)?;
    if !manifest.schema_versions.is_supported_by(&SchemaVersions::CURRENT) {
        return Err(SnapshotError::SchemaTooNew {
            bundle: manifest.schema_versions.manifest,
            runtime: SchemaVersions::CURRENT.manifest,
        });
    }

    let manifest_ok = recompute_manifest_seal(&manifest);

    Ok(VerifyReport {
        bundle: bundle.to_path_buf(),
        manifest_ok,
        bundle_sha256_ok,
        per_artifact_ok,
        schema_versions: manifest.schema_versions,
        age_protected,
    })
}

/// Recompute SHA-256 of the bundle file and compare with the value
/// stored at snapshot time in the sibling `.sha256` file. A missing
/// sibling means the bundle was hand-edited or copied without the
/// hash; report it as a failed check rather than a hard error so the
/// VerifyReport remains usable for diagnosis.
fn check_sibling_sha256(bundle: &Path) -> Result<bool, SnapshotError> {
    let sib = bundle_sha256_sibling(bundle);
    if !sib.exists() {
        return Ok(false);
    }
    let expected = fs::read_to_string(&sib)?.trim().to_lowercase();
    let bytes = fs::read(bundle)?;
    let actual = sha256_hex(&bytes);
    Ok(actual == expected)
}

/// Walk the tar entries, validate per-artifact digests, and parse the
/// manifest. Returns `(manifest, per_artifact_ok)`.
fn inspect_bundle(bundle: &Path) -> Result<(Manifest, bool), SnapshotError> {
    let f = fs::File::open(bundle)?;
    let dec = zstd::stream::Decoder::new(f)?;
    let mut tar = tar::Archive::new(dec);

    let mut manifest: Option<Manifest> = None;
    let mut entry_digests: HashMap<String, String> = HashMap::new();

    for entry in tar.entries()? {
        let mut entry = entry?;
        let path_in_bundle = entry.path()?.into_owned();
        let key = path_in_bundle.to_string_lossy().into_owned();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;

        if key == MANIFEST_ENTRY {
            let parsed: Manifest = serde_json::from_slice(&buf)?;
            manifest = Some(parsed);
            // The manifest itself is excluded from the per-artifact set
            // — its `bundle_sha256` covers the artifact list.
            continue;
        }
        entry_digests.insert(key, sha256_hex(&buf));
    }

    let manifest = manifest.ok_or_else(|| SnapshotError::MissingArtifact(MANIFEST_ENTRY.into()))?;

    let mut per_artifact_ok = true;
    for art in &manifest.artifacts {
        match entry_digests.get(&art.path_in_bundle) {
            Some(seen) if seen == &art.sha256 => {}
            Some(_) => per_artifact_ok = false,
            None => {
                return Err(SnapshotError::MissingArtifact(art.path_in_bundle.clone()));
            }
        }
    }

    Ok((manifest, per_artifact_ok))
}

/// Recompute the manifest's `bundle_sha256` (= SHA-256 of the
/// concatenated per-artifact hex digests in declared order) and
/// compare against the stored value.
fn recompute_manifest_seal(m: &Manifest) -> bool {
    let mut concat = String::with_capacity(m.artifacts.len() * 64);
    for a in &m.artifacts {
        concat.push_str(&a.sha256);
    }
    sha256_hex(concat.as_bytes()) == m.bundle_sha256
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::AgentId;
    use crate::request::SnapshotRequest;
    use crate::snapshotter::MemorySnapshotter;
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

    async fn make_bundle(tmp: &Path) -> std::path::PathBuf {
        let s = super::super::LocalFsSnapshotter::builder()
            .state_root(tmp)
            .memdir_root(tmp.join("agents-memdir"))
            .sqlite_root(tmp.join("agents-sqlite"))
            .build()
            .unwrap();
        seed_memdir(&tmp.join("agents-memdir/ana"));
        seed_sqlite(&tmp.join("agents-sqlite/ana/long_term.sqlite"), 4).await;
        let meta = s.snapshot(SnapshotRequest::cli("ana", "default")).await.unwrap();
        meta.bundle_path
    }

    #[tokio::test]
    async fn verify_clean_bundle_reports_all_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_bundle(tmp.path()).await;

        let s = super::super::LocalFsSnapshotter::builder()
            .state_root(tmp.path())
            .build()
            .unwrap();
        let report = s.verify(&bundle).await.unwrap();
        assert!(report.manifest_ok);
        assert!(report.bundle_sha256_ok);
        assert!(report.per_artifact_ok);
        assert!(!report.age_protected);
    }

    #[tokio::test]
    async fn verify_detects_bit_flip_in_bundle_body() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_bundle(tmp.path()).await;

        // Flip a single byte inside the bundle so the file-level hash
        // diverges. The sibling .sha256 still records the original.
        let mut bytes = fs::read(&bundle).unwrap();
        // Pick a byte well past zstd magic bytes to avoid breaking decompression.
        let idx = bytes.len() / 2;
        bytes[idx] ^= 0x01;
        fs::write(&bundle, &bytes).unwrap();

        let s = super::super::LocalFsSnapshotter::builder()
            .state_root(tmp.path())
            .build()
            .unwrap();
        let report = s.verify(&bundle).await.ok();
        // Bit-flip can either be caught by the file-level hash (most
        // likely) or surface as a manifest/artifact decode error.
        // Both outcomes count as detection.
        match report {
            Some(r) => {
                assert!(
                    !r.bundle_sha256_ok || !r.manifest_ok || !r.per_artifact_ok,
                    "verify should not accept a bit-flipped bundle"
                );
            }
            None => {} // decode failure also counts as detection
        }
    }

    #[tokio::test]
    async fn verify_reports_missing_sibling_hash_as_failed_check() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_bundle(tmp.path()).await;
        fs::remove_file(bundle_sha256_sibling(&bundle)).unwrap();

        let s = super::super::LocalFsSnapshotter::builder()
            .state_root(tmp.path())
            .build()
            .unwrap();
        let report = s.verify(&bundle).await.unwrap();
        assert!(!report.bundle_sha256_ok);
        // Manifest seal still checks out — only the whole-file hash
        // is missing.
        assert!(report.manifest_ok);
    }

    #[tokio::test]
    async fn verify_returns_not_found_when_bundle_missing() {
        let s = super::super::LocalFsSnapshotter::builder()
            .state_root("/nonexistent")
            .build()
            .unwrap();
        let err = s
            .verify(Path::new("/tmp/never-was-a-snapshot.tar.zst"))
            .await
            .unwrap_err();
        match err {
            SnapshotError::Io(io) => assert_eq!(io.kind(), std::io::ErrorKind::NotFound),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_recognizes_age_extension_without_decrypting() {
        // Create a tiny "fake" age-wrapped path: just rename a real
        // bundle to add `.age`. Verify should mark it age_protected
        // and still check the sibling hash via the new filename.
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_bundle(tmp.path()).await;
        let age_path = bundle.with_extension("zst.age");
        fs::rename(&bundle, &age_path).unwrap();
        // Move the sibling alongside.
        let old_sib = bundle_sha256_sibling(&bundle);
        let new_sib = bundle_sha256_sibling(&age_path);
        // Recompute against the renamed file's bytes — same content,
        // different name; the sibling must reflect the bytes we are
        // about to verify.
        let bytes = fs::read(&age_path).unwrap();
        fs::write(&new_sib, sha256_hex(&bytes)).unwrap();
        let _ = fs::remove_file(&old_sib);

        let s = super::super::LocalFsSnapshotter::builder()
            .state_root(tmp.path())
            .build()
            .unwrap();
        let report = s.verify(&age_path).await.unwrap();
        assert!(report.age_protected);
        assert!(report.bundle_sha256_ok);
        // Manifest + artifact checks intentionally skipped while
        // encrypted — the body cannot be opened without the identity.
        assert!(report.manifest_ok);
        assert!(report.per_artifact_ok);
    }

    // Silence clippy about the dummy import staying alive while only
    // tests reference it.
    #[allow(dead_code)]
    fn _agent_id_alias_marker(_: AgentId) {}
}
