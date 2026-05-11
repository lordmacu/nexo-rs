//! Admin-layer trait + DTOs the daemon's admin RPC routes
//! call to drive persona install / list / remove. Mirrors
//! the plugin-admin surface (Phase 31.1.c) so the same admin
//! plugin can speak both protocols.
//!
//! `PersonaAdmin` is `async_trait`-flavored. The default
//! implementation lives in [`InMemoryPersonaAdmin`] for tests
//! + early dev; the daemon wires a disk-backed impl in
//! Phase F5.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::PersonaInstallError;

/// Compact summary of one installed persona, returned by
/// [`PersonaAdmin::list`]. Distinct from [`crate::InstalledPersona`]
/// because the admin layer doesn't need to ship the full
/// manifest over the RPC wire on every list call (CLI can
/// fetch detail via [`PersonaAdmin::get`] when the operator
/// asks for it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaListEntry {
    /// Persona id.
    pub id: String,
    /// Installed version.
    pub version: Version,
    /// On-disk install root.
    pub install_root: PathBuf,
    /// `<owner>/<repo>` the pack was sourced from.
    pub source_repo: String,
    /// Wall-clock install timestamp (carried through from
    /// the install operation).
    pub installed_at: DateTime<Utc>,
}

/// Admin-RPC-friendly persona manager. Daemon binds its disk-
/// backed impl into the admin plugin's RPC routes; tests bind
/// [`InMemoryPersonaAdmin`].
#[async_trait]
pub trait PersonaAdmin: Send + Sync {
    /// Add a fully-installed persona to the admin's catalog.
    /// Called by the orchestrator after [`crate::install_persona`]
    /// returns. Idempotent: re-registering the same `id+version`
    /// is a no-op.
    async fn register(
        &self,
        installed: crate::orchestrator::InstalledPersona,
    ) -> Result<(), PersonaInstallError>;

    /// Enumerate installed personas. Order is not guaranteed;
    /// callers needing a stable order should sort client-side.
    async fn list(&self) -> Result<Vec<PersonaListEntry>, PersonaInstallError>;

    /// Look up one persona by id. Returns `Ok(None)` rather
    /// than an error when the id is unknown so CLI callers
    /// can render "not found" without a `match` on error
    /// variants.
    async fn get(&self, id: &str) -> Result<Option<PersonaListEntry>, PersonaInstallError>;

    /// Remove a persona by id. Returns the removed entry so
    /// the caller can publish a lifecycle event with the
    /// correct version. Errors with [`PersonaInstallError::NotFound`]
    /// when the id is unknown.
    async fn remove(&self, id: &str) -> Result<PersonaListEntry, PersonaInstallError>;
}

/// In-memory implementation backed by a `BTreeMap<id, entry>`.
/// Useful for tests (no I/O), boot-time staging (before the
/// daemon's disk catalog is loaded), and dry-run modes.
///
/// `register` overwrites any existing entry for the same id —
/// matches the disk-backed impl semantics where an
/// idempotent re-install with a NEW version replaces the
/// catalog row.
#[derive(Debug, Default)]
pub struct InMemoryPersonaAdmin {
    inner: Mutex<BTreeMap<String, PersonaListEntry>>,
}

impl InMemoryPersonaAdmin {
    /// New empty catalog.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PersonaAdmin for InMemoryPersonaAdmin {
    async fn register(
        &self,
        installed: crate::orchestrator::InstalledPersona,
    ) -> Result<(), PersonaInstallError> {
        let entry = PersonaListEntry {
            id: installed.id.clone(),
            version: installed.version,
            install_root: installed.install_root,
            source_repo: format!("{}/{}", installed.coords.owner, installed.coords.repo),
            installed_at: installed.installed_at,
        };
        self.inner.lock().unwrap().insert(installed.id, entry);
        Ok(())
    }

    async fn list(&self) -> Result<Vec<PersonaListEntry>, PersonaInstallError> {
        Ok(self.inner.lock().unwrap().values().cloned().collect())
    }

    async fn get(&self, id: &str) -> Result<Option<PersonaListEntry>, PersonaInstallError> {
        Ok(self.inner.lock().unwrap().get(id).cloned())
    }

    async fn remove(&self, id: &str) -> Result<PersonaListEntry, PersonaInstallError> {
        let mut guard = self.inner.lock().unwrap();
        guard.remove(id).ok_or_else(|| PersonaInstallError::NotFound {
            id: id.to_string(),
            state_root: "<in-memory>".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::InstalledPersona;
    use chrono::TimeZone;

    fn fake_installed(id: &str, version: &str) -> InstalledPersona {
        let manifest = nexo_persona_manifest::parse_str(&format!(
            r#"manifest_version = 2
[persona]
id = "{id}"
version = "{version}"
description = "x"
min_nexo_version = ">=0.1.0"
"#
        ))
        .unwrap();
        InstalledPersona {
            id: id.to_string(),
            version: version.parse().unwrap(),
            install_root: format!("/tmp/personas/{id}-{version}").into(),
            coords: nexo_ext_installer::RepoCoords::parse(&format!("alice/{id}@v{version}"))
                .unwrap(),
            installed_at: Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap(),
            manifest,
            tarball_bytes: 0,
            was_already_present: false,
        }
    }

    #[tokio::test]
    async fn in_memory_admin_register_list_get_remove_round_trip() {
        let admin = InMemoryPersonaAdmin::new();

        admin.register(fake_installed("cody", "0.2.0")).await.unwrap();
        admin.register(fake_installed("muse", "0.1.0")).await.unwrap();

        let listed = admin.list().await.unwrap();
        assert_eq!(listed.len(), 2);

        let cody = admin.get("cody").await.unwrap().expect("cody present");
        assert_eq!(cody.version.to_string(), "0.2.0");
        assert_eq!(cody.source_repo, "alice/cody");

        let removed = admin.remove("cody").await.unwrap();
        assert_eq!(removed.id, "cody");
        assert!(admin.get("cody").await.unwrap().is_none());

        match admin.remove("cody").await {
            Err(PersonaInstallError::NotFound { id, .. }) => assert_eq!(id, "cody"),
            other => panic!("expected NotFound after second remove, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn re_register_overwrites_same_id() {
        let admin = InMemoryPersonaAdmin::new();
        admin.register(fake_installed("cody", "0.1.0")).await.unwrap();
        admin.register(fake_installed("cody", "0.2.0")).await.unwrap();
        let cody = admin.get("cody").await.unwrap().unwrap();
        assert_eq!(
            cody.version.to_string(),
            "0.2.0",
            "register must overwrite, matching disk-backed impl semantics"
        );
        assert_eq!(admin.list().await.unwrap().len(), 1);
    }
}
