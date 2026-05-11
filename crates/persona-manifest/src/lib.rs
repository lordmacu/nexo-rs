//! v2 persona-pack manifest schema + validator. Sibling of
//! `nexo-plugin-manifest` but for personas (out-of-tree agent
//! definitions: system prompt + plugin bindings + workspace
//! seed + secrets templates) installed via `nexo persona
//! install <owner>/<repo>`.
//!
//! # v1 vs v2
//!
//! - **v1** (`manifest_version = 1`) — install.sh-driven flow
//!   (Phase B of cody-extraction). The current Cody persona
//!   pack ships v1. Stays supported by install.sh; this crate
//!   does NOT parse v1 and rejects it with a clear migration
//!   error.
//! - **v2** (`manifest_version = 2`) — daemon-driven flow.
//!   `nexo persona install` consumes v2 manifests, applies
//!   the same path-safety + id/semver validation the plugin
//!   manifest enforces, and writes contributed files into the
//!   operator's nexo state dir. Phase F2 of the
//!   `cody-cli-install` follow-up wave.
//!
//! # Surface
//!
//! - [`PersonaManifest`] — the parsed top-level shape.
//! - [`parse_str`] — TOML deserialization with version
//!   discrimination (rejects non-v2 explicitly).
//! - [`validate`] — runs the full validator pipeline (id
//!   regex, semver shape, path safety, env-var name shape).
//!   Callers typically chain `parse_str` + `validate`.

#![deny(missing_docs)]

pub mod error;
pub mod manifest;
pub mod validate;

pub use error::PersonaManifestError;
pub use manifest::{
    PersonaContributes, PersonaEnvVar, PersonaManifest, PersonaMeta, PersonaRequires,
    PersonaSection, MANIFEST_VERSION_V2,
};
pub use validate::validate;

/// Convenience: parse a TOML manifest string into a typed
/// [`PersonaManifest`]. Rejects any `manifest_version` other
/// than [`MANIFEST_VERSION_V2`] up-front so the caller gets a
/// clear, actionable error instead of a confusing missing-
/// fields parse failure.
///
/// Does NOT run the structural validator — call [`validate`]
/// after `parse_str` if you want id/path/semver checks too.
pub fn parse_str(input: &str) -> Result<PersonaManifest, PersonaManifestError> {
    // Discriminate on `manifest_version` BEFORE the typed
    // parse so v1 packs error with a migration message
    // instead of the cryptic "missing required field" we'd
    // get if v1 fields differed.
    #[derive(serde::Deserialize)]
    struct VersionProbe {
        #[serde(default)]
        manifest_version: Option<u32>,
    }

    let probe: VersionProbe = toml::from_str(input)
        .map_err(|e| PersonaManifestError::ParseToml(format!("manifest is not valid TOML: {e}")))?;

    match probe.manifest_version {
        None => return Err(PersonaManifestError::MissingManifestVersion),
        Some(MANIFEST_VERSION_V2) => {}
        Some(1) => {
            return Err(PersonaManifestError::UnsupportedManifestVersion {
                got: 1,
                hint: "v1 packs install via the persona's install.sh — \
                       `nexo persona install` only consumes v2. Bump \
                       manifest_version to 2 (no field-shape changes \
                       required) to opt in.",
            });
        }
        Some(other) => {
            return Err(PersonaManifestError::UnsupportedManifestVersion {
                got: other,
                hint: "only manifest_version = 2 is recognized by this \
                       daemon; upgrade nexo-rs or downgrade the persona \
                       pack",
            });
        }
    }

    toml::from_str::<PersonaManifest>(input).map_err(|e| {
        PersonaManifestError::ParseToml(format!("v2 manifest typed parse failed: {e}"))
    })
}
