//! Parameterized extraction contract — lets the resolve+download
//! pipeline serve any source-of-truth manifest, not just
//! `nexo-plugin.toml`. The same crate powers both `nexo plugin install`
//! and `nexo persona install` without two
//! parallel copies of the GitHub Releases plumbing.
//!
//! # Why a trait, not an enum
//!
//! The persona pack ships a v2 manifest (different filename,
//! different schema) and a different in-process consumer.
//! Encoding every source-of-truth shape into a closed enum
//! would mean every new artifact kind requires editing this
//! crate. A trait keeps the abstraction open: persona-installer
//! adds its own `PersonaExtractContract` in its own crate.
//!
//! # Surface
//!
//! - [`ExtractContract`] — declares manifest filename + parser
//!   + id extraction. Three methods; everything else (semver
//!   from tag, tarball naming, sha256, cosign) is universal
//!   and stays in the resolver.
//! - [`PluginExtractContract`] — the existing
//!   `nexo-plugin.toml` behavior reified as an impl. Used
//!   automatically by [`crate::resolve_release`] so legacy
//!   callers see no behavior change.

use crate::error::InstallError;

/// Parameterizes the manifest-locating + parsing step of a
/// GitHub Releases resolve. Everything else (semver from
/// release tag, `<id>-<version>-<target>.tar.gz` asset shape,
/// `.sha256` sibling lookup, optional cosign material) is
/// shared across all contracts and stays in the resolver.
pub trait ExtractContract {
    /// Typed manifest exposed to the caller after parsing.
    /// Callers can inspect this directly to build their own
    /// entry type (e.g. `ExtEntry` for plugins, a future
    /// `PersonaEntry` for personas).
    type Manifest;

    /// Filename of the manifest asset within the GitHub
    /// release. The resolver locates the asset by exact name
    /// match; convention is a stable filename so the asset URL
    /// is discoverable without the resolver knowing the
    /// version.
    fn manifest_asset_name(&self) -> &'static str;

    /// Parse the manifest bytes into the typed manifest. Errors
    /// must map to [`InstallError::ReleaseShape`] so the CLI
    /// presents a release-side problem consistently. UTF-8
    /// validation is the contract's responsibility (different
    /// manifest formats might allow different encodings).
    fn parse_manifest(
        &self,
        bytes: &[u8],
        coords: &crate::RepoCoords,
    ) -> Result<Self::Manifest, InstallError>;

    /// Extract the package id from the parsed manifest. The
    /// resolver uses this to build the tarball asset name
    /// `<id>-<version>-<target>.tar.gz` and the noarch fallback
    /// `<id>-<version>-noarch.tar.gz`. Must match the id
    /// chosen by the release publisher when uploading assets.
    fn manifest_id(&self, manifest: &Self::Manifest) -> String;
}

/// Plugin-flavoured contract — the original behavior reified.
/// Manifest filename is `nexo-plugin.toml`, parsed into the
/// `nexo_plugin_manifest::PluginManifest` type, id pulled from
/// the `[plugin] id = "..."` key.
///
/// Stateless; cheap to construct on every resolve call.
#[derive(Debug, Default, Clone, Copy)]
pub struct PluginExtractContract;

impl ExtractContract for PluginExtractContract {
    type Manifest = nexo_plugin_manifest::PluginManifest;

    fn manifest_asset_name(&self) -> &'static str {
        "nexo-plugin.toml"
    }

    fn parse_manifest(
        &self,
        bytes: &[u8],
        coords: &crate::RepoCoords,
    ) -> Result<Self::Manifest, InstallError> {
        let text = std::str::from_utf8(bytes).map_err(|e| InstallError::ReleaseShape {
            owner: coords.owner.clone(),
            repo: coords.repo.clone(),
            reason: format!("manifest is not valid UTF-8: {e}"),
        })?;
        toml::from_str::<nexo_plugin_manifest::PluginManifest>(text).map_err(|e| {
            InstallError::ReleaseShape {
                owner: coords.owner.clone(),
                repo: coords.repo.clone(),
                reason: format!("manifest parse failed: {e}"),
            }
        })
    }

    fn manifest_id(&self, manifest: &Self::Manifest) -> String {
        manifest.plugin.id.clone()
    }
}
