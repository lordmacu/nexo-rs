//! Phase 31.1 — error variants returned by the installer.

/// Errors surfaced by [`crate::resolve_release`],
/// [`crate::download_and_verify`], and the
/// [`crate::install_plugin`] one-shot.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// `<owner>/<repo>[@<tag>]` coords malformed.
    #[error("plugin coords `{got}` invalid: {reason}")]
    CoordsInvalid {
        /// The offending coords string.
        got: String,
        /// Why parsing failed.
        reason: &'static str,
    },

    /// HTTP request failed (network, non-2xx, decode).
    #[error("install http error: {0}")]
    Http(String),

    /// IO write to the destination tarball path failed.
    #[error("install io error: {0}")]
    Io(String),

    /// GitHub release JSON didn't match our convention (missing
    /// `nexo-plugin.toml` asset, malformed manifest, tag isn't
    /// semver-shaped).
    #[error("release `{owner}/{repo}` shape error: {reason}")]
    ReleaseShape {
        /// Repo owner.
        owner: String,
        /// Repo name.
        reason: String,
        /// Reason the release didn't fit the convention.
        repo: String,
    },

    /// Release found, but no tarball asset matched the
    /// requested target. Listing the available targets so the
    /// operator can pick one or override `NEXO_INSTALL_TARGET`.
    #[error(
        "release for `{id}@{version}` has no tarball for target `{target}`. \
         Available: {available:?}"
    )]
    TargetNotFound {
        /// Plugin id (from the release's manifest asset).
        id: String,
        /// Resolved version (semver).
        version: semver::Version,
        /// Target triple the daemon requested.
        target: String,
        /// Tarball asset names actually present in the release.
        available: Vec<String>,
    },

    /// `.sha256` asset's body wasn't 64 lowercase hex chars.
    #[error(
        "release for `{id}` `.sha256` asset body `{got}` invalid: must be \
         exactly 64 hex chars"
    )]
    Sha256Invalid {
        /// Plugin id.
        id: String,
        /// Body read from the .sha256 asset.
        got: String,
    },

    /// Computed sha256 of the downloaded tarball doesn't match
    /// the value advertised in the `.sha256` asset.
    #[error(
        "sha256 mismatch for `{id}`: expected `{expected}`, got `{got}`. \
         Tarball was tampered or the .sha256 asset is stale."
    )]
    Sha256Mismatch {
        /// Plugin id.
        id: String,
        /// SHA256 hex from the .sha256 asset.
        expected: String,
        /// SHA256 hex computed from the downloaded bytes.
        got: String,
    },
}
