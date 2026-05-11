//! Boot-time persona discovery — scans the operator's state
//! root for installed persona dirs (`<id>-<version>/`),
//! re-parses each `persona.toml`, validates, and returns a
//! [`Vec<DiscoveredPersona>`] the daemon can register in its
//! shared [`crate::PersonaAdmin`] cell + merge into the
//! [`AgentsDirectory`] loader.
//!
//! Discovery is best-effort: malformed/missing manifests are
//! logged + skipped rather than aborting boot. The daemon
//! prefers a partially-populated catalog over a refusing-to-
//! start failure when one persona pack happens to be broken.
//!
//! # Layout convention
//!
//! Discovery walks a flat list of dirs at exactly one depth:
//!
//! ```text
//! <search_path>/
//!   cody-0.2.0/
//!     persona.toml
//!     agents.d/...
//!   muse-0.1.0/
//!     persona.toml
//!     ...
//! ```
//!
//! Sub-sub-dirs are NOT recursed (avoids combinatorial scan
//! cost + pin the contract: one persona == one dir).

use std::path::{Path, PathBuf};

use nexo_persona_manifest::PersonaManifest;
use tracing::{debug, warn};

/// One discovered persona — output of [`discover_personas`].
/// Carries enough info for the daemon to (a) register in the
/// admin catalog + (b) compute the absolute paths of each
/// contributed `agent_configs` entry to feed the loader.
///
/// Internal — no serde derive (would require pushing
/// derives onto upstream `PersonaManifest`). The wire type
/// crossing admin RPC is [`crate::PersonaListEntry`].
#[derive(Debug, Clone)]
pub struct DiscoveredPersona {
    /// Absolute install dir (`<search_path>/<id>-<version>/`).
    pub install_root: PathBuf,
    /// Re-parsed v2 manifest from the on-disk `persona.toml`.
    pub manifest: PersonaManifest,
}

impl DiscoveredPersona {
    /// Resolve the persona's `contributes.agent_configs`
    /// entries to absolute paths under [`Self::install_root`].
    /// Returns an empty vec when the persona doesn't declare
    /// any agent configs.
    pub fn agent_config_paths(&self) -> Vec<PathBuf> {
        match &self.manifest.persona.contributes {
            Some(c) => c
                .agent_configs
                .iter()
                .map(|rel| self.install_root.join(rel))
                .collect(),
            None => Vec::new(),
        }
    }

    /// Resolve the persona's `contributes.plugin_configs_partial`
    /// entries to absolute paths.
    pub fn plugin_config_partial_paths(&self) -> Vec<PathBuf> {
        match &self.manifest.persona.contributes {
            Some(c) => c
                .plugin_configs_partial
                .iter()
                .map(|rel| self.install_root.join(rel))
                .collect(),
            None => Vec::new(),
        }
    }
}

/// Scan one search path for installed personas. Skips
/// non-dir entries, dirs without a `persona.toml`, and dirs
/// whose `persona.toml` fails parse/validate (logged at WARN
/// so operators see broken packs).
///
/// Order is alphabetical by dir name (deterministic for
/// boot-time logging). Async to keep the daemon's startup
/// path uniformly tokio-flavored — uses
/// [`tokio::fs::read_dir`] + spawn_blocking for the per-file
/// parse so a slow disk doesn't park the runtime.
pub async fn discover_personas_in(search_path: &Path) -> Vec<DiscoveredPersona> {
    let mut found: Vec<DiscoveredPersona> = Vec::new();
    let mut read_dir = match tokio::fs::read_dir(search_path).await {
        Ok(d) => d,
        Err(e) => {
            // Missing search path is non-fatal — most
            // operators won't install personas. Log at debug
            // since this is the expected default state.
            debug!(
                path = %search_path.display(),
                error = %e,
                "persona search path absent — discovery skipped"
            );
            return found;
        }
    };

    let mut entries: Vec<PathBuf> = Vec::new();
    loop {
        match read_dir.next_entry().await {
            Ok(Some(entry)) => entries.push(entry.path()),
            Ok(None) => break,
            Err(e) => {
                warn!(
                    path = %search_path.display(),
                    error = %e,
                    "persona discovery: read_dir entry failed"
                );
                break;
            }
        }
    }
    entries.sort();

    for entry_path in entries {
        let metadata = match tokio::fs::metadata(&entry_path).await {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    path = %entry_path.display(),
                    error = %e,
                    "persona discovery: stat failed; skipping"
                );
                continue;
            }
        };
        if !metadata.is_dir() {
            continue;
        }
        let manifest_path = entry_path.join("persona.toml");
        if !manifest_path.exists() {
            debug!(
                path = %entry_path.display(),
                "persona discovery: dir lacks persona.toml; skipping"
            );
            continue;
        }
        match parse_one(&manifest_path).await {
            Ok(manifest) => found.push(DiscoveredPersona {
                install_root: entry_path,
                manifest,
            }),
            Err(reason) => warn!(
                path = %manifest_path.display(),
                error = %reason,
                "persona discovery: manifest parse/validate failed; skipping"
            ),
        }
    }
    found
}

/// Scan multiple search paths in order, concatenating
/// results. Duplicate ids across search paths are kept in
/// scan order (caller decides dedup policy — disk-backed
/// admin's `register` overwrites by id, so the LAST scan
/// path wins).
pub async fn discover_personas(search_paths: &[PathBuf]) -> Vec<DiscoveredPersona> {
    let mut all: Vec<DiscoveredPersona> = Vec::new();
    for path in search_paths {
        all.extend(discover_personas_in(path).await);
    }
    all
}

async fn parse_one(manifest_path: &Path) -> Result<PersonaManifest, String> {
    let bytes = tokio::fs::read(manifest_path)
        .await
        .map_err(|e| format!("read: {e}"))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| format!("utf8: {e}"))?;
    let manifest = nexo_persona_manifest::parse_str(text).map_err(|e| format!("parse: {e}"))?;
    nexo_persona_manifest::validate(&manifest).map_err(|e| format!("validate: {e}"))?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_persona(dir: &Path, id: &str, version: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let body = format!(
            r#"manifest_version = 2
[persona]
id = "{id}"
version = "{version}"
description = "Test persona"
min_nexo_version = ">=0.1.0"
"#
        );
        std::fs::write(dir.join("persona.toml"), body).unwrap();
    }

    #[tokio::test]
    async fn discover_returns_empty_when_search_path_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nonexistent");
        let found = discover_personas_in(&missing).await;
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn discover_finds_well_formed_persona_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        write_persona(&tmp.path().join("cody-0.2.0"), "cody", "0.2.0");
        write_persona(&tmp.path().join("muse-0.1.0"), "muse", "0.1.0");
        let found = discover_personas_in(tmp.path()).await;
        assert_eq!(found.len(), 2);
        // Alphabetical order: cody before muse.
        assert_eq!(found[0].manifest.persona.id, "cody");
        assert_eq!(found[1].manifest.persona.id, "muse");
    }

    #[tokio::test]
    async fn discover_skips_dirs_without_persona_toml() {
        let tmp = tempfile::tempdir().unwrap();
        // Real persona.
        write_persona(&tmp.path().join("cody-0.2.0"), "cody", "0.2.0");
        // Decoy dir with no persona.toml.
        std::fs::create_dir_all(tmp.path().join("not-a-persona")).unwrap();
        std::fs::write(tmp.path().join("not-a-persona/README.md"), b"hello").unwrap();
        let found = discover_personas_in(tmp.path()).await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.persona.id, "cody");
    }

    #[tokio::test]
    async fn discover_skips_packs_with_invalid_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        // Valid one.
        write_persona(&tmp.path().join("cody-0.2.0"), "cody", "0.2.0");
        // Broken manifest — uppercase id fails validate.
        let bad_dir = tmp.path().join("broken-0.1.0");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(
            bad_dir.join("persona.toml"),
            r#"manifest_version = 2
[persona]
id = "BadCase"
version = "0.1.0"
description = "x"
min_nexo_version = ">=0.1.0"
"#,
        )
        .unwrap();
        let found = discover_personas_in(tmp.path()).await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.persona.id, "cody");
    }

    #[tokio::test]
    async fn discover_skips_v1_manifests() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("legacy-0.1.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("persona.toml"),
            r#"manifest_version = 1
[persona]
id = "legacy"
version = "0.1.0"
description = "v1 pack"
min_nexo_version = ">=0.1.0"
"#,
        )
        .unwrap();
        let found = discover_personas_in(tmp.path()).await;
        assert!(found.is_empty(), "v1 packs are not discoverable by daemon");
    }

    #[tokio::test]
    async fn discover_personas_concatenates_multiple_search_paths() {
        let tmp_a = tempfile::tempdir().unwrap();
        let tmp_b = tempfile::tempdir().unwrap();
        write_persona(&tmp_a.path().join("cody-0.2.0"), "cody", "0.2.0");
        write_persona(&tmp_b.path().join("muse-0.1.0"), "muse", "0.1.0");
        let paths = vec![tmp_a.path().to_path_buf(), tmp_b.path().to_path_buf()];
        let found = discover_personas(&paths).await;
        assert_eq!(found.len(), 2);
    }

    #[tokio::test]
    async fn agent_config_paths_resolves_under_install_root() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cody-0.2.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("persona.toml"),
            r#"manifest_version = 2
[persona]
id = "cody"
version = "0.2.0"
description = "x"
min_nexo_version = ">=0.1.0"
[persona.contributes]
agent_configs = ["agents.d/cody.yaml", "agents.d/cody-secondary.yaml"]
"#,
        )
        .unwrap();
        let found = discover_personas_in(tmp.path()).await;
        assert_eq!(found.len(), 1);
        let paths = found[0].agent_config_paths();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("cody-0.2.0/agents.d/cody.yaml"));
        assert!(paths[1].ends_with("cody-0.2.0/agents.d/cody-secondary.yaml"));
    }
}
