//! `manifest_version = 1` legacy compat layer.
//!
//! The framework used to ship two parallel manifest
//! parsers: `nexo-extensions::manifest` (strict —
//! `[plugin]`, `[capabilities]`, `[transport]`, `[meta]`) and
//! `nexo-plugin-manifest::manifest` (richer —
//! `[plugin.entrypoint]`, `[plugin.capabilities.admin]`,
//! `[plugin.capabilities.http_server]`, etc.). Now everything
//! unifies on the modern shape with
//! [`crate::CURRENT_MANIFEST_VERSION`]; the legacy shape gets
//! read by this module + auto-translated to v2 in-memory at
//! parse time.
//!
//! Plugin authors get one full minor cycle to migrate their
//! `plugin.toml` to v2; a deprecation warn fires once per
//! `(plugin_id, version)` tuple per process so a busy daemon
//! doesn't drown the operator in log noise.
//!
//! Pure self-contained: this module does NOT depend on
//! `nexo-extensions` (that would create a dependency cycle). The
//! v1 structs mirror `nexo-extensions::manifest` field-for-field.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::error::ManifestError;
use crate::manifest::{
    AdminCapabilities, Capabilities, EntrypointSection, HttpServerCapability, MetaSection,
    PluginManifest, PluginSection, SubscriptionsSection,
};

// ── v1 legacy struct mirrors ────────────────────────────────────

/// v1 root — flat top-level sections.
///
/// Mirrors `nexo_extensions::manifest::ExtensionManifest` so this
/// module stays self-contained. Field names + serde attrs MUST
/// stay byte-equivalent.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1Manifest {
    /// Operators MAY explicitly write `manifest_version = 1` to
    /// pin the legacy path. The dispatcher peels that off before
    /// reaching us, but a stricter `deny_unknown_fields` parse
    /// would reject the leftover field — so we accept + ignore
    /// it here too.
    #[serde(default)]
    #[allow(dead_code)]
    manifest_version: Option<u32>,
    plugin: LegacyV1PluginMeta,
    #[serde(default)]
    capabilities: LegacyV1Capabilities,
    transport: LegacyV1Transport,
    #[serde(default)]
    meta: LegacyV1Meta,
    /// MCP server declarations. Top-level table
    /// in v1; nested under `[plugin.mcp_servers]` after migration.
    #[serde(default)]
    mcp_servers: BTreeMap<String, toml::Value>,
    #[serde(default)]
    context: LegacyV1ContextConfig,
    #[serde(default)]
    requires: LegacyV1Requires,
    /// Outbound dispatch allowlist. Top-level table
    /// in v1.
    #[serde(default)]
    outbound_bindings: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1PluginMeta {
    id: String,
    version: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    min_agent_version: Option<String>,
    #[serde(default)]
    priority: i32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1Capabilities {
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    hooks: Vec<String>,
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default)]
    providers: Vec<String>,
    #[serde(default)]
    pollers: Vec<String>,
    /// Admin RPC capabilities. Field-for-field
    /// identical to the v2 [`AdminCapabilities`] so the migrator
    /// can pass it through.
    #[serde(default)]
    admin: LegacyV1AdminCapabilities,
    /// HTTP server. Field-for-field identical to
    /// v2 [`HttpServerCapability`].
    #[serde(default)]
    http_server: Option<LegacyV1HttpServerCapability>,
    /// Broker subscribe/publish allowlist. Field-
    /// for-field identical to v2 [`BrokerCapability`]; the migrator
    /// passes it through without dropping.
    #[serde(default)]
    broker: Option<LegacyV1BrokerCapability>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1BrokerCapability {
    #[serde(default)]
    subscribe: Vec<String>,
    #[serde(default)]
    publish: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1AdminCapabilities {
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    optional: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1HttpServerCapability {
    port: u16,
    #[serde(default = "default_legacy_http_bind")]
    bind: String,
    token_env: String,
    #[serde(default = "default_legacy_health_path")]
    health_path: String,
    /// Extra env-var passthrough allowlist.
    /// Microapps that proxy to a sibling extension on a
    /// shared bearer (e.g. agent-creator → marketing on
    /// `MARKETING_ADMIN_TOKEN`) declare them here so the
    /// daemon's secret-suffix blocklist doesn't strip the
    /// var at spawn. Field-for-field identical to v2
    /// [`HttpServerCapability::extra_env_passthrough`].
    /// Backwards-compat: v1 manifests pre-dating the field
    /// omit it; the migrator defaults to an empty vec.
    #[serde(default)]
    extra_env_passthrough: Vec<String>,
}

fn default_legacy_http_bind() -> String {
    "127.0.0.1".to_string()
}

fn default_legacy_health_path() -> String {
    "/healthz".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[allow(dead_code)] // Nats / Http variants accept the field for shape parity but
                    // the migrator drops them with a warning today; a full v2 home
                    // for non-stdio transports is a future addition.
enum LegacyV1Transport {
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
struct LegacyV1Meta {
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    repository: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1ContextConfig {
    #[serde(default)]
    passthrough: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1Requires {
    #[serde(default)]
    bins: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
}

// ── Migration ───────────────────────────────────────────────────

/// Outcome of a v1 → v2 migration. The translated manifest is
/// always returned; `dropped_fields` carries the list of legacy
/// fields that don't have a direct v2 equivalent in this release
/// (a future release adds homes for them). Callers log the list
/// once per plugin so the operator knows what wasn't translated.
pub struct MigrationOutcome {
    pub manifest: PluginManifest,
    pub dropped_fields: Vec<&'static str>,
}

/// Translate a parsed legacy v1 doc into the canonical v2 shape.
fn migrate_v1_to_v2(legacy: LegacyV1Manifest) -> Result<MigrationOutcome, ManifestError> {
    let mut dropped = Vec::new();

    // Transport → entrypoint. v2's EntrypointSection only models
    // stdio today; nats / http variants are warned + dropped.
    let entrypoint = match legacy.transport {
        LegacyV1Transport::Stdio { command, args } => EntrypointSection {
            command: Some(command),
            args,
            env: BTreeMap::new(),
        },
        LegacyV1Transport::Nats { .. } => {
            dropped.push("transport.kind=nats");
            EntrypointSection::default()
        }
        LegacyV1Transport::Http { .. } => {
            dropped.push("transport.kind=http");
            EntrypointSection::default()
        }
    };

    // Capabilities.admin and http_server pass through verbatim.
    let admin = AdminCapabilities {
        required: legacy.capabilities.admin.required,
        optional: legacy.capabilities.admin.optional,
    };
    let http_server = legacy
        .capabilities
        .http_server
        .map(|h| HttpServerCapability {
            port: h.port,
            bind: h.bind,
            token_env: h.token_env,
            health_path: h.health_path,
            // Pass the allowlist through verbatim. Older v1
            // manifests omit the field and arrive here as an
            // empty vec.
            extra_env_passthrough: h.extra_env_passthrough,
        });

    // Capabilities.tools/hooks/channels/providers/pollers — these
    // are runtime-registration informational lists in v1. v2 uses
    // typed sections (`[plugin.tools.exposed.*]`, etc.). Today
    // we drop them; a future release ships preservation.
    if !legacy.capabilities.tools.is_empty() {
        dropped.push("capabilities.tools");
    }
    if !legacy.capabilities.hooks.is_empty() {
        dropped.push("capabilities.hooks");
    }
    if !legacy.capabilities.channels.is_empty() {
        dropped.push("capabilities.channels");
    }
    if !legacy.capabilities.providers.is_empty() {
        dropped.push("capabilities.providers");
    }
    if !legacy.capabilities.pollers.is_empty() {
        dropped.push("capabilities.pollers");
    }
    if legacy
        .capabilities
        .broker
        .as_ref()
        .is_some_and(|b| !b.subscribe.is_empty() || !b.publish.is_empty())
    {
        // V1 manifests pre-date the v2 [plugin.capabilities.broker]
        // home; broker auto-mapping is opt-in via v2 manifest.
        dropped.push("capabilities.broker");
    }

    // Top-level v1 tables that don't yet have v2 homes (the wire
    // surface is deferred; runtime consumers still read them from
    // the legacy plugin.toml in the meantime).
    if !legacy.mcp_servers.is_empty() {
        dropped.push("mcp_servers");
    }
    if !legacy.outbound_bindings.is_empty() {
        dropped.push("outbound_bindings");
    }
    if legacy.context.passthrough {
        dropped.push("context.passthrough");
    }
    if !legacy.requires.bins.is_empty() {
        dropped.push("requires.bins");
    }
    if !legacy.requires.env.is_empty() {
        dropped.push("requires.env");
    }
    if legacy.plugin.priority != 0 {
        dropped.push("plugin.priority");
    }

    // Build v2 PluginSection. Defaults fill the modern-only
    // fields (extends/contracts/agents/skills/etc.) since v1
    // plugins didn't have them.
    let plugin = PluginSection {
        id: legacy.plugin.id,
        version: parse_version(&legacy.plugin.version, "plugin.version")?,
        name: legacy.plugin.name.unwrap_or_default(),
        description: legacy.plugin.description.unwrap_or_default(),
        min_nexo_version: parse_version_req(
            legacy
                .plugin
                .min_agent_version
                .as_deref()
                .unwrap_or(">=0.0.0"),
            "plugin.min_agent_version",
        )?,
        enabled_by_default: false,
        capabilities: Capabilities {
            provides: Vec::new(),
            admin,
            http_server,
            skills: Vec::new(),
            // V1 legacy plugins don't get auto-mapped broker
            // capability — broker access opts in via v2 manifest
            // and an explicit `[capabilities.broker]` table.
            broker: None,
        },
        tools: Default::default(),
        advisors: Default::default(),
        agents: Default::default(),
        channels: Default::default(),
        skills: Default::default(),
        config: Default::default(),
        extends: Default::default(),
        requires: Default::default(),
        capability_gates: Default::default(),
        ui: Default::default(),
        contracts: Default::default(),
        meta: MetaSection {
            author: legacy.meta.author,
            license: legacy.meta.license,
            homepage: legacy.meta.homepage,
            repository: legacy.meta.repository,
        },
        supervisor: Default::default(),
        sandbox: Default::default(),
        // V1 manifests pre-date `[plugin.subscriptions]`;
        // default to empty so legacy in-tree plugins keep
        // booting unchanged. Out-of-tree plugins that need
        // broker traffic upgraded to v2 alongside this field.
        subscriptions: SubscriptionsSection::default(),
        // V1 manifests pre-date the top-level `[plugin.http_server]`
        // documentation block — None matches the v1 wire shape.
        http_server: None,
        entrypoint,
    };

    Ok(MigrationOutcome {
        manifest: PluginManifest {
            manifest_version: 2,
            plugin,
        },
        dropped_fields: dropped,
    })
}

fn parse_version(s: &str, field: &'static str) -> Result<semver::Version, ManifestError> {
    semver::Version::parse(s).map_err(|_| ManifestError::VersionInvalid {
        field,
        value: s.to_string(),
    })
}

fn parse_version_req(s: &str, field: &'static str) -> Result<semver::VersionReq, ManifestError> {
    semver::VersionReq::parse(s).map_err(|_| ManifestError::VersionInvalid {
        field,
        value: s.to_string(),
    })
}

// ── Dispatcher ──────────────────────────────────────────────────

/// Parse a manifest TOML doc as either v2 (canonical) or v1
/// (legacy) and return a v2 [`PluginManifest`] alongside a flag
/// indicating which path was taken. Callers log a one-shot
/// deprecation warn for v1 plugins.
///
/// Detection strategy:
/// 1. If `manifest_version` is explicitly set:
///    - `2` → strict v2 parse.
///    - anything else (`1`, stray int) → v1 legacy parse + migrate.
/// 2. Otherwise, **try v2 parse first**. v2 is the canonical shape
///    going forward; most newer plugins won't bother
///    setting `manifest_version` even though they're v2-shaped.
///    Only fall back to v1 if the v2 parse rejects an unknown field
///    or a missing required field — those are the legacy markers.
///
/// This ordering keeps existing v2 manifests (which never set
/// `manifest_version` historically) parsing unchanged, while
/// real v1 docs (with `[transport]`, top-level `[capabilities]`,
/// etc.) cleanly fall through.
pub fn try_parse_v2_or_v1(raw: &str) -> Result<(PluginManifest, bool), ManifestError> {
    let value: toml::Value = toml::from_str(raw)?;
    let explicit_version = value
        .as_table()
        .and_then(|t| t.get("manifest_version"))
        .and_then(|v| v.as_integer());

    match explicit_version {
        Some(2) => {
            // Operator pinned v2 explicitly; strict parse — any
            // failure surfaces with the original error.
            let parsed: PluginManifest = toml::from_str(raw)?;
            Ok((parsed, false))
        }
        Some(1) => {
            // Operator pinned v1 explicitly; force legacy path.
            let legacy: LegacyV1Manifest = toml::from_str(raw)?;
            let outcome = migrate_v1_to_v2(legacy)?;
            if !outcome.dropped_fields.is_empty() {
                tracing::warn!(
                    plugin_id = %outcome.manifest.plugin.id,
                    dropped = ?outcome.dropped_fields,
                    "manifest_version=1 fields dropped during migration; full preservation is a future addition",
                );
            }
            Ok((outcome.manifest, true))
        }
        Some(other) => {
            use serde::de::Error as _;
            Err(ManifestError::Parse(toml::de::Error::custom(format!(
                "manifest_version `{other}` not supported (accepted: 1 | 2)"
            ))))
        }
        None => {
            // Try v2 first. Older v2 plugins didn't bother
            // setting manifest_version even though their shape is
            // canonical.
            match toml::from_str::<PluginManifest>(raw) {
                Ok(parsed) => Ok((parsed, false)),
                Err(_v2_err) => {
                    // v2 parse failed → assume legacy. Surface the
                    // legacy parse error if THAT also fails (more
                    // useful for the operator than the v2 error,
                    // which mentions modern field names they didn't
                    // write).
                    let legacy: LegacyV1Manifest = toml::from_str(raw)?;
                    let outcome = migrate_v1_to_v2(legacy)?;
                    if !outcome.dropped_fields.is_empty() {
                        tracing::warn!(
                            plugin_id = %outcome.manifest.plugin.id,
                            dropped = ?outcome.dropped_fields,
                            "manifest_version=1 fields dropped during migration; full preservation is a future addition",
                        );
                    }
                    Ok((outcome.manifest, true))
                }
            }
        }
    }
}

/// Process-wide dedup so the deprecation warn fires once per
/// `(plugin_id, version)` tuple. Without this a daemon that
/// rediscovers extensions on every config-reload would spam the
/// operator with the same warning per cycle.
static DEPRECATION_WARN_DEDUP: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    OnceLock::new();

/// Emit the one-shot deprecation warning for a plugin caught
/// using v1 schema. Idempotent per `(id, version)` tuple.
pub fn emit_v1_deprecation_warning(plugin_id: &str, plugin_version: &str) {
    let key = format!("{plugin_id}:{plugin_version}");
    let dedup = DEPRECATION_WARN_DEDUP
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    let mut guard = dedup.lock().unwrap_or_else(|p| p.into_inner());
    if guard.insert(key) {
        tracing::warn!(
            plugin_id = %plugin_id,
            plugin_version = %plugin_version,
            "manifest_version=1 is deprecated; please migrate to v2 before nexo-rs 0.2.0",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_minimal_manifest() -> &'static str {
        r#"
[plugin]
id = "agent-creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."

[transport]
kind = "stdio"
command = "./agent-creator"
"#
    }

    #[test]
    fn try_parse_v1_legacy_returns_was_v1_true() {
        let (manifest, was_v1) = try_parse_v2_or_v1(legacy_minimal_manifest()).unwrap();
        assert!(was_v1);
        assert_eq!(manifest.manifest_version, 2);
        assert_eq!(manifest.plugin.id, "agent-creator");
        assert_eq!(
            manifest.plugin.entrypoint.command.as_deref(),
            Some("./agent-creator")
        );
    }

    #[test]
    fn try_parse_v2_canonical_returns_was_v1_false() {
        let v2 = r#"
manifest_version = 2

[plugin]
id = "agent_creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."
min_nexo_version = ">=0.0.0"
"#;
        let (manifest, was_v1) = try_parse_v2_or_v1(v2).unwrap();
        assert!(!was_v1);
        assert_eq!(manifest.manifest_version, 2);
        assert_eq!(manifest.plugin.id, "agent_creator");
    }

    #[test]
    fn migrate_legacy_admin_caps_pass_through() {
        let raw = r#"
[plugin]
id = "agent-creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."

[capabilities.admin]
required = ["agents_crud", "skills_crud"]
optional = ["channels_crud"]

[transport]
kind = "stdio"
command = "./agent-creator"
"#;
        let (m, _) = try_parse_v2_or_v1(raw).unwrap();
        assert_eq!(
            m.plugin.capabilities.admin.required,
            vec!["agents_crud".to_string(), "skills_crud".into()]
        );
        assert_eq!(
            m.plugin.capabilities.admin.optional,
            vec!["channels_crud".to_string()]
        );
    }

    #[test]
    fn migrate_legacy_http_server_pass_through() {
        let raw = r#"
[plugin]
id = "agent-creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."

[capabilities.http_server]
port = 8765
bind = "127.0.0.1"
token_env = "AGENT_CREATOR_TOKEN"
health_path = "/healthz"

[transport]
kind = "stdio"
command = "./agent-creator"
"#;
        let (m, _) = try_parse_v2_or_v1(raw).unwrap();
        let http = m.plugin.capabilities.http_server.expect("http_server");
        assert_eq!(http.port, 8765);
        assert_eq!(http.bind, "127.0.0.1");
        assert_eq!(http.token_env, "AGENT_CREATOR_TOKEN");
        assert_eq!(http.health_path, "/healthz");
    }

    /// v1 manifests with the allowlist must
    /// translate verbatim. Without this fix the parser rejects
    /// the field as `deny_unknown_fields`, the manifest fails to
    /// load, and the microapp boots without an admin router →
    /// every admin RPC times out.
    #[test]
    fn migrate_legacy_http_server_propagates_extra_env_passthrough() {
        let raw = r#"
[plugin]
id = "agent-creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."

[capabilities.http_server]
port = 8765
bind = "127.0.0.1"
token_env = "AGENT_CREATOR_TOKEN"
health_path = "/healthz"
extra_env_passthrough = ["MARKETING_ADMIN_TOKEN"]

[transport]
kind = "stdio"
command = "./agent-creator"
"#;
        let (m, was_v1) = try_parse_v2_or_v1(raw).unwrap();
        assert!(was_v1);
        let http = m.plugin.capabilities.http_server.expect("http_server");
        assert_eq!(http.extra_env_passthrough, vec!["MARKETING_ADMIN_TOKEN"]);
    }

    /// v1 manifests without the field default
    /// to empty (back-compat).
    #[test]
    fn migrate_legacy_http_server_defaults_extra_env_passthrough_empty() {
        let raw = r#"
[plugin]
id = "agent-creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."

[capabilities.http_server]
port = 8765
bind = "127.0.0.1"
token_env = "AGENT_CREATOR_TOKEN"

[transport]
kind = "stdio"
command = "./agent-creator"
"#;
        let (m, _) = try_parse_v2_or_v1(raw).unwrap();
        let http = m.plugin.capabilities.http_server.expect("http_server");
        assert!(http.extra_env_passthrough.is_empty());
    }

    #[test]
    fn migrate_legacy_meta_block_to_v2() {
        let raw = r#"
[plugin]
id = "agent-creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."

[transport]
kind = "stdio"
command = "./agent-creator"

[meta]
author = "Cristian"
license = "MIT"
homepage = "https://example.com"
repository = "https://github.com/x/y"
"#;
        let (m, _) = try_parse_v2_or_v1(raw).unwrap();
        assert_eq!(m.plugin.meta.author.as_deref(), Some("Cristian"));
        assert_eq!(m.plugin.meta.license.as_deref(), Some("MIT"));
        assert_eq!(
            m.plugin.meta.homepage.as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            m.plugin.meta.repository.as_deref(),
            Some("https://github.com/x/y")
        );
    }

    #[test]
    fn migrate_legacy_min_agent_version_renames_to_min_nexo_version() {
        let raw = r#"
[plugin]
id = "agent-creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."
min_agent_version = ">=0.1.0"

[transport]
kind = "stdio"
command = "./agent-creator"
"#;
        let (m, _) = try_parse_v2_or_v1(raw).unwrap();
        assert_eq!(m.plugin.min_nexo_version.to_string(), ">=0.1.0");
    }

    #[test]
    fn migrate_legacy_drops_unsupported_fields_and_returns_list() {
        let raw = r#"
[plugin]
id = "agent-creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."

[capabilities]
tools = ["foo"]
hooks = ["before_message"]
channels = ["whatsapp"]
providers = ["minimax"]
pollers = ["agent_turn"]

[transport]
kind = "stdio"
command = "./agent-creator"

[mcp_servers.echo]
command = "./echo"

[outbound_bindings]
whatsapp = ["personal"]

[context]
passthrough = true

[requires]
bins = ["jq"]
env = ["MINIMAX_API_KEY"]
"#;
        let legacy: LegacyV1Manifest = toml::from_str(raw).unwrap();
        let outcome = migrate_v1_to_v2(legacy).unwrap();
        let dropped: std::collections::HashSet<&'static str> =
            outcome.dropped_fields.iter().copied().collect();
        for field in [
            "capabilities.tools",
            "capabilities.hooks",
            "capabilities.channels",
            "capabilities.providers",
            "capabilities.pollers",
            "mcp_servers",
            "outbound_bindings",
            "context.passthrough",
            "requires.bins",
            "requires.env",
        ] {
            assert!(
                dropped.contains(field),
                "expected `{field}` in dropped list; got {dropped:?}"
            );
        }
    }

    #[test]
    fn migrate_legacy_transport_nats_is_dropped_and_logged() {
        let raw = r#"
[plugin]
id = "remote-broker"
version = "0.0.1"
name = "Remote Broker"
description = "x."

[transport]
kind = "nats"
subject_prefix = "ext"
"#;
        let legacy: LegacyV1Manifest = toml::from_str(raw).unwrap();
        let outcome = migrate_v1_to_v2(legacy).unwrap();
        assert!(outcome.dropped_fields.contains(&"transport.kind=nats"));
        assert!(outcome.manifest.plugin.entrypoint.command.is_none());
    }

    #[test]
    fn try_parse_explicit_manifest_version_1_uses_legacy_path() {
        let raw = r#"
manifest_version = 1

[plugin]
id = "agent-creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."

[transport]
kind = "stdio"
command = "./agent-creator"
"#;
        let (m, was_v1) = try_parse_v2_or_v1(raw).unwrap();
        assert!(was_v1);
        assert_eq!(m.manifest_version, 2);
    }

    #[test]
    fn deprecation_warn_dedups_per_id_version() {
        // Idempotent — calling twice with the same key adds once.
        emit_v1_deprecation_warning("dedup-test-plugin", "0.0.1");
        emit_v1_deprecation_warning("dedup-test-plugin", "0.0.1");
        emit_v1_deprecation_warning("dedup-test-plugin", "0.0.2"); // different version, fires
        let dedup = DEPRECATION_WARN_DEDUP.get().unwrap();
        let guard = dedup.lock().unwrap();
        assert!(guard.contains("dedup-test-plugin:0.0.1"));
        assert!(guard.contains("dedup-test-plugin:0.0.2"));
    }

    #[test]
    fn try_parse_explicit_unsupported_manifest_version_errors() {
        let raw = r#"
manifest_version = 99

[plugin]
id = "agent-creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."
"#;
        let err = try_parse_v2_or_v1(raw).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("manifest_version `99`"), "got: {s}");
    }

    #[test]
    fn try_parse_unset_version_with_v2_shape_skips_legacy_path() {
        // Legitimate v2 doc that doesn't bother setting
        // manifest_version. The dispatcher should v2-parse first
        // + succeed without ever touching the v1 path.
        let raw = r#"
[plugin]
id = "agent_creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."
min_nexo_version = ">=0.0.0"

[plugin.entrypoint]
command = "./agent-creator"
"#;
        let (m, was_v1) = try_parse_v2_or_v1(raw).unwrap();
        assert!(!was_v1);
        assert_eq!(m.plugin.id, "agent_creator");
    }

    #[test]
    fn try_parse_v2_with_unsupported_extra_root_field_errors() {
        // v2 path is strict (deny_unknown_fields). A v2 doc with
        // an unexpected root key fails clearly.
        let raw = r#"
manifest_version = 2

[plugin]
id = "agent-creator"
version = "0.0.1"
name = "Agent Creator"
description = "Operator UI."
min_nexo_version = ">=0.0.0"

[unknown_section]
foo = "bar"
"#;
        let result = try_parse_v2_or_v1(raw);
        assert!(result.is_err());
    }
}
