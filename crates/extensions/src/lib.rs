//! Phase 11 — extension system for third-party plugins.
//!
//! 11.1 ships only the manifest parser. Later sub-phases add discovery,
//! runtime spawn, tool registration, lifecycle hooks, and CLI integration.

pub mod cli;
pub mod discovery;
pub mod manifest;
pub mod runtime;
pub mod state;
pub mod watch;

pub use discovery::{
    collect_mcp_declarations, DiagnosticLevel, DiscoveryDiagnostic, DiscoveryReport,
    ExtensionCandidate, ExtensionDiscovery, ExtensionMcpDecl, ExtensionOrigin,
};
pub use manifest::{
    agent_version, is_reserved_id, register_reserved_ids, set_agent_version, Capabilities,
    ExtensionManifest, ManifestError, Meta, PluginMeta, Transport, AGENT_VERSION_FALLBACK,
    MANIFEST_FILENAME, MAX_DESC_LEN, MAX_ID_LEN, RESERVED_IDS,
};
pub use runtime::{
    is_valid_hook, CallError, HandshakeInfo, HookResponse, RpcError, RuntimeState, StartError,
    StdioRuntime, StdioSpawnOptions, ToolDescriptor, HOOK_NAMES,
};
pub use state::{
    ensure_state_dir, extension_state_root, state_dir_for, EXTENSION_STATE_ROOT_ENV,
};
// Back-compat alias so call sites that imported the old constant keep
// compiling. New code should use `agent_version()` to respect overrides.
#[allow(deprecated)]
#[deprecated(note = "use agent_version() to respect set_agent_version() overrides")]
pub use manifest::AGENT_VERSION;
pub use watch::{spawn_extensions_watcher, KnownPluginSnapshot, PLUGIN_TOML_FILENAME};
