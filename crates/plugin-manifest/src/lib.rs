//! `nexo-plugin.toml` manifest schema + validator.
//!
//! Defines the TOML manifest contract every native Rust nexo plugin
//! must ship.
//!
//! Distinction vs `crates/extensions/<n>/plugin.toml`:
//! that schema describes **subprocess tool extensions** (stdio /
//! HTTP MCP servers spawned as separate processes). This schema
//! describes **native Rust plugins** that link into the daemon
//! and ship full mini-applications (agents + tools + skills +
//! channels + advisors + capability gates).

pub mod admin;
pub mod compat_v1;
pub mod config_schema;
pub mod dashboard;
pub mod discover;
pub mod error;
pub mod http;
pub mod id_regex;
pub mod manifest;
pub mod metrics;
pub mod pairing;
pub mod public_tunnel;
pub mod sandbox;
pub mod validate;

pub use discover::{discover_in_root, PLUGIN_MANIFEST_FILENAMES};

pub use config_schema::{
    is_validation_bypassed, validate_config, ConfigSchemaError, SKIP_SCHEMA_ENV,
};
pub use error::ManifestError;
pub use pairing::{PairingFieldDescriptor, PairingKind, PairingSection};
pub use public_tunnel::PluginPublicTunnelSection;
pub use manifest::{
    AdminCapabilities, AdvisorsSection, AgentsSection, BrokerCapability, Capabilities, Capability,
    CapabilityGateDecl, CapabilityGatesSection, ChannelDecl, ChannelsSection,
    ConfigSchemaSection, CredentialsSchemaSection,
    ConfigSection, ConfigShape, ContractsSection, EntrypointSection, ExtendsSection, GateKind,
    GateRisk, HttpServerCapability,
    MetaSection, PluginManifest, PluginSection, RequiresSection, SkillsSection, SupervisorSection,
    ToolsSection, UiHint, UiSection, CURRENT_MANIFEST_VERSION, EXTENDS_SECTIONS,
    PLUGIN_MANIFEST_FILENAME, SUPERVISOR_STDERR_TAIL_MAX,
};
pub use sandbox::{
    contains_state_dir_token, path_under_or_equals_denylist, SandboxNetwork, SandboxPathKind,
    SandboxSection, SANDBOX_DENYLIST_HOME_SUBPATHS, SANDBOX_DENYLIST_HOST_PATHS,
    SANDBOX_STATE_DIR_TOKEN,
};
