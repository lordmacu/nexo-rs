//! Extension manifest (`plugin.toml`) — parser and semantic validator.
//!
//! Parsing is a two-step process: serde-driven structural parse, then
//! `validate()` for semantic rules (id shape, reserved ids, semver, capability
//! name shape, transport variant invariants). `from_str`/`from_path` run both.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;
use thiserror::Error;

/// File name every extension ships at its root directory.
pub const MANIFEST_FILENAME: &str = "plugin.toml";

/// Fallback version string when the host binary has not called
/// [`set_agent_version`]. Matches the extensions crate's own version;
/// useful for unit tests and embedded uses where the `agent` binary
/// doesn't initialize the override.
pub const AGENT_VERSION_FALLBACK: &str = env!("CARGO_PKG_VERSION");

/// Back-compat constant. Prefer [`agent_version()`] — the old name
/// always reads the fallback and never sees overrides.
#[deprecated(note = "use agent_version() to respect set_agent_version() overrides")]
pub const AGENT_VERSION: &str = AGENT_VERSION_FALLBACK;

static AGENT_VERSION_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Set the agent version the extension loader compares against
/// `plugin.min_agent_version`. Intended for the `agent` binary to call
/// once at startup so the user sees the host version, not the library's.
/// Subsequent calls after the first one are ignored.
pub fn set_agent_version(version: impl Into<String>) -> Result<(), &'static str> {
    AGENT_VERSION_OVERRIDE
        .set(version.into())
        .map_err(|_| "agent version already initialized")
}

/// Current agent version — respects an override set by the host binary,
/// falls back to the extensions crate version otherwise.
pub fn agent_version() -> &'static str {
    AGENT_VERSION_OVERRIDE
        .get()
        .map(String::as_str)
        .unwrap_or(AGENT_VERSION_FALLBACK)
}

pub const MAX_ID_LEN: usize = 64;
pub const MAX_DESC_LEN: usize = 512;
pub const MAX_CAPABILITY_NAME_LEN: usize = 64;

/// IDs owned by native (compiled-in) plugins. Extensions cannot claim these,
/// even if the native crate is not loaded on a given deployment.
pub const RESERVED_IDS: &[&str] = &[
    "agent",
    "browser",
    "core",
    "email",
    "heartbeat",
    "memory",
    "telegram",
    "whatsapp",
];

/// Host-side extension of [`RESERVED_IDS`]. The `agent` binary can add
/// ids belonging to native crates that aren't baked into this crate's
/// static list — useful when a new bundled plugin lands before
/// `RESERVED_IDS` is updated.
///
/// Populated via [`register_reserved_ids`]. `is_reserved` unions the
/// static and dynamic sets.
static DYNAMIC_RESERVED_IDS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Idempotent: first call wins; subsequent calls are ignored with an
/// `Err(&'static str)` so the caller can log + move on.
pub fn register_reserved_ids(ids: impl IntoIterator<Item = String>) -> Result<(), &'static str> {
    let mut v: Vec<String> = ids.into_iter().collect();
    v.sort();
    v.dedup();
    DYNAMIC_RESERVED_IDS
        .set(v)
        .map_err(|_| "reserved ids already initialized")
}

/// Return true if `id` is reserved by either the static list or the
/// host-registered dynamic set.
pub fn is_reserved_id(id: &str) -> bool {
    if RESERVED_IDS.contains(&id) {
        return true;
    }
    DYNAMIC_RESERVED_IDS
        .get()
        .map(|v| v.iter().any(|s| s == id))
        .unwrap_or(false)
}

static ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_-]*$").unwrap());

static CAPABILITY_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_]*$").unwrap());

static HTTP_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://[^/\s:?#]+(?::\d+)?(?:/.*)?$").unwrap());

// ─── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    pub plugin: PluginMeta,
    #[serde(default)]
    pub capabilities: Capabilities,
    pub transport: Transport,
    #[serde(default)]
    pub meta: Meta,
    /// Phase 12.7 — optional MCP server declarations. Merged into the
    /// runtime with keys `{ext_id}.{name}`; `${EXTENSION_ROOT}` placeholders
    /// are resolved at merge time.
    #[serde(default)]
    pub mcp_servers: std::collections::BTreeMap<String, nexo_config::McpServerYaml>,
    /// Phase 11.5 follow-up — opt-in context propagation. When enabled,
    /// `ExtensionTool::call` injects `_meta.agent_id` and
    /// `_meta.session_id` into the JSON-RPC args object.
    #[serde(default)]
    pub context: ExtensionContextConfig,
    /// Informational declaration of host binaries and environment variables
    /// the extension expects. Loader does not enforce — consumed by docs,
    /// `status` tools, and ops tooling.
    #[serde(default)]
    pub requires: Requires,
}

/// Per-extension context propagation knobs. Defaults to no injection so
/// extensions with `deny-unknown-fields` schemas are not broken by
/// unexpected `_meta` keys.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionContextConfig {
    #[serde(default)]
    pub passthrough: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    #[serde(default)]
    pub bins: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
}

impl Requires {
    /// Return the subset of declared `bins` not findable via `$PATH`
    /// and the subset of declared `env` not present in the process
    /// environment. Empty vectors mean the precondition is satisfied.
    /// This is a best-effort surface — Windows + unusual shells may
    /// see false positives; callers log instead of fail the spawn.
    pub fn missing(&self) -> (Vec<String>, Vec<String>) {
        let path_var = std::env::var_os("PATH");
        let missing_bins = self
            .bins
            .iter()
            .filter(|b| match &path_var {
                None => true,
                Some(pv) => !std::env::split_paths(pv).any(|dir| {
                    let candidate = dir.join(b);
                    candidate.is_file()
                        || (cfg!(target_os = "windows")
                            && candidate.with_extension("exe").is_file())
                }),
            })
            .cloned()
            .collect();
        let missing_env = self
            .env
            .iter()
            .filter(|k| std::env::var_os(k).map(|v| v.is_empty()).unwrap_or(true))
            .cloned()
            .collect();
        (missing_bins, missing_env)
    }
}

/// Max length of an MCP server key declared inside an extension manifest.
pub const MAX_MCP_SERVER_NAME_LEN: usize = 32;

static MCP_NAME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_-]*$").unwrap());

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginMeta {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub min_agent_version: Option<String>,
    /// Hook firing order. Lower values fire first (e.g. a `security`
    /// extension with `priority = -10` runs before a `logger` with
    /// `priority = 0`). Default `0`. Within a priority class the
    /// order is whatever discovery returned (deterministic but
    /// file-system-dependent).
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    /// Phase 19 follow-up — one entry per `kind` the extension
    /// implements as a poller module. The runtime calls
    /// `poll_tick { kind, ctx }` on the extension for each tick.
    #[serde(default)]
    pub pollers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Nats {
        subject_prefix: String,
    },
    Http {
        url: String,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
}

// ─── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("failed to read manifest at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse TOML: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("plugin.id is empty")]
    EmptyId,

    #[error("plugin.id `{id}` must match `^[a-z][a-z0-9_-]*$` (max 64 chars)")]
    InvalidId { id: String },

    #[error("plugin.id `{id}` is reserved for native plugins")]
    ReservedId { id: String },

    #[error("plugin.version `{version}` is not valid semver: {source}")]
    InvalidVersion {
        version: String,
        #[source]
        source: semver::Error,
    },

    #[error("plugin.min_agent_version `{required}` is not valid semver: {source}")]
    InvalidMinAgentVersion {
        required: String,
        #[source]
        source: semver::Error,
    },

    #[error("plugin requires agent >= {required}, current is {current}")]
    AgentTooOld { required: String, current: String },

    #[error("plugin must declare at least one tool, hook, channel, or provider")]
    NoCapabilities,

    #[error("{field} name `{name}` is invalid (expected `^[a-z][a-z0-9_]*$`, max 64 chars)")]
    InvalidCapabilityName { field: &'static str, name: String },

    #[error("duplicate {field} name `{name}`")]
    DuplicateCapabilityName { field: &'static str, name: String },

    #[error("transport.stdio requires non-empty `command`")]
    EmptyStdioCommand,

    #[error("transport.nats requires non-empty `subject_prefix`")]
    EmptyNatsPrefix,

    #[error("transport.http url `{url}` is invalid: {reason}")]
    InvalidHttpUrl { url: String, reason: &'static str },

    #[error("description exceeds 512 characters")]
    DescriptionTooLong,

    #[error("mcp server name `{name}` is invalid (expected `^[a-z][a-z0-9_-]*$`, max 32 chars)")]
    InvalidMcpServerName { name: String },

    #[error("mcp server `{name}`: stdio requires non-empty command")]
    McpEmptyStdioCommand { name: String },

    #[error("mcp server `{name}`: url `{url}` is not valid http(s)")]
    McpInvalidHttpUrl { name: String, url: String },
}

// ─── Parsing + validation ─────────────────────────────────────────────────────

impl ExtensionManifest {
    /// Parse a manifest from TOML source and run full semantic validation.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(src: &str) -> Result<Self, ManifestError> {
        let manifest: ExtensionManifest = toml::from_str(src)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Read `plugin.toml` from disk (path to the file itself).
    pub fn from_path(path: &Path) -> Result<Self, ManifestError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_str(&raw)
    }

    pub fn id(&self) -> &str {
        &self.plugin.id
    }

    pub fn version(&self) -> &str {
        &self.plugin.version
    }

    /// Re-run all semantic rules. `from_str` already does this on parse; call
    /// again if you mutate a manifest post-construction.
    pub fn validate(&self) -> Result<(), ManifestError> {
        validate_id(&self.plugin.id)?;
        validate_version(&self.plugin.version)?;
        validate_min_agent_version(self.plugin.min_agent_version.as_deref())?;
        validate_description(self.plugin.description.as_deref())?;
        validate_capabilities(&self.capabilities)?;
        validate_transport(&self.transport)?;
        validate_mcp_servers(&self.mcp_servers)?;
        Ok(())
    }
}

fn validate_mcp_servers(
    servers: &std::collections::BTreeMap<String, nexo_config::McpServerYaml>,
) -> Result<(), ManifestError> {
    for (name, server) in servers {
        if name.is_empty() || name.len() > MAX_MCP_SERVER_NAME_LEN || !MCP_NAME_RE.is_match(name) {
            return Err(ManifestError::InvalidMcpServerName { name: name.clone() });
        }
        match server {
            nexo_config::McpServerYaml::Stdio { command, .. } => {
                if command.trim().is_empty() {
                    return Err(ManifestError::McpEmptyStdioCommand { name: name.clone() });
                }
            }
            nexo_config::McpServerYaml::StreamableHttp { url, .. }
            | nexo_config::McpServerYaml::Sse { url, .. }
            | nexo_config::McpServerYaml::Auto { url, .. } => match url::Url::parse(url) {
                Ok(parsed) if parsed.scheme() == "http" || parsed.scheme() == "https" => {}
                _ => {
                    return Err(ManifestError::McpInvalidHttpUrl {
                        name: name.clone(),
                        url: url.clone(),
                    })
                }
            },
        }
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), ManifestError> {
    if id.is_empty() {
        return Err(ManifestError::EmptyId);
    }
    if id.len() > MAX_ID_LEN || !ID_RE.is_match(id) {
        return Err(ManifestError::InvalidId { id: id.to_string() });
    }
    if is_reserved_id(id) {
        return Err(ManifestError::ReservedId { id: id.to_string() });
    }
    Ok(())
}

fn validate_version(version: &str) -> Result<(), ManifestError> {
    semver::Version::parse(version).map_err(|source| ManifestError::InvalidVersion {
        version: version.to_string(),
        source,
    })?;
    Ok(())
}

fn validate_min_agent_version(required: Option<&str>) -> Result<(), ManifestError> {
    let Some(required) = required else {
        return Ok(());
    };
    let parsed_required = semver::Version::parse(required).map_err(|source| {
        ManifestError::InvalidMinAgentVersion {
            required: required.to_string(),
            source,
        }
    })?;
    // Prefer the host-injected version (see `set_agent_version`); fall
    // back to this crate's version for unit tests and embedded callers.
    let current = agent_version();
    // A malformed host-injected version used to panic here and abort
    // extension discovery entirely. Treat an unparseable current
    // version as "ignore the min_agent_version gate" with a warning
    // — the extension still loads; the operator can fix the string.
    let parsed_current = match semver::Version::parse(current) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                current = %current,
                required = %required,
                error = %e,
                "agent version is not valid semver — skipping min_agent_version check"
            );
            return Ok(());
        }
    };
    if parsed_current < parsed_required {
        return Err(ManifestError::AgentTooOld {
            required: required.to_string(),
            current: current.to_string(),
        });
    }
    Ok(())
}

fn validate_description(desc: Option<&str>) -> Result<(), ManifestError> {
    if let Some(d) = desc {
        if d.chars().count() > MAX_DESC_LEN {
            return Err(ManifestError::DescriptionTooLong);
        }
    }
    Ok(())
}

fn validate_capabilities(caps: &Capabilities) -> Result<(), ManifestError> {
    let total = caps.tools.len()
        + caps.hooks.len()
        + caps.channels.len()
        + caps.providers.len()
        + caps.pollers.len();
    if total == 0 {
        return Err(ManifestError::NoCapabilities);
    }
    validate_capability_list("tools", &caps.tools)?;
    validate_capability_list("hooks", &caps.hooks)?;
    validate_capability_list("channels", &caps.channels)?;
    validate_capability_list("providers", &caps.providers)?;
    validate_capability_list("pollers", &caps.pollers)?;
    Ok(())
}

fn validate_capability_list(field: &'static str, list: &[String]) -> Result<(), ManifestError> {
    let mut seen = std::collections::HashSet::new();
    for name in list {
        if name.is_empty()
            || name.len() > MAX_CAPABILITY_NAME_LEN
            || !CAPABILITY_NAME_RE.is_match(name)
        {
            return Err(ManifestError::InvalidCapabilityName {
                field,
                name: name.clone(),
            });
        }
        if !seen.insert(name.as_str()) {
            return Err(ManifestError::DuplicateCapabilityName {
                field,
                name: name.clone(),
            });
        }
    }
    Ok(())
}

fn validate_transport(transport: &Transport) -> Result<(), ManifestError> {
    match transport {
        Transport::Stdio { command, .. } => {
            if command.trim().is_empty() {
                return Err(ManifestError::EmptyStdioCommand);
            }
        }
        Transport::Nats { subject_prefix } => {
            if subject_prefix.trim().is_empty() {
                return Err(ManifestError::EmptyNatsPrefix);
            }
        }
        Transport::Http { url } => {
            if url.trim().is_empty() {
                return Err(ManifestError::InvalidHttpUrl {
                    url: url.clone(),
                    reason: "empty url",
                });
            }
            if !HTTP_URL_RE.is_match(url) {
                return Err(ManifestError::InvalidHttpUrl {
                    url: url.clone(),
                    reason: "expected http(s)://host[:port][/path]",
                });
            }
        }
    }
    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod reserved_id_tests {
    use super::*;

    #[test]
    fn dynamic_ids_join_static_list() {
        // OnceLock is process-global — this test is the sole caller of
        // `register_reserved_ids` in the crate to avoid interference.
        let _ = register_reserved_ids(["slack".to_string(), "discord".to_string()]);
        assert!(is_reserved_id("agent"), "static id still reserved");
        assert!(is_reserved_id("slack"), "dynamic id now reserved");
        assert!(is_reserved_id("discord"), "dynamic id now reserved");
        assert!(!is_reserved_id("weather"), "unrelated id passes");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_STDIO: &str = r#"
[plugin]
id = "my-weather"
version = "0.1.0"

[capabilities]
tools = ["get_weather"]

[transport]
kind = "stdio"
command = "./my-weather"
"#;

    // --- Happy paths -----------------------------------------------------------

    #[test]
    fn context_defaults_to_false() {
        let m = ExtensionManifest::from_str(MINIMAL_STDIO).unwrap();
        assert!(!m.context.passthrough);
    }

    #[test]
    fn context_passthrough_parses_true() {
        let src = r#"
[plugin]
id = "ctx-ext"
version = "0.1.0"

[capabilities]
tools = ["echo"]

[context]
passthrough = true

[transport]
kind = "stdio"
command = "./ctx-ext"
"#;
        let m = ExtensionManifest::from_str(src).unwrap();
        assert!(m.context.passthrough);
    }

    #[test]
    fn requires_defaults_to_empty_lists() {
        let m = ExtensionManifest::from_str(MINIMAL_STDIO).unwrap();
        assert!(m.requires.bins.is_empty());
        assert!(m.requires.env.is_empty());
    }

    #[test]
    fn requires_parses_bins_and_env() {
        let src = r#"
[plugin]
id = "req-ext"
version = "0.1.0"

[capabilities]
tools = ["status"]

[requires]
bins = ["curl", "jq"]
env = ["API_TOKEN", "API_BASE_URL"]

[transport]
kind = "stdio"
command = "./req-ext"
"#;
        let m = ExtensionManifest::from_str(src).unwrap();
        assert_eq!(m.requires.bins, vec!["curl", "jq"]);
        assert_eq!(m.requires.env, vec!["API_TOKEN", "API_BASE_URL"]);
    }

    #[test]
    fn requires_rejects_unknown_fields() {
        let src = r#"
[plugin]
id = "req-bad"
version = "0.1.0"

[capabilities]
tools = ["status"]

[requires]
bins = ["curl"]
unexpected = true

[transport]
kind = "stdio"
command = "./req-bad"
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
    }

    #[test]
    fn parses_minimal_stdio() {
        let m = ExtensionManifest::from_str(MINIMAL_STDIO).unwrap();
        assert_eq!(m.id(), "my-weather");
        assert_eq!(m.version(), "0.1.0");
        assert_eq!(m.capabilities.tools, vec!["get_weather"]);
        match &m.transport {
            Transport::Stdio { command, args } => {
                assert_eq!(command, "./my-weather");
                assert!(args.is_empty());
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn parses_all_fields_stdio() {
        let src = r#"
[plugin]
id = "my_plugin"
version = "1.2.3"
name = "My Plugin"
description = "Does things"
min_agent_version = "0.1.0"

[capabilities]
tools = ["t1", "t2"]
hooks = ["before_message"]
channels = ["my_channel"]
providers = ["my_provider"]

[transport]
kind = "stdio"
command = "/usr/local/bin/my-plugin"
args = ["--json", "--quiet"]

[meta]
author = "Cristian García"
license = "MIT"
"#;
        let m = ExtensionManifest::from_str(src).unwrap();
        assert_eq!(m.plugin.name.as_deref(), Some("My Plugin"));
        assert_eq!(m.meta.license.as_deref(), Some("MIT"));
        if let Transport::Stdio { args, .. } = &m.transport {
            assert_eq!(args, &vec!["--json".to_string(), "--quiet".to_string()]);
        } else {
            panic!("expected stdio");
        }
    }

    #[test]
    fn parses_nats_transport() {
        let src = r#"
[plugin]
id = "nats-plugin"
version = "0.1.0"

[capabilities]
hooks = ["after_message"]

[transport]
kind = "nats"
subject_prefix = "ext.nats-plugin"
"#;
        let m = ExtensionManifest::from_str(src).unwrap();
        match m.transport {
            Transport::Nats { subject_prefix } => {
                assert_eq!(subject_prefix, "ext.nats-plugin");
            }
            _ => panic!("expected nats"),
        }
    }

    #[test]
    fn parses_http_transport() {
        let src = r#"
[plugin]
id = "http-plugin"
version = "0.1.0"

[capabilities]
providers = ["remote_llm"]

[transport]
kind = "http"
url = "https://api.example.com:8443/rpc"
"#;
        let m = ExtensionManifest::from_str(src).unwrap();
        assert!(matches!(m.transport, Transport::Http { .. }));
    }

    // --- ID rules --------------------------------------------------------------

    #[test]
    fn rejects_empty_id() {
        let src = MINIMAL_STDIO.replace("\"my-weather\"", "\"\"");
        let err = ExtensionManifest::from_str(&src).unwrap_err();
        assert!(matches!(err, ManifestError::EmptyId));
    }

    #[test]
    fn rejects_uppercase_id() {
        let src = MINIMAL_STDIO.replace("\"my-weather\"", "\"MyPlugin\"");
        let err = ExtensionManifest::from_str(&src).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidId { .. }));
    }

    #[test]
    fn rejects_id_with_space() {
        let src = MINIMAL_STDIO.replace("\"my-weather\"", "\"my plugin\"");
        let err = ExtensionManifest::from_str(&src).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidId { .. }));
    }

    #[test]
    fn rejects_reserved_id() {
        for reserved in ["browser", "memory", "whatsapp"] {
            let src = MINIMAL_STDIO.replace("\"my-weather\"", &format!("\"{reserved}\""));
            let err = ExtensionManifest::from_str(&src).unwrap_err();
            assert!(
                matches!(err, ManifestError::ReservedId { .. }),
                "expected ReservedId for {reserved}, got {:?}",
                err
            );
        }
    }

    // --- Version rules ---------------------------------------------------------

    #[test]
    fn rejects_invalid_version() {
        let src = MINIMAL_STDIO.replace("\"0.1.0\"", "\"not-a-version\"");
        let err = ExtensionManifest::from_str(&src).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidVersion { .. }));
    }

    #[test]
    fn rejects_min_agent_version_above_current() {
        let src = r#"
[plugin]
id = "future-plugin"
version = "0.1.0"
min_agent_version = "999.0.0"

[capabilities]
tools = ["x"]

[transport]
kind = "stdio"
command = "./x"
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(matches!(err, ManifestError::AgentTooOld { .. }));
    }

    // --- Capability rules ------------------------------------------------------

    #[test]
    fn rejects_no_capabilities() {
        let src = r#"
[plugin]
id = "empty-caps"
version = "0.1.0"

[capabilities]

[transport]
kind = "stdio"
command = "./x"
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(matches!(err, ManifestError::NoCapabilities));
    }

    #[test]
    fn rejects_duplicate_tool() {
        let src = r#"
[plugin]
id = "dup-tools"
version = "0.1.0"

[capabilities]
tools = ["a", "a"]

[transport]
kind = "stdio"
command = "./x"
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(
            matches!(
                err,
                ManifestError::DuplicateCapabilityName { field: "tools", .. }
            ),
            "got {:?}",
            err
        );
    }

    #[test]
    fn rejects_invalid_tool_name() {
        let src = r#"
[plugin]
id = "bad-tool"
version = "0.1.0"

[capabilities]
tools = ["Get-Weather"]

[transport]
kind = "stdio"
command = "./x"
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidCapabilityName { .. }));
    }

    // --- Transport rules -------------------------------------------------------

    #[test]
    fn rejects_stdio_with_empty_command() {
        let src = MINIMAL_STDIO.replace("\"./my-weather\"", "\"\"");
        let err = ExtensionManifest::from_str(&src).unwrap_err();
        assert!(matches!(err, ManifestError::EmptyStdioCommand));
    }

    #[test]
    fn rejects_nats_with_empty_prefix() {
        let src = r#"
[plugin]
id = "n"
version = "0.1.0"

[capabilities]
tools = ["x"]

[transport]
kind = "nats"
subject_prefix = ""
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(matches!(err, ManifestError::EmptyNatsPrefix));
    }

    #[test]
    fn rejects_http_with_non_http_scheme() {
        let src = r#"
[plugin]
id = "bad-http"
version = "0.1.0"

[capabilities]
tools = ["x"]

[transport]
kind = "http"
url = "ftp://example.com"
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidHttpUrl { .. }));
    }

    // --- I/O -------------------------------------------------------------------

    #[test]
    fn from_path_reads_tempfile() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(MINIMAL_STDIO.as_bytes()).unwrap();
        let m = ExtensionManifest::from_path(f.path()).unwrap();
        assert_eq!(m.id(), "my-weather");
    }

    #[test]
    fn from_path_missing_file_returns_read_error() {
        let err = ExtensionManifest::from_path(Path::new("/definitely/not/here.toml")).unwrap_err();
        assert!(matches!(err, ManifestError::Read { .. }));
    }

    // --- Misc ------------------------------------------------------------------

    #[test]
    fn rejects_description_too_long() {
        let long = "x".repeat(MAX_DESC_LEN + 1);
        let src = format!(
            r#"
[plugin]
id = "desc"
version = "0.1.0"
description = "{long}"

[capabilities]
tools = ["x"]

[transport]
kind = "stdio"
command = "./x"
"#
        );
        let err = ExtensionManifest::from_str(&src).unwrap_err();
        assert!(matches!(err, ManifestError::DescriptionTooLong));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let src = r#"
[plugin]
id = "x"
version = "0.1.0"

[capabilities]
tools = ["x"]

[transport]
kind = "stdio"
command = "./x"

[wat]
foo = 1
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)), "got {:?}", err);
    }

    #[test]
    fn rejects_unknown_transport_kind() {
        let src = r#"
[plugin]
id = "x"
version = "0.1.0"

[capabilities]
tools = ["x"]

[transport]
kind = "websocket"
url = "ws://x"
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)), "got {:?}", err);
    }

    // ─── Phase 12.7 — mcp_servers parsing and validation ───────────────────

    #[test]
    fn parses_inline_mcp_servers() {
        let src = r#"
[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["ping"]

[transport]
kind = "stdio"
command = "/bin/true"

[mcp_servers.weather-api]
transport = "streamable_http"
url = "https://api.example.com/mcp"

[mcp_servers.weather-api.headers]
Authorization = "Bearer x"

[mcp_servers.local]
transport = "stdio"
command = "${EXTENSION_ROOT}/bin/geo"
"#;
        let m = ExtensionManifest::from_str(src).unwrap();
        assert_eq!(m.mcp_servers.len(), 2);
        assert!(m.mcp_servers.contains_key("weather-api"));
        assert!(m.mcp_servers.contains_key("local"));
    }

    #[test]
    fn rejects_invalid_mcp_server_key() {
        let src = r#"
[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["ping"]

[transport]
kind = "stdio"
command = "/bin/true"

[mcp_servers.BadName]
transport = "stdio"
command = "/bin/true"
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(
            matches!(err, ManifestError::InvalidMcpServerName { ref name } if name == "BadName"),
            "got {:?}",
            err
        );
    }

    #[test]
    fn rejects_mcp_stdio_empty_command() {
        let src = r#"
[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["ping"]

[transport]
kind = "stdio"
command = "/bin/true"

[mcp_servers.local]
transport = "stdio"
command = ""
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(
            matches!(err, ManifestError::McpEmptyStdioCommand { ref name } if name == "local"),
            "got {:?}",
            err
        );
    }

    #[test]
    fn rejects_mcp_http_non_http_scheme() {
        let src = r#"
[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["ping"]

[transport]
kind = "stdio"
command = "/bin/true"

[mcp_servers.api]
transport = "streamable_http"
url = "ftp://example.com/mcp"
"#;
        let err = ExtensionManifest::from_str(src).unwrap_err();
        assert!(
            matches!(err, ManifestError::McpInvalidHttpUrl { ref name, .. } if name == "api"),
            "got {:?}",
            err
        );
    }
}
