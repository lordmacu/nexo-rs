//! [`PersonaExtractContract`] — adapts the v2 persona manifest
//! parser to [`nexo_ext_installer::ExtractContract`] so the
//! same resolve+download pipeline serves both
//! `nexo plugin install` and `nexo persona install` via the
//! parameterized `resolve_release_with_contract`.

use nexo_ext_installer::{ExtractContract, InstallError, RepoCoords};
use nexo_persona_manifest::PersonaManifest;

/// Plugs the v2 persona-manifest parser into the resolver.
/// Manifest filename is `persona.toml` (matches the existing
/// v1 install.sh convention so persona repos can publish
/// either flavor); typed manifest is
/// [`nexo_persona_manifest::PersonaManifest`]; id used for
/// tarball naming is `persona.id`.
///
/// Stateless; cheap to construct on every resolve call.
#[derive(Debug, Default, Clone, Copy)]
pub struct PersonaExtractContract;

impl ExtractContract for PersonaExtractContract {
    type Manifest = PersonaManifest;

    fn manifest_asset_name(&self) -> &'static str {
        "persona.toml"
    }

    fn parse_manifest(
        &self,
        bytes: &[u8],
        coords: &RepoCoords,
    ) -> Result<Self::Manifest, InstallError> {
        let text = std::str::from_utf8(bytes).map_err(|e| InstallError::ReleaseShape {
            owner: coords.owner.clone(),
            repo: coords.repo.clone(),
            reason: format!("persona.toml is not valid UTF-8: {e}"),
        })?;
        // parse_str maps v1 / unknown / missing version to
        // typed errors with actionable hints; we widen those
        // into the resolver's ReleaseShape variant since the
        // resolver exposes a single error surface to CLI
        // callers.
        nexo_persona_manifest::parse_str(text).map_err(|e| InstallError::ReleaseShape {
            owner: coords.owner.clone(),
            repo: coords.repo.clone(),
            reason: format!("persona.toml: {e}"),
        })
    }

    fn manifest_id(&self, manifest: &Self::Manifest) -> String {
        manifest.persona.id.clone()
    }
}
