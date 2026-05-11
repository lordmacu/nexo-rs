//! End-to-end install pipeline: resolve → validate → download
//! → verify → extract → return [`InstalledPersona`]. Wraps
//! [`nexo_ext_installer`]'s generic resolver via
//! [`crate::PersonaExtractContract`] so persona-installer
//! reuses ~60% of plugin-installer's plumbing.
//!
//! Tarball extraction is local to this crate (rather than
//! reusing [`nexo_ext_installer::extract_verified_tarball`])
//! because the plugin extractor expects a `bin/<id>` binary
//! + `nexo-plugin.toml` re-parse — neither applies to
//! personas. Safety limits mirror the ext-installer's
//! defaults so a malicious persona pack can't OOM the
//! daemon.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use nexo_ext_installer::{download_and_verify_url, resolve_release_with_contract, RepoCoords};
use nexo_persona_manifest::PersonaManifest;

use crate::contract::PersonaExtractContract;
use crate::error::PersonaInstallError;

/// Maximum number of entries the tarball may contain. Mirrors
/// [`nexo_ext_installer::MAX_ENTRIES`] for parity.
pub const MAX_ENTRIES: usize = 8_192;
/// Maximum on-disk size of any single extracted entry.
pub const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB
/// Maximum total uncompressed size of the extracted pack.
pub const MAX_EXTRACTED_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB
/// Maximum compressed tarball size we'll accept on disk.
pub const MAX_TARBALL_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

/// Outcome of a successful install. Carries enough info for
/// the admin layer to publish a lifecycle event + register
/// the persona in the boot-time discovery pipeline.
///
/// Internal struct — no serde derive (would require pushing
/// derives onto upstream `RepoCoords` + `PersonaManifest`).
/// The wire type that crosses admin RPC is
/// [`crate::PersonaListEntry`], not this.
#[derive(Debug, Clone)]
pub struct InstalledPersona {
    /// Persona id (echoed from manifest).
    pub id: String,
    /// Installed semver version.
    pub version: semver::Version,
    /// Absolute path of the install directory:
    /// `<state_root>/personas/<id>-<version>/`.
    pub install_root: PathBuf,
    /// Coords the install was sourced from
    /// (`<owner>/<repo>@<tag>`). Echoed for audit / lifecycle.
    pub coords: RepoCoords,
    /// Wall-clock timestamp when the install completed.
    pub installed_at: DateTime<Utc>,
    /// Re-parsed manifest from the on-disk `persona.toml`
    /// after extraction. Round-trip guarantees the on-disk
    /// copy matches what was downloaded; contract callers can
    /// inspect this directly.
    pub manifest: PersonaManifest,
    /// Bytes of the verified tarball (pre-extraction). Useful
    /// for telemetry + sanity checks.
    pub tarball_bytes: u64,
    /// `true` when the install dir was already present at the
    /// requested version (idempotent re-install). Lets the
    /// CLI / admin layer skip re-publishing a lifecycle
    /// `Installed` event.
    pub was_already_present: bool,
}

/// Inputs to [`install_persona`]. Bundled into a struct so
/// future knobs (custom user-agent, alternate hash algo, etc.)
/// land without breaking the call sites.
#[derive(Debug, Clone)]
pub struct InstallInputs<'a> {
    /// Resolver client. Caller owns connection pooling +
    /// proxy config.
    pub client: &'a reqwest::Client,
    /// Coords parsed from the operator's `<owner>/<repo>[@<tag>]`
    /// CLI argument.
    pub coords: &'a RepoCoords,
    /// Daemon's target triple — passed to the resolver for
    /// per-target asset matching. Persona packs typically
    /// publish `noarch` only (no native binaries) so this is
    /// usually a hint that hits the noarch fallback.
    pub target: &'a str,
    /// Absolute root path under which `<id>-<version>/` will
    /// be created. Typically `<state_root>/personas/`.
    pub install_root: &'a Path,
    /// GitHub Releases API base URL. Use
    /// [`nexo_ext_installer::DEFAULT_GITHUB_API_BASE`]
    /// outside of tests.
    pub api_base: &'a str,
}

/// Run the end-to-end install pipeline.
///
/// Steps:
/// 1. Reject relative `install_root` (loud-fail, not silent
///    canonicalization to CWD).
/// 2. Resolve via [`resolve_release_with_contract`] +
///    [`PersonaExtractContract`].
/// 3. Run the manifest validator (id regex, semver, paths).
/// 4. Idempotency check: if
///    `<install_root>/<id>-<version>/persona.toml` already
///    exists, return [`InstalledPersona`] with
///    `was_already_present = true` (no re-download).
/// 5. Download tarball to a temp path under `install_root`.
/// 6. Extract into the final install dir with safety limits.
/// 7. Re-parse the on-disk `persona.toml` to confirm round-
///    trip; fail loudly if the on-disk copy doesn't validate.
pub async fn install_persona(
    inputs: InstallInputs<'_>,
) -> Result<InstalledPersona, PersonaInstallError> {
    let InstallInputs {
        client,
        coords,
        target,
        install_root,
        api_base,
    } = inputs;

    if !install_root.is_absolute() {
        return Err(PersonaInstallError::InstallRootNotAbsolute {
            got: install_root.display().to_string(),
        });
    }

    let resolved =
        resolve_release_with_contract(&PersonaExtractContract, client, coords, target, api_base)
            .await?;

    nexo_persona_manifest::validate(&resolved.manifest)?;

    let id = resolved.manifest.persona.id.clone();
    let version = resolved.version.clone();
    let final_dir = install_root.join(format!("{id}-{version}"));
    let on_disk_manifest_path = final_dir.join("persona.toml");

    // Idempotent re-install — the install dir already holds a
    // persona.toml that matches the requested version, so we
    // skip the download/extract entirely + return the cached
    // manifest.
    if tokio::fs::try_exists(&on_disk_manifest_path)
        .await
        .map_err(|e| PersonaInstallError::Io {
            op: "read",
            path: on_disk_manifest_path.display().to_string(),
            reason: e.to_string(),
        })?
    {
        let cached = read_and_validate_on_disk_manifest(&on_disk_manifest_path).await?;
        return Ok(InstalledPersona {
            id: cached.persona.id.clone(),
            version: version.clone(),
            install_root: final_dir,
            coords: coords.clone(),
            installed_at: Utc::now(),
            manifest: cached,
            tarball_bytes: 0,
            was_already_present: true,
        });
    }

    // Ensure parent + create staging tarball path. Staging
    // file lives in install_root (NOT in /tmp) so a renamed
    // mount or cross-fs-link footgun doesn't bite during the
    // final move.
    tokio::fs::create_dir_all(install_root)
        .await
        .map_err(|e| PersonaInstallError::Io {
            op: "mkdir",
            path: install_root.display().to_string(),
            reason: e.to_string(),
        })?;
    let staging_tarball = install_root.join(format!(".{id}-{version}.tar.gz.partial"));

    let tarball_bytes = download_and_verify_url(
        client,
        &resolved.tarball_url,
        &resolved.sha256_url,
        &id,
        &staging_tarball,
    )
    .await?;

    if tarball_bytes > MAX_TARBALL_BYTES {
        let _ = tokio::fs::remove_file(&staging_tarball).await;
        return Err(PersonaInstallError::Extract {
            persona_id: id.clone(),
            reason: format!(
                "verified tarball is {tarball_bytes} bytes, exceeds MAX_TARBALL_BYTES \
                 ({MAX_TARBALL_BYTES})"
            ),
        });
    }

    // Extract under a staging dir, then atomic-rename to the
    // final dir to avoid partially-extracted state being
    // visible to a concurrent boot-time discovery scan.
    let staging_dir = install_root.join(format!(".{id}-{version}.staging"));
    if tokio::fs::try_exists(&staging_dir).await.unwrap_or(false) {
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
    }
    tokio::fs::create_dir_all(&staging_dir)
        .await
        .map_err(|e| PersonaInstallError::Io {
            op: "mkdir",
            path: staging_dir.display().to_string(),
            reason: e.to_string(),
        })?;

    let staging_dir_for_extract = staging_dir.clone();
    let staging_tarball_for_extract = staging_tarball.clone();
    let id_for_extract = id.clone();
    let extract_res = tokio::task::spawn_blocking(move || {
        extract_persona_tarball(
            &staging_tarball_for_extract,
            &staging_dir_for_extract,
            &id_for_extract,
        )
    })
    .await
    .map_err(|e| PersonaInstallError::Extract {
        persona_id: id.clone(),
        reason: format!("extraction task panicked: {e}"),
    })?;
    extract_res?;

    // Move staging → final dir. Cleanup staging tarball after.
    if let Err(e) = tokio::fs::rename(&staging_dir, &final_dir).await {
        // Cleanup partial state.
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        let _ = tokio::fs::remove_file(&staging_tarball).await;
        return Err(PersonaInstallError::Io {
            op: "rename",
            path: final_dir.display().to_string(),
            reason: e.to_string(),
        });
    }
    let _ = tokio::fs::remove_file(&staging_tarball).await;

    // Re-parse the on-disk persona.toml to confirm the pack
    // shipped a valid manifest (defense in depth — the
    // tarball may legally lack a top-level persona.toml even
    // when the release publishes one as an asset).
    let on_disk = read_and_validate_on_disk_manifest(&on_disk_manifest_path).await?;

    Ok(InstalledPersona {
        id,
        version,
        install_root: final_dir,
        coords: coords.clone(),
        installed_at: Utc::now(),
        manifest: on_disk,
        tarball_bytes,
        was_already_present: false,
    })
}

async fn read_and_validate_on_disk_manifest(
    path: &Path,
) -> Result<PersonaManifest, PersonaInstallError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| PersonaInstallError::Io {
            op: "read",
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
    let text = std::str::from_utf8(&bytes).map_err(|e| PersonaInstallError::Io {
        op: "read",
        path: path.display().to_string(),
        reason: format!("not valid UTF-8: {e}"),
    })?;
    let manifest = nexo_persona_manifest::parse_str(text)?;
    nexo_persona_manifest::validate(&manifest)?;
    Ok(manifest)
}

/// Sync extraction. Runs inside `spawn_blocking` because the
/// `tar` crate's API is sync. Safety limits enforced per
/// entry; any violation aborts + leaves the staging dir in a
/// half-extracted state for the caller to clean up.
fn extract_persona_tarball(
    tarball_path: &Path,
    dest_dir: &Path,
    persona_id_for_errors: &str,
) -> Result<(), PersonaInstallError> {
    let file = std::fs::File::open(tarball_path).map_err(|e| PersonaInstallError::Io {
        op: "read",
        path: tarball_path.display().to_string(),
        reason: e.to_string(),
    })?;
    let gz = GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);

    let mut entry_count: usize = 0;
    let mut total_bytes: u64 = 0;

    let entries = archive
        .entries()
        .map_err(|e| PersonaInstallError::Extract {
            persona_id: persona_id_for_errors.to_string(),
            reason: format!("open tar entries iterator: {e}"),
        })?;

    for entry_res in entries {
        let mut entry = entry_res.map_err(|e| PersonaInstallError::Extract {
            persona_id: persona_id_for_errors.to_string(),
            reason: format!("read tar entry: {e}"),
        })?;

        entry_count += 1;
        if entry_count > MAX_ENTRIES {
            return Err(PersonaInstallError::Extract {
                persona_id: persona_id_for_errors.to_string(),
                reason: format!("entry count exceeds MAX_ENTRIES ({MAX_ENTRIES})"),
            });
        }

        let header_size = entry
            .header()
            .size()
            .map_err(|e| PersonaInstallError::Extract {
                persona_id: persona_id_for_errors.to_string(),
                reason: format!("read entry size from header: {e}"),
            })?;
        if header_size > MAX_ENTRY_BYTES {
            return Err(PersonaInstallError::Extract {
                persona_id: persona_id_for_errors.to_string(),
                reason: format!(
                    "entry size {header_size} exceeds MAX_ENTRY_BYTES ({MAX_ENTRY_BYTES})"
                ),
            });
        }
        total_bytes = total_bytes.saturating_add(header_size);
        if total_bytes > MAX_EXTRACTED_BYTES {
            return Err(PersonaInstallError::Extract {
                persona_id: persona_id_for_errors.to_string(),
                reason: format!(
                    "cumulative extraction exceeds MAX_EXTRACTED_BYTES \
                     ({MAX_EXTRACTED_BYTES})"
                ),
            });
        }

        // Manual path-safety check: reject entries whose
        // header path is absolute or contains `..`. The `tar`
        // crate's `unpack` does *some* of this but we don't
        // fully trust the crate's defaults across versions.
        let path_in_tar = entry.path().map_err(|e| PersonaInstallError::Extract {
            persona_id: persona_id_for_errors.to_string(),
            reason: format!("read entry path: {e}"),
        })?;
        if path_in_tar.is_absolute() {
            return Err(PersonaInstallError::Extract {
                persona_id: persona_id_for_errors.to_string(),
                reason: format!(
                    "tar entry path `{}` is absolute; rejected for safety",
                    path_in_tar.display()
                ),
            });
        }
        for component in path_in_tar.components() {
            if matches!(component, std::path::Component::ParentDir) {
                return Err(PersonaInstallError::Extract {
                    persona_id: persona_id_for_errors.to_string(),
                    reason: format!(
                        "tar entry path `{}` contains `..`; rejected for safety",
                        path_in_tar.display()
                    ),
                });
            }
        }

        // Unpack into dest_dir. The `tar` crate joins the
        // entry path safely under dest_dir; we've already
        // ruled out absolute + parent-traversal above, so
        // this lands inside dest_dir.
        entry
            .unpack_in(dest_dir)
            .map_err(|e| PersonaInstallError::Extract {
                persona_id: persona_id_for_errors.to_string(),
                reason: format!("unpack entry: {e}"),
            })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Validates `extract_persona_tarball` rejects an entry
    /// path that escapes the dest dir via `..`. We build a
    /// minimal in-memory tar with a single bad entry and
    /// confirm the typed error variant fires.
    ///
    /// Note: `tar::Builder::append_data` has a writer-side
    /// guard that refuses `..` in the entry name (defense in
    /// depth). To exercise our extractor's reader-side check,
    /// we bypass by writing the entry via raw
    /// `Builder::append(&header, data)` after manually
    /// stuffing the header's name field with the malicious
    /// path (header.name is bytes 0..100 of the 512-byte tar
    /// block).
    #[test]
    fn extract_rejects_parent_traversal_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let tar_path = tmp.path().join("malicious.tar.gz");
        let dest_dir = tmp.path().join("out");
        std::fs::create_dir_all(&dest_dir).unwrap();

        let file = std::fs::File::create(&tar_path).unwrap();
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(gz);

        let payload = b"escape data";
        let mut header = tar::Header::new_old();
        // Stuff the name field directly — bypasses
        // append_data's `..` validator.
        let bad_name = b"../escape.txt";
        header.as_old_mut().name[..bad_name.len()].copy_from_slice(bad_name);
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append(&header, &payload[..]).unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        let result = extract_persona_tarball(&tar_path, &dest_dir, "cody");
        match result {
            Err(PersonaInstallError::Extract { persona_id, reason }) => {
                assert_eq!(persona_id, "cody");
                assert!(
                    reason.contains(".."),
                    "error must mention `..`, got: {reason}"
                );
            }
            other => panic!("expected Extract error, got {other:?}"),
        }
    }
}
