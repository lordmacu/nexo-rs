//! `nexo-persona-installer` — install / list / remove v2
//! persona packs from GitHub Releases. Sibling of
//! `nexo-ext-installer`'s plugin install path; reuses ~60% of
//! the resolve+download plumbing via Phase F1's
//! `ExtractContract` abstraction.
//!
//! # Surface
//!
//! - [`PersonaExtractContract`] — adapts the v2 persona
//!   manifest parser to the generic resolver.
//! - [`install_persona`] + [`InstalledPersona`] +
//!   [`InstallInputs`] — end-to-end install pipeline
//!   (resolve → validate → download → verify → extract).
//! - [`PersonaAdmin`] trait + [`InMemoryPersonaAdmin`] +
//!   [`PersonaListEntry`] — admin layer DTOs the daemon's
//!   admin RPC routes call.
//! - [`LifecycleEvent`] + [`DEFAULT_LIFECYCLE_SUBJECT_PREFIX`]
//!   — lifecycle events emitted on install / remove / failure.
//! - [`PersonaInstallError`] — typed errors surfaced to the
//!   CLI / admin caller.
//!
//! # Pipeline at a glance
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │ install_persona(coords, target, install_root)            │
//! │   ├─ resolve_release_with_contract(PersonaExtractContract)│
//! │   ├─ validate(manifest)                                   │
//! │   ├─ idempotency check on <install_root>/<id>-<ver>/      │
//! │   ├─ download_and_verify_url → staging tarball            │
//! │   ├─ extract_persona_tarball → staging dir                │
//! │   ├─ rename(staging dir → final dir)                      │
//! │   └─ re-parse + validate on-disk persona.toml             │
//! │ → InstalledPersona { id, version, install_root, … }       │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! # Phase F3 of the `cody-cli-install` follow-up wave
//!
//! Reuses Phase F1's `ExtractContract` (introduced via
//! `cody-cli.F1` commit) so plugin + persona installers share
//! the GitHub Releases plumbing. Lifecycle events mirror the
//! plugin lifecycle shape from Phase 81.21.b for subscriber
//! decoder reuse.

#![deny(missing_docs)]

pub mod admin;
pub mod contract;
pub mod discover;
pub mod error;
pub mod lifecycle;
pub mod orchestrator;

pub use admin::{InMemoryPersonaAdmin, PersonaAdmin, PersonaListEntry};
pub use contract::PersonaExtractContract;
pub use discover::{discover_personas, discover_personas_in, DiscoveredPersona};
pub use error::PersonaInstallError;
pub use lifecycle::{LifecycleEvent, DEFAULT_LIFECYCLE_SUBJECT_PREFIX};
pub use orchestrator::{
    install_persona, InstallInputs, InstalledPersona, MAX_ENTRIES, MAX_ENTRY_BYTES,
    MAX_EXTRACTED_BYTES, MAX_TARBALL_BYTES,
};
