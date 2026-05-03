//! Phase 31.1.b — extract a sha-verified plugin tarball into the
//! daemon's plugin discovery directory.
//!
//! Pipeline:
//! 1. Idempotent re-install check (`<dest_root>/<id>-<version>/`
//!    already present + manifest matches expected).
//! 2. Cleanup of any stray `.staging-*` from prior crashed runs.
//! 3. Allocate a unique staging dir.
//! 4. Stream-extract the tarball (sync, behind `spawn_blocking`),
//!    enforcing per-entry path safety + entry-type whitelist +
//!    size budgets.
//! 5. Re-parse `nexo-plugin.toml` from staging and validate
//!    `id`/`version` match the resolver-advertised entry; reject
//!    on mismatch.
//! 6. Verify `bin/<id>` exists and is executable on Unix.
//! 7. Atomic-rename staging → final dir.
//!
//! The crate intentionally does NOT read configuration: the
//! caller resolves `dest_root` from
//! `plugins.discovery.search_paths[0]` and passes it in.

use std::fs;
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use nexo_ext_registry::ExtEntry;
use nexo_plugin_manifest::PluginManifest;
use rand::Rng;
use tar::{Archive, EntryType};

use crate::extract_error::ExtractError;

// ── Defaults exposed for `ExtractLimits` ───────────────────────────

/// Default cap on the compressed tarball file size.
pub const MAX_TARBALL_BYTES: u64 = 100 * 1024 * 1024;

/// Default cap on the number of entries (files + dirs) inside a tarball.
pub const MAX_ENTRIES: u64 = 10_000;

/// Default cap on the sum of all entry sizes after extraction.
pub const MAX_EXTRACTED_BYTES: u64 = 250 * 1024 * 1024;

/// Default cap on a single entry's header-declared size.
pub const MAX_ENTRY_BYTES: u64 = 100 * 1024 * 1024;

const STAGING_PREFIX: &str = ".staging-";
const MANIFEST_FILE: &str = "nexo-plugin.toml";
const BIN_DIR: &str = "bin";

/// Bounds applied during extraction. Defaults are suitable for
/// the plugin tarballs we expect (a single binary + manifest +
/// optional small assets).
#[derive(Debug, Clone)]
pub struct ExtractLimits {
    /// Maximum compressed tarball size.
    pub max_tarball_bytes: u64,
    /// Maximum number of entries.
    pub max_entries: u64,
    /// Maximum sum of all entry sizes after extraction.
    pub max_extracted_bytes: u64,
    /// Maximum header-declared size of any single entry.
    pub max_entry_bytes: u64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_tarball_bytes: MAX_TARBALL_BYTES,
            max_entries: MAX_ENTRIES,
            max_extracted_bytes: MAX_EXTRACTED_BYTES,
            max_entry_bytes: MAX_ENTRY_BYTES,
        }
    }
}

/// Inputs to [`extract_verified_tarball`].
#[derive(Debug)]
pub struct ExtractInput<'a> {
    /// Path to the verified `.tar.gz` on disk.
    pub tarball_path: &'a Path,
    /// Root dir under which `<plugin_id>-<version>/` will be created.
    pub dest_root: &'a Path,
    /// Plugin entry the resolver built. Used as the source of
    /// truth for expected id + version + binary name.
    pub expected: &'a ExtEntry,
    /// Limits applied during extraction.
    pub limits: ExtractLimits,
}

/// Successful extraction result.
#[derive(Debug, Clone)]
pub struct ExtractedPlugin {
    /// Final on-disk dir: `<dest_root>/<plugin_id>-<version>/`.
    pub plugin_dir: PathBuf,
    /// Manifest re-parsed from `<plugin_dir>/nexo-plugin.toml`.
    pub manifest: PluginManifest,
    /// Path to the executable: `<plugin_dir>/bin/<plugin_id>`.
    pub binary_path: PathBuf,
    /// `true` when the dir was already present (idempotent re-install).
    pub was_already_present: bool,
}

// ── Public API ─────────────────────────────────────────────────────

/// Extract a verified `.tar.gz` into `dest_root`, validating
/// content against `expected`.
pub async fn extract_verified_tarball(
    input: ExtractInput<'_>,
) -> Result<ExtractedPlugin, ExtractError> {
    let ExtractInput {
        tarball_path,
        dest_root,
        expected,
        limits,
    } = input;

    let final_dir = dest_root.join(format!("{}-{}", expected.id, expected.version));
    let binary_path = final_dir.join(BIN_DIR).join(&expected.id);

    // Idempotent re-install check.
    if final_dir.join(MANIFEST_FILE).exists() {
        let manifest = parse_manifest(&final_dir.join(MANIFEST_FILE))?;
        check_manifest_matches(&manifest, expected)?;
        return Ok(ExtractedPlugin {
            plugin_dir: final_dir,
            manifest,
            binary_path,
            was_already_present: true,
        });
    }

    // Ensure dest_root exists; cleanup stale staging dirs.
    fs::create_dir_all(dest_root).map_err(|e| ExtractError::Io(e.to_string()))?;
    cleanup_stale_staging(dest_root)?;

    // Allocate unique staging dir.
    let suffix: u64 = rand::thread_rng().gen();
    let staging_dir = dest_root.join(format!("{}{}-{:x}", STAGING_PREFIX, expected.id, suffix));
    fs::create_dir_all(&staging_dir).map_err(|e| ExtractError::Io(e.to_string()))?;

    // Run sync extract under spawn_blocking. Owned copies for the
    // closure; tarball + staging paths cloned to satisfy 'static.
    let extract_outcome = {
        let tarball = tarball_path.to_path_buf();
        let staging = staging_dir.clone();
        let limits = limits.clone();
        tokio::task::spawn_blocking(move || extract_to_staging(&tarball, &staging, &limits)).await
    };

    if let Err(e) = match extract_outcome {
        Ok(inner) => inner,
        Err(join) => Err(ExtractError::JoinError(join.to_string())),
    } {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(e);
    }

    // Re-parse manifest from staging and validate.
    let staging_manifest_path = staging_dir.join(MANIFEST_FILE);
    let manifest = match parse_manifest(&staging_manifest_path) {
        Ok(m) => m,
        Err(e) => {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(e);
        }
    };
    if let Err(e) = check_manifest_matches(&manifest, expected) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(e);
    }

    // Verify expected binary path.
    let staging_binary = staging_dir.join(BIN_DIR).join(&expected.id);
    if !staging_binary.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(ExtractError::BinaryMissing { path: binary_path });
    }
    chmod_executable(&staging_binary);

    // Atomic rename staging → final.
    if let Err(e) = fs::rename(&staging_dir, &final_dir) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(ExtractError::Io(format!(
            "rename staging → final failed: {}",
            e
        )));
    }

    Ok(ExtractedPlugin {
        plugin_dir: final_dir,
        manifest,
        binary_path,
        was_already_present: false,
    })
}

// ── Helpers ────────────────────────────────────────────────────────

fn validate_entry_path(p: &Path) -> Result<(), ExtractError> {
    let display = p.display().to_string();
    if p.is_absolute() {
        return Err(ExtractError::UnsafePath {
            path: display,
            reason: "entry path is absolute",
        });
    }
    for c in p.components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_str().ok_or(ExtractError::UnsafePath {
                    path: display.clone(),
                    reason: "entry component is not valid UTF-8",
                })?;
                if s.contains('\0') {
                    return Err(ExtractError::UnsafePath {
                        path: display,
                        reason: "entry component contains NUL byte",
                    });
                }
            }
            Component::ParentDir => {
                return Err(ExtractError::UnsafePath {
                    path: display,
                    reason: "entry contains parent component (`..`)",
                });
            }
            Component::RootDir => {
                return Err(ExtractError::UnsafePath {
                    path: display,
                    reason: "entry contains root separator",
                });
            }
            Component::Prefix(_) => {
                return Err(ExtractError::UnsafePath {
                    path: display,
                    reason: "entry contains windows path prefix",
                });
            }
            Component::CurDir => {}
        }
    }
    Ok(())
}

fn cleanup_stale_staging(dest_root: &Path) -> Result<(), ExtractError> {
    let read = match fs::read_dir(dest_root) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(ExtractError::Io(e.to_string())),
    };
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if name_str.starts_with(STAGING_PREFIX) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
    Ok(())
}

fn entry_kind_label(t: EntryType) -> &'static str {
    match t {
        EntryType::Symlink => "symlink",
        EntryType::Link => "hardlink",
        EntryType::Char => "character device",
        EntryType::Block => "block device",
        EntryType::Fifo => "fifo",
        EntryType::Continuous => "continuous",
        EntryType::GNULongName => "gnu long name",
        EntryType::GNULongLink => "gnu long link",
        EntryType::GNUSparse => "gnu sparse",
        EntryType::XGlobalHeader => "pax global header",
        EntryType::XHeader => "pax extended header",
        _ => "non-regular",
    }
}

fn extract_to_staging(
    tarball_path: &Path,
    staging_dir: &Path,
    limits: &ExtractLimits,
) -> Result<(), ExtractError> {
    let metadata = fs::metadata(tarball_path).map_err(|e| ExtractError::Io(e.to_string()))?;
    let size = metadata.len();
    if size > limits.max_tarball_bytes {
        return Err(ExtractError::TarballTooLarge {
            path: tarball_path.to_path_buf(),
            actual: size,
            limit: limits.max_tarball_bytes,
        });
    }

    let file = fs::File::open(tarball_path).map_err(|e| ExtractError::Io(e.to_string()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    archive.set_preserve_permissions(true);
    archive.set_overwrite(false);

    let mut entry_count: u64 = 0;
    let mut total_bytes: u64 = 0;

    let entries = archive
        .entries()
        .map_err(|e| ExtractError::Io(format!("read tar entries: {}", e)))?;

    for entry_result in entries {
        let mut entry =
            entry_result.map_err(|e| ExtractError::Io(format!("read tar entry: {}", e)))?;

        let header = entry.header().clone();
        let kind = header.entry_type();

        let path_owned = entry
            .path()
            .map_err(|e| ExtractError::Io(format!("read entry path: {}", e)))?
            .into_owned();
        let path_str = path_owned.display().to_string();

        match kind {
            EntryType::Regular | EntryType::Directory => {}
            other => {
                return Err(ExtractError::DisallowedEntryType {
                    path: path_str,
                    kind: entry_kind_label(other),
                });
            }
        }

        validate_entry_path(&path_owned)?;

        entry_count += 1;
        if entry_count > limits.max_entries {
            return Err(ExtractError::TooManyEntries {
                limit: limits.max_entries,
            });
        }

        let entry_size = header
            .size()
            .map_err(|e| ExtractError::Io(format!("read entry size: {}", e)))?;
        if entry_size > limits.max_entry_bytes {
            return Err(ExtractError::EntryTooLarge {
                path: path_str,
                actual: entry_size,
                limit: limits.max_entry_bytes,
            });
        }
        total_bytes = total_bytes.saturating_add(entry_size);
        if total_bytes > limits.max_extracted_bytes {
            return Err(ExtractError::ExtractedTooLarge {
                limit: limits.max_extracted_bytes,
            });
        }

        entry
            .unpack_in(staging_dir)
            .map_err(|e| ExtractError::Io(format!("unpack entry `{}`: {}", path_str, e)))?;
    }

    Ok(())
}

fn parse_manifest(path: &Path) -> Result<PluginManifest, ExtractError> {
    let body = fs::read_to_string(path).map_err(|e| ExtractError::ManifestInvalid {
        path: path.to_path_buf(),
        reason: format!("read failed: {}", e),
    })?;
    toml::from_str::<PluginManifest>(&body).map_err(|e| ExtractError::ManifestInvalid {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}

fn check_manifest_matches(
    actual: &PluginManifest,
    expected: &ExtEntry,
) -> Result<(), ExtractError> {
    if actual.plugin.id != expected.id || actual.plugin.version != expected.version {
        return Err(ExtractError::ManifestMismatch {
            expected_id: expected.id.clone(),
            expected_version: expected.version.clone(),
            got_id: actual.plugin.id.clone(),
            got_version: actual.plugin.version.clone(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn chmod_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    let mut perms = metadata.permissions();
    let mode = perms.mode();
    if mode & 0o111 == 0 {
        perms.set_mode((mode & 0o777) | 0o755);
        let _ = fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn chmod_executable(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_ext_registry::{ExtDownload, ExtTier};
    use semver::Version;
    use std::io::Write;
    use tempfile::TempDir;

    // ── Test fixtures ──────────────────────────────────────────────

    fn manifest_toml(id: &str, version: &str) -> String {
        format!(
            r#"
[plugin]
id = "{id}"
version = "{version}"
name = "{id}"
description = "test plugin"
min_nexo_version = ">=0.1.0"
"#
        )
    }

    enum FakeEntry<'a> {
        File(&'a str, &'a [u8]),
        Symlink(&'a str, &'a str),
    }

    fn make_test_tarball(entries: &[FakeEntry<'_>]) -> tempfile::NamedTempFile {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tar::{Builder, Header};

        let file = tempfile::NamedTempFile::new().unwrap();
        let encoder = GzEncoder::new(file.reopen().unwrap(), Compression::default());
        let mut builder = Builder::new(encoder);

        for e in entries {
            match e {
                FakeEntry::File(path, body) => {
                    let mut header = Header::new_gnu();
                    header.set_path(path).unwrap();
                    header.set_size(body.len() as u64);
                    header.set_mode(0o644);
                    header.set_entry_type(EntryType::Regular);
                    header.set_cksum();
                    builder.append(&header, &body[..]).unwrap();
                }
                FakeEntry::Symlink(path, target) => {
                    let mut header = Header::new_gnu();
                    header.set_size(0);
                    header.set_mode(0o777);
                    header.set_entry_type(EntryType::Symlink);
                    builder
                        .append_link(&mut header, path, std::path::Path::new(target))
                        .unwrap();
                }
            }
        }
        builder.into_inner().unwrap().finish().unwrap();
        file
    }

    fn build_happy_tarball(id: &str, version: &str) -> tempfile::NamedTempFile {
        let manifest = manifest_toml(id, version);
        let bin_path = format!("bin/{}", id);
        make_test_tarball(&[
            FakeEntry::File(MANIFEST_FILE, manifest.as_bytes()),
            FakeEntry::File(&bin_path, b"#!/bin/sh\necho hi\n"),
        ])
    }

    fn make_expected(id: &str, version: &str) -> ExtEntry {
        ExtEntry {
            id: id.to_string(),
            version: Version::parse(version).unwrap(),
            name: id.to_string(),
            description: "test".into(),
            homepage: "https://example.test".into(),
            tier: ExtTier::Community,
            min_nexo_version: semver::VersionReq::parse(">=0.1.0").unwrap(),
            downloads: vec![ExtDownload {
                target: "x86_64-unknown-linux-gnu".into(),
                url: "https://example.test/t.tar.gz".parse().unwrap(),
                sha256: "0".repeat(64),
                size_bytes: 1,
            }],
            manifest_url: "https://example.test/nexo-plugin.toml".into(),
            signing: None,
            authors: vec![],
        }
    }

    // ── Helper-level tests (step 4 + 5) ────────────────────────────

    #[test]
    fn validate_entry_path_accepts_normal_relative() {
        validate_entry_path(Path::new("bin/foo")).unwrap();
        validate_entry_path(Path::new("nexo-plugin.toml")).unwrap();
        validate_entry_path(Path::new("a/b/c")).unwrap();
    }

    #[test]
    fn validate_entry_path_rejects_parent_component() {
        let err = validate_entry_path(Path::new("../etc/passwd")).unwrap_err();
        assert!(matches!(err, ExtractError::UnsafePath { .. }));
    }

    #[test]
    fn validate_entry_path_rejects_absolute() {
        let err = validate_entry_path(Path::new("/etc/passwd")).unwrap_err();
        assert!(matches!(err, ExtractError::UnsafePath { .. }));
    }

    #[test]
    fn validate_entry_path_rejects_nested_parent() {
        let err = validate_entry_path(Path::new("bin/../../../escape")).unwrap_err();
        assert!(matches!(err, ExtractError::UnsafePath { .. }));
    }

    #[test]
    fn cleanup_stale_staging_removes_only_prefix() {
        let tmp = TempDir::new().unwrap();
        let stale = tmp.path().join(".staging-foo-deadbeef");
        fs::create_dir_all(&stale).unwrap();
        let mut f = fs::File::create(stale.join("junk")).unwrap();
        f.write_all(b"x").unwrap();
        let keep = tmp.path().join("real-plugin-1.0.0");
        fs::create_dir_all(&keep).unwrap();

        cleanup_stale_staging(tmp.path()).unwrap();

        assert!(!stale.exists(), "stale staging dir should be removed");
        assert!(keep.exists(), "non-staging dirs must be preserved");
    }

    // ── Public-API tests (step 7) ──────────────────────────────────

    #[tokio::test]
    async fn happy_path_extracts_and_returns_binary_path() {
        let tmp = TempDir::new().unwrap();
        let dest_root = tmp.path();
        let tarball = build_happy_tarball("slack", "0.2.0");
        let expected = make_expected("slack", "0.2.0");

        let result = extract_verified_tarball(ExtractInput {
            tarball_path: tarball.path(),
            dest_root,
            expected: &expected,
            limits: ExtractLimits::default(),
        })
        .await
        .expect("extract should succeed");

        assert!(!result.was_already_present);
        assert_eq!(result.plugin_dir, dest_root.join("slack-0.2.0"));
        assert_eq!(result.binary_path, dest_root.join("slack-0.2.0/bin/slack"));
        assert!(result.binary_path.exists());
        assert_eq!(result.manifest.plugin.id, "slack");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&result.binary_path).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "binary must be executable: {:o}", mode);
        }
    }

    #[tokio::test]
    async fn idempotent_skip_when_dir_exists() {
        let tmp = TempDir::new().unwrap();
        let dest_root = tmp.path();
        let tarball = build_happy_tarball("slack", "0.2.0");
        let expected = make_expected("slack", "0.2.0");

        // First call: real extract.
        let _ = extract_verified_tarball(ExtractInput {
            tarball_path: tarball.path(),
            dest_root,
            expected: &expected,
            limits: ExtractLimits::default(),
        })
        .await
        .unwrap();

        // Second call: must short-circuit.
        let result = extract_verified_tarball(ExtractInput {
            tarball_path: tarball.path(),
            dest_root,
            expected: &expected,
            limits: ExtractLimits::default(),
        })
        .await
        .unwrap();

        assert!(result.was_already_present);
        assert_eq!(result.manifest.plugin.id, "slack");
    }

    #[tokio::test]
    async fn mismatched_manifest_returns_error_and_cleans_staging() {
        let tmp = TempDir::new().unwrap();
        let dest_root = tmp.path();
        // Tarball claims id=evil, expected says slack.
        let tarball = build_happy_tarball("evil", "0.2.0");
        let expected = make_expected("slack", "0.2.0");

        let err = extract_verified_tarball(ExtractInput {
            tarball_path: tarball.path(),
            dest_root,
            expected: &expected,
            limits: ExtractLimits::default(),
        })
        .await
        .unwrap_err();

        assert!(matches!(err, ExtractError::ManifestMismatch { .. }));
        // No staging leftovers.
        let leftovers: Vec<_> = fs::read_dir(dest_root)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(STAGING_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "staging dirs must be cleaned up");
        // No final dir.
        assert!(!dest_root.join("slack-0.2.0").exists());
    }

    #[tokio::test]
    async fn path_traversal_via_dot_dot_rejected() {
        // `tar::Builder::set_path` rejects `..` at tarball-build
        // time, so we craft the offending header by hand against
        // the GNU header `name` field.
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tar::{Builder, Header};

        let tmp = TempDir::new().unwrap();
        let manifest = manifest_toml("slack", "0.2.0");

        let tarball = tempfile::NamedTempFile::new().unwrap();
        let encoder = GzEncoder::new(tarball.reopen().unwrap(), Compression::default());
        let mut builder = Builder::new(encoder);

        let mut h1 = Header::new_gnu();
        h1.set_path(MANIFEST_FILE).unwrap();
        h1.set_size(manifest.len() as u64);
        h1.set_mode(0o644);
        h1.set_entry_type(EntryType::Regular);
        h1.set_cksum();
        builder.append(&h1, manifest.as_bytes()).unwrap();

        let mut h2 = Header::new_gnu();
        h2.set_path("bin/slack").unwrap();
        h2.set_size(1);
        h2.set_mode(0o755);
        h2.set_entry_type(EntryType::Regular);
        h2.set_cksum();
        builder.append(&h2, &b"x"[..]).unwrap();

        // Raw `..` injection: write directly into the GNU name field.
        let evil = b"../../escape";
        let mut h3 = Header::new_gnu();
        h3.as_old_mut().name[..evil.len()].copy_from_slice(evil);
        h3.set_size(1);
        h3.set_mode(0o644);
        h3.set_entry_type(EntryType::Regular);
        h3.set_cksum();
        builder.append(&h3, &b"x"[..]).unwrap();

        builder.into_inner().unwrap().finish().unwrap();

        let expected = make_expected("slack", "0.2.0");
        let err = extract_verified_tarball(ExtractInput {
            tarball_path: tarball.path(),
            dest_root: tmp.path(),
            expected: &expected,
            limits: ExtractLimits::default(),
        })
        .await
        .unwrap_err();

        assert!(matches!(err, ExtractError::UnsafePath { .. }), "got {:?}", err);
    }

    #[tokio::test]
    async fn absolute_path_rejected() {
        let tmp = TempDir::new().unwrap();
        let manifest = manifest_toml("slack", "0.2.0");
        // tar's set_path normalizes absolute paths, so we must build
        // a header by hand.
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tar::{Builder, Header};

        let tarball = tempfile::NamedTempFile::new().unwrap();
        let encoder = GzEncoder::new(tarball.reopen().unwrap(), Compression::default());
        let mut builder = Builder::new(encoder);

        let mut h1 = Header::new_gnu();
        h1.set_path(MANIFEST_FILE).unwrap();
        h1.set_size(manifest.len() as u64);
        h1.set_mode(0o644);
        h1.set_entry_type(EntryType::Regular);
        h1.set_cksum();
        builder.append(&h1, manifest.as_bytes()).unwrap();

        let mut h2 = Header::new_gnu();
        h2.set_path("bin/slack").unwrap();
        h2.set_size(8);
        h2.set_mode(0o755);
        h2.set_entry_type(EntryType::Regular);
        h2.set_cksum();
        builder.append(&h2, &b"#!/bin/sh"[..8]).unwrap();

        // Manually craft an absolute-path header. `set_path` strips
        // the leading `/`; bypass via direct name field.
        let mut h3 = Header::new_gnu();
        h3.as_old_mut().name[..11].copy_from_slice(b"/etc/passwd");
        h3.set_size(1);
        h3.set_mode(0o644);
        h3.set_entry_type(EntryType::Regular);
        h3.set_cksum();
        builder.append(&h3, &b"x"[..]).unwrap();

        builder.into_inner().unwrap().finish().unwrap();

        let expected = make_expected("slack", "0.2.0");
        let err = extract_verified_tarball(ExtractInput {
            tarball_path: tarball.path(),
            dest_root: tmp.path(),
            expected: &expected,
            limits: ExtractLimits::default(),
        })
        .await
        .unwrap_err();

        assert!(matches!(err, ExtractError::UnsafePath { .. }), "got {:?}", err);
    }

    #[tokio::test]
    async fn symlink_entry_rejected() {
        let tmp = TempDir::new().unwrap();
        let manifest = manifest_toml("slack", "0.2.0");
        let tarball = make_test_tarball(&[
            FakeEntry::File(MANIFEST_FILE, manifest.as_bytes()),
            FakeEntry::File("bin/slack", b"x"),
            FakeEntry::Symlink("link-out", "/etc/passwd"),
        ]);
        let expected = make_expected("slack", "0.2.0");

        let err = extract_verified_tarball(ExtractInput {
            tarball_path: tarball.path(),
            dest_root: tmp.path(),
            expected: &expected,
            limits: ExtractLimits::default(),
        })
        .await
        .unwrap_err();

        assert!(
            matches!(err, ExtractError::DisallowedEntryType { kind: "symlink", .. }),
            "got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn entry_count_limit_enforced() {
        let tmp = TempDir::new().unwrap();
        let manifest = manifest_toml("slack", "0.2.0");
        let mut entries = vec![
            FakeEntry::File(MANIFEST_FILE, manifest.as_bytes()),
            FakeEntry::File("bin/slack", b"x"),
        ];
        let extras = ["a", "b", "c", "d", "e", "f"];
        for path in &extras {
            entries.push(FakeEntry::File(path, b"x"));
        }
        let tarball = make_test_tarball(&entries);
        let expected = make_expected("slack", "0.2.0");

        let err = extract_verified_tarball(ExtractInput {
            tarball_path: tarball.path(),
            dest_root: tmp.path(),
            expected: &expected,
            limits: ExtractLimits {
                max_entries: 5,
                ..ExtractLimits::default()
            },
        })
        .await
        .unwrap_err();

        assert!(matches!(err, ExtractError::TooManyEntries { limit: 5 }), "got {:?}", err);
    }

    #[tokio::test]
    async fn binary_missing_after_extract() {
        let tmp = TempDir::new().unwrap();
        let manifest = manifest_toml("slack", "0.2.0");
        // Manifest only — no bin/slack.
        let tarball = make_test_tarball(&[FakeEntry::File(MANIFEST_FILE, manifest.as_bytes())]);
        let expected = make_expected("slack", "0.2.0");

        let err = extract_verified_tarball(ExtractInput {
            tarball_path: tarball.path(),
            dest_root: tmp.path(),
            expected: &expected,
            limits: ExtractLimits::default(),
        })
        .await
        .unwrap_err();

        assert!(matches!(err, ExtractError::BinaryMissing { .. }), "got {:?}", err);
        // Staging cleaned up.
        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(STAGING_PREFIX))
            .collect();
        assert!(leftovers.is_empty());
    }
}
