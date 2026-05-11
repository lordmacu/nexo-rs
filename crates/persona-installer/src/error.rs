//! Typed errors raised by the persona installer pipeline.
//! Wraps [`nexo_ext_installer::InstallError`] for the
//! resolve+download stages and adds persona-specific
//! variants for manifest validation, extraction, and on-disk
//! state.

use thiserror::Error;

/// All recoverable failures raised by `nexo persona install`
/// orchestration. `non_exhaustive` so future variants don't
/// break downstream `match`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PersonaInstallError {
    /// GitHub Releases resolve / download / sha256 verify
    /// failure (anything from `nexo-ext-installer`).
    #[error("ext-installer: {0}")]
    Ext(#[from] nexo_ext_installer::InstallError),

    /// v2 persona manifest parse / validate failure
    /// (anything from `nexo-persona-manifest`).
    #[error("persona manifest: {0}")]
    Manifest(#[from] nexo_persona_manifest::PersonaManifestError),

    /// Tarball extraction failure (untar I/O, entry-count
    /// limit, total-size limit, path-traversal in entry
    /// name). `persona_id` echoes which install raised it for
    /// CLI context.
    #[error("persona `{persona_id}` extraction failed: {reason}")]
    Extract {
        /// Persona id from the manifest (or coords if the
        /// manifest didn't parse yet).
        persona_id: String,
        /// One-line failure description.
        reason: String,
    },

    /// On-disk state (mkdir / write / read) failure. Echoes
    /// the operation + path for actionable CLI output.
    #[error("persona installer state I/O failed during `{op}` on `{path}`: {reason}")]
    Io {
        /// Operation that failed (`mkdir`, `write`, `read`,
        /// `remove`, `read_dir`).
        op: &'static str,
        /// Path involved.
        path: String,
        /// Underlying I/O error message.
        reason: String,
    },

    /// `extract_root` (the on-disk install location) is not
    /// an absolute path. The installer refuses to install
    /// under a relative path so persona installs are
    /// reproducible regardless of the daemon's CWD.
    #[error("persona install root `{got}` must be an absolute path")]
    InstallRootNotAbsolute {
        /// Path as supplied.
        got: String,
    },

    /// Persona is already installed at the same version. The
    /// orchestrator surfaces this as a typed variant so the
    /// CLI can present a clean "already installed" message
    /// (with the install timestamp) instead of treating it
    /// as a hard error.
    #[error("persona `{id}` v{version} already installed at `{install_root}`")]
    AlreadyInstalled {
        /// Persona id.
        id: String,
        /// Installed version.
        version: String,
        /// On-disk install root.
        install_root: String,
    },

    /// Caller asked to remove a persona id that has no
    /// matching install dir.
    #[error("persona `{id}` is not installed under `{state_root}`")]
    NotFound {
        /// Persona id.
        id: String,
        /// State root scanned.
        state_root: String,
    },
}
