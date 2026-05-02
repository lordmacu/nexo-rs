//! Phase 81.1 — `nexo-plugin.toml` manifest schema + validator.
//!
//! Defines the TOML manifest contract every native Rust nexo plugin
//! must ship. Future phases consume this:
//! - **81.2** `NexoPlugin` trait + lifecycle context.
//! - **81.3** namespace enforcement at boot (registry hook).
//! - **81.5** `PluginRegistry::discover` walks the workspace + user
//!   plugin dirs reading manifests.
//! - **81.9** `Mode::Run` boot loop replaces per-plugin wire with
//!   registry iteration.
//!
//! Distinction vs `crates/extensions/<n>/plugin.toml` (Phase 11.1):
//! that schema describes **subprocess tool extensions** (stdio /
//! HTTP MCP servers spawned as separate processes). This schema
//! describes **native Rust plugins** that link into the daemon
//! and ship full mini-applications (agents + tools + skills +
//! channels + advisors + capability gates).
//!
//! IRROMPIBLE refs:
//! - `research/src/plugins/manifest-types.ts:1-20` — `PluginConfigUiHint`
//!   shape we mirror in [`UiHint`].
//! - `research/src/plugins/manifest.ts:17` — `PLUGIN_MANIFEST_FILENAME`
//!   precedent (OpenClaw uses `openclaw.plugin.json`; we use
//!   `nexo-plugin.toml` — TOML over JSON5 because Rust ecosystem
//!   stable).
//! - `research/src/plugins/manifest.ts:54-60` —
//!   `PluginManifestActivation` capability enum (we mirror as
//!   [`Capability`]).
//! - `research/extensions/firecrawl/openclaw.plugin.json` — example
//!   with `providerAuthEnvVars`, `uiHints`, `contracts`.
//! - `research/extensions/brave/openclaw.plugin.json` —
//!   `configContracts.compatibilityRuntimePaths` (deferred to
//!   future schema bump).
//! - claude-code-leak `src/tools/*` — **absence of plugin manifest**;
//!   leak hardcodes every tool as a TS module. Internal nexo split
//!   between core tools and plugin-shippable tools is what 81.x
//!   delivers.
//! - Internal precedent: `crates/extensions/src/manifest.rs:108-132`
//!   (Phase 11.1 extension manifest — distinct concept, distinct
//!   filename).

pub mod config_schema;
pub mod error;
pub mod manifest;
pub mod validate;

pub use config_schema::{
    is_validation_bypassed, validate_config, ConfigSchemaError, SKIP_SCHEMA_ENV,
};
pub use error::ManifestError;
pub use manifest::{
    AdvisorsSection, AgentsSection, Capabilities, Capability, CapabilityGateDecl,
    CapabilityGatesSection, ChannelDecl, ChannelsSection, ConfigSection, ContractsSection,
    GateKind, GateRisk, MetaSection, PluginManifest, PluginSection, RequiresSection,
    SkillsSection, ToolsSection, UiHint, UiSection, PLUGIN_MANIFEST_FILENAME,
};
