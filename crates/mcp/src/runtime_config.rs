//! Runtime-side MCP config — per-server launch descriptors.
//!
//! Produced from `agent_config::McpConfig` at startup. 12.2 expanded the
//! shape with HTTP variants; the existing stdio `McpServerConfig` (12.1)
//! is unchanged and embedded in `McpServerRuntimeConfig::Stdio`.

use std::collections::BTreeMap;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::config::McpServerConfig;
use crate::http::HttpTransportMode;
use crate::resource_cache::ResourceCacheConfig;

/// Per-server launch descriptor. Wraps stdio and HTTP variants.
#[derive(Debug, Clone)]
pub enum McpServerRuntimeConfig {
    Stdio(McpServerConfig),
    Http {
        name: String,
        url: url::Url,
        transport: HttpTransportMode,
        /// Ordered header pairs (stable via `BTreeMap` in the YAML layer).
        /// Values arrive already env-resolved by `AppConfig::load`.
        headers: Vec<(String, String)>,
        connect_timeout: Duration,
        initialize_timeout: Duration,
        call_timeout: Duration,
        shutdown_grace: Duration,
        /// Phase 12.8 — if Some and server advertises `logging`
        /// capability, client sends `logging/setLevel` post-init.
        log_level: Option<String>,
        /// Phase 12.8 — per-server override for `mcp.context.passthrough`.
        context_passthrough: Option<bool>,
    },
}

impl McpServerRuntimeConfig {
    pub fn name(&self) -> &str {
        match self {
            Self::Stdio(s) => &s.name,
            Self::Http { name, .. } => name,
        }
    }
}

/// Runtime-level config for the MCP subsystem.
#[derive(Debug, Clone)]
pub struct McpRuntimeConfig {
    /// Sorted by name for stable fingerprints.
    pub servers: Vec<McpServerRuntimeConfig>,
    pub session_ttl: Duration,
    pub idle_reap_interval: Duration,
    /// Phase 12.8 follow-up — opt-in reset when a live server's
    /// `log_level` is unset on hot-reload (Some→None).
    pub reset_level_on_unset: bool,
    /// Phase 12.8 follow-up — level sent when `reset_level_on_unset`
    /// fires. Default `"info"`.
    pub default_reset_level: String,
    /// Phase 12.5 follow-up — LRU+TTL cache for `resources/read`. Off
    /// by default.
    pub resource_cache: ResourceCacheConfig,
    /// Phase 12.5 follow-up — opt-in URI scheme allowlist (e.g.
    /// `["file", "db"]`). Empty = permissive; otherwise violations are
    /// logged + counted before the call is dispatched.
    pub resource_uri_allowlist: Vec<String>,
}

/// Phase 12.7 — extension-declared MCP servers to merge into the runtime.
/// Caller (agent-core / main) obtains these via
/// `agent_extensions::collect_mcp_declarations`.
#[derive(Debug, Clone)]
pub struct ExtensionServerDecl {
    pub ext_id: String,
    pub ext_version: String,
    pub ext_root: std::path::PathBuf,
    pub servers: std::collections::BTreeMap<String, agent_config::McpServerYaml>,
}

impl McpRuntimeConfig {
    pub fn from_yaml(cfg: &agent_config::McpConfig) -> Self {
        Self::from_yaml_with_extensions(cfg, &[])
    }

    /// Merge yaml-declared servers with extension-declared servers. YAML
    /// wins on full-key collision (`{ext_id}.{name}`); `${EXTENSION_ROOT}`
    /// placeholders are expanded at merge time.
    ///
    /// Operational note: keys containing `.` in `mcp.yaml` are treated as
    /// explicit namespace keys and are primarily intended for this
    /// shadowing behavior.
    pub fn from_yaml_with_extensions(
        cfg: &agent_config::McpConfig,
        extensions: &[ExtensionServerDecl],
    ) -> Self {
        let mut servers: Vec<McpServerRuntimeConfig> = cfg
            .servers
            .iter()
            .map(|(name, raw)| server_from_yaml(name, raw, cfg))
            .collect();
        let yaml_keys: std::collections::HashSet<String> =
            servers.iter().map(|s| s.name().to_string()).collect();

        for decl in extensions {
            for (name, raw) in &decl.servers {
                let full_name = format!("{}.{}", decl.ext_id, name);
                if yaml_keys.contains(&full_name) {
                    tracing::warn!(
                        ext_id = %decl.ext_id,
                        server = %name,
                        full = %full_name,
                        "yaml mcp server shadows extension declaration"
                    );
                    continue;
                }
                if let Some((cmd, resolved)) =
                    detect_relative_stdio_without_root_placeholder(raw, &decl.ext_root)
                {
                    tracing::warn!(
                        ext_id = %decl.ext_id,
                        server = %name,
                        command = %cmd,
                        "extension stdio command is relative and does not use ${{EXTENSION_ROOT}}"
                    );
                    tracing::debug!(
                        ext_id = %decl.ext_id,
                        server = %name,
                        resolved = %resolved,
                        "resolved relative stdio command path"
                    );
                }
                let expanded = apply_extension_root(
                    raw.clone(),
                    &decl.ext_root,
                    &decl.ext_id,
                    &decl.ext_version,
                );
                let expanded = apply_manifest_env_placeholders(expanded, &full_name);
                if let Some(bad) = detect_escape(&expanded, &decl.ext_root) {
                    if cfg.strict_root_paths {
                        tracing::error!(
                            ext_id = %decl.ext_id,
                            server = %name,
                            path = %bad,
                            root = %decl.ext_root.display(),
                            "rejecting MCP declaration: path escapes extension root (strict_root_paths=true)"
                        );
                        continue;
                    }
                    tracing::warn!(
                        ext_id = %decl.ext_id,
                        server = %name,
                        path = %bad,
                        root = %decl.ext_root.display(),
                        "extension command escapes extension root"
                    );
                }
                servers.push(server_from_yaml(&full_name, &expanded, cfg));
            }
        }
        servers.sort_by(|a, b| a.name().cmp(b.name()));
        Self {
            servers,
            session_ttl: cfg.session_ttl,
            idle_reap_interval: cfg.idle_reap_interval,
            reset_level_on_unset: cfg.reset_level_on_unset,
            default_reset_level: cfg.default_reset_level.clone(),
            resource_cache: ResourceCacheConfig {
                enabled: cfg.resource_cache.enabled,
                ttl: cfg.resource_cache.ttl,
                max_entries: cfg.resource_cache.max_entries,
            },
            resource_uri_allowlist: cfg.resource_uri_allowlist.clone(),
        }
    }

    /// Stable hex SHA256 fingerprint. Excludes timeouts so tweaking them
    /// doesn't tear existing runtimes. Includes variant tag so flipping
    /// stdio ↔ http for the same name is a real change.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        let canonical: Vec<FingerprintEntry<'_>> =
            self.servers.iter().map(fingerprint_entry).collect();
        let json = serde_json::to_string(&canonical).unwrap_or_else(|_| String::from("<invalid>"));
        hasher.update(json.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

fn server_from_yaml(
    name: &str,
    raw: &agent_config::McpServerYaml,
    cfg: &agent_config::McpConfig,
) -> McpServerRuntimeConfig {
    match raw {
        agent_config::McpServerYaml::Stdio {
            command,
            args,
            env,
            cwd,
            log_level,
            context_passthrough,
        } => {
            let env_map: std::collections::HashMap<String, String> =
                env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            McpServerRuntimeConfig::Stdio(McpServerConfig {
                name: name.to_string(),
                command: command.clone(),
                args: args.clone(),
                env: env_map,
                cwd: cwd.as_ref().map(std::path::PathBuf::from),
                connect_timeout: cfg.connect_timeout,
                initialize_timeout: cfg.initialize_timeout,
                call_timeout: cfg.call_timeout,
                shutdown_grace: cfg.shutdown_grace,
                log_level: log_level.clone(),
                context_passthrough: *context_passthrough,
            })
        }
        agent_config::McpServerYaml::StreamableHttp {
            url,
            headers,
            log_level,
            context_passthrough,
        } => McpServerRuntimeConfig::Http {
            name: name.to_string(),
            url: url::Url::parse(url)
                .unwrap_or_else(|_| url::Url::parse("http://invalid.local").unwrap()),
            transport: HttpTransportMode::StreamableHttp,
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            connect_timeout: cfg.connect_timeout,
            initialize_timeout: cfg.initialize_timeout,
            call_timeout: cfg.call_timeout,
            shutdown_grace: cfg.shutdown_grace,
            log_level: log_level.clone(),
            context_passthrough: *context_passthrough,
        },
        agent_config::McpServerYaml::Sse {
            url,
            headers,
            log_level,
            context_passthrough,
        } => McpServerRuntimeConfig::Http {
            name: name.to_string(),
            url: url::Url::parse(url)
                .unwrap_or_else(|_| url::Url::parse("http://invalid.local").unwrap()),
            transport: HttpTransportMode::Sse,
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            connect_timeout: cfg.connect_timeout,
            initialize_timeout: cfg.initialize_timeout,
            call_timeout: cfg.call_timeout,
            shutdown_grace: cfg.shutdown_grace,
            log_level: log_level.clone(),
            context_passthrough: *context_passthrough,
        },
        agent_config::McpServerYaml::Auto {
            url,
            headers,
            log_level,
            context_passthrough,
        } => McpServerRuntimeConfig::Http {
            name: name.to_string(),
            url: url::Url::parse(url)
                .unwrap_or_else(|_| url::Url::parse("http://invalid.local").unwrap()),
            transport: HttpTransportMode::Auto,
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            connect_timeout: cfg.connect_timeout,
            initialize_timeout: cfg.initialize_timeout,
            call_timeout: cfg.call_timeout,
            shutdown_grace: cfg.shutdown_grace,
            log_level: log_level.clone(),
            context_passthrough: *context_passthrough,
        },
    }
}

#[derive(serde::Serialize)]
#[serde(tag = "kind")]
enum FingerprintEntry<'a> {
    Stdio {
        name: &'a str,
        command: &'a str,
        args: &'a [String],
        env: BTreeMap<&'a str, &'a str>,
        cwd: Option<String>,
    },
    Http {
        name: &'a str,
        transport: &'static str,
        url: String,
        headers: Vec<(&'a str, &'a str)>,
    },
}

/// Replace extension placeholders with concrete values in every field
/// carried by `McpServerYaml`.
///
/// Supported tokens:
/// - `${EXTENSION_ROOT}`
/// - `${EXTENSION_ID}`
/// - `${EXTENSION_VERSION}`
/// - `${AGENT_VERSION}`
///
/// Other `${...}` tokens are left literal here and handled by
/// `apply_manifest_env_placeholders`.
fn apply_extension_root(
    server: agent_config::McpServerYaml,
    ext_root: &std::path::Path,
    ext_id: &str,
    ext_version: &str,
) -> agent_config::McpServerYaml {
    let root = ext_root.to_string_lossy().to_string();
    let agent_version = env!("CARGO_PKG_VERSION");
    let sub = |s: String| {
        s.replace("${EXTENSION_ROOT}", &root)
            .replace("${EXTENSION_ID}", ext_id)
            .replace("${EXTENSION_VERSION}", ext_version)
            .replace("${AGENT_VERSION}", agent_version)
    };
    match server {
        agent_config::McpServerYaml::Stdio {
            command,
            args,
            env,
            cwd,
            log_level,
            context_passthrough,
        } => agent_config::McpServerYaml::Stdio {
            command: sub(command),
            args: args.into_iter().map(sub).collect(),
            env: env.into_iter().map(|(k, v)| (k, sub(v))).collect(),
            cwd: cwd.map(sub),
            log_level: log_level.map(sub),
            context_passthrough,
        },
        agent_config::McpServerYaml::StreamableHttp {
            url,
            headers,
            log_level,
            context_passthrough,
        } => agent_config::McpServerYaml::StreamableHttp {
            url,
            headers: headers.into_iter().map(|(k, v)| (k, sub(v))).collect(),
            log_level: log_level.map(sub),
            context_passthrough,
        },
        agent_config::McpServerYaml::Sse {
            url,
            headers,
            log_level,
            context_passthrough,
        } => agent_config::McpServerYaml::Sse {
            url,
            headers: headers.into_iter().map(|(k, v)| (k, sub(v))).collect(),
            log_level: log_level.map(sub),
            context_passthrough,
        },
        agent_config::McpServerYaml::Auto {
            url,
            headers,
            log_level,
            context_passthrough,
        } => agent_config::McpServerYaml::Auto {
            url,
            headers: headers.into_iter().map(|(k, v)| (k, sub(v))).collect(),
            log_level: log_level.map(sub),
            context_passthrough,
        },
    }
}

/// Best-effort `${ENV}` / `${file:...}` expansion for extension-manifest MCP
/// declarations. We intentionally fail-open (warn + keep literal) so a
/// missing env var doesn't drop the entire declaration at runtime.
fn apply_manifest_env_placeholders(
    server: agent_config::McpServerYaml,
    source: &str,
) -> agent_config::McpServerYaml {
    let sub = |s: String| match agent_config::env::resolve_placeholders(&s, source) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                server = %source,
                error = %e,
                "failed to resolve manifest env placeholder; keeping literal value"
            );
            s
        }
    };
    match server {
        agent_config::McpServerYaml::Stdio {
            command,
            args,
            env,
            cwd,
            log_level,
            context_passthrough,
        } => agent_config::McpServerYaml::Stdio {
            command: sub(command),
            args: args.into_iter().map(sub).collect(),
            env: env.into_iter().map(|(k, v)| (k, sub(v))).collect(),
            cwd: cwd.map(sub),
            log_level: log_level.map(sub),
            context_passthrough,
        },
        agent_config::McpServerYaml::StreamableHttp {
            url,
            headers,
            log_level,
            context_passthrough,
        } => agent_config::McpServerYaml::StreamableHttp {
            url: sub(url),
            headers: headers.into_iter().map(|(k, v)| (k, sub(v))).collect(),
            log_level: log_level.map(sub),
            context_passthrough,
        },
        agent_config::McpServerYaml::Sse {
            url,
            headers,
            log_level,
            context_passthrough,
        } => agent_config::McpServerYaml::Sse {
            url: sub(url),
            headers: headers.into_iter().map(|(k, v)| (k, sub(v))).collect(),
            log_level: log_level.map(sub),
            context_passthrough,
        },
        agent_config::McpServerYaml::Auto {
            url,
            headers,
            log_level,
            context_passthrough,
        } => agent_config::McpServerYaml::Auto {
            url: sub(url),
            headers: headers.into_iter().map(|(k, v)| (k, sub(v))).collect(),
            log_level: log_level.map(sub),
            context_passthrough,
        },
    }
}

/// Detect whether a stdio command resolves to a path outside `ext_root`.
/// Returns the offending path (for logging) or None. Commands without path
/// separators are treated as PATH lookups (never "escape").
fn detect_escape(
    server: &agent_config::McpServerYaml,
    ext_root: &std::path::Path,
) -> Option<String> {
    let agent_config::McpServerYaml::Stdio { command, .. } = server else {
        return None;
    };
    let candidate = std::path::Path::new(command);
    if !candidate.is_absolute() {
        return None;
    }
    if candidate.starts_with(ext_root) {
        return None;
    }
    Some(command.clone())
}

/// Detect stdio commands that are relative and don't use
/// `${EXTENSION_ROOT}`. Returns the raw command plus the extension-root
/// joined path used for debug logs.
fn detect_relative_stdio_without_root_placeholder(
    server: &agent_config::McpServerYaml,
    ext_root: &std::path::Path,
) -> Option<(String, String)> {
    let agent_config::McpServerYaml::Stdio { command, .. } = server else {
        return None;
    };
    if command.contains("${EXTENSION_ROOT}") {
        return None;
    }
    let p = std::path::Path::new(command);
    if p.is_absolute() {
        return None;
    }
    let resolved = ext_root.join(p).to_string_lossy().to_string();
    Some((command.clone(), resolved))
}

fn fingerprint_entry(s: &McpServerRuntimeConfig) -> FingerprintEntry<'_> {
    match s {
        McpServerRuntimeConfig::Stdio(cfg) => {
            let env: BTreeMap<&str, &str> = cfg
                .env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            FingerprintEntry::Stdio {
                name: &cfg.name,
                command: &cfg.command,
                args: &cfg.args,
                env,
                cwd: cfg.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
            }
        }
        McpServerRuntimeConfig::Http {
            name,
            transport,
            url,
            headers,
            ..
        } => FingerprintEntry::Http {
            name,
            transport: match transport {
                HttpTransportMode::StreamableHttp => "streamable_http",
                HttpTransportMode::Sse => "sse",
                HttpTransportMode::Auto => "auto",
            },
            url: url.as_str().to_string(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap as YamlMap;

    fn yaml_cfg() -> agent_config::McpConfig {
        let mut servers = YamlMap::new();
        servers.insert(
            "b".to_string(),
            agent_config::McpServerYaml::Stdio {
                command: "cmd_b".into(),
                args: vec!["--flag".into()],
                env: YamlMap::new(),
                cwd: None,
                log_level: None,
                context_passthrough: None,
            },
        );
        servers.insert(
            "a".to_string(),
            agent_config::McpServerYaml::Stdio {
                command: "cmd_a".into(),
                args: vec![],
                env: YamlMap::from([("K".into(), "v".into())]),
                cwd: Some("/tmp".into()),
                log_level: None,
                context_passthrough: None,
            },
        );
        agent_config::McpConfig {
            servers,
            ..Default::default()
        }
    }

    #[test]
    fn fingerprint_stable_across_runs() {
        let c = McpRuntimeConfig::from_yaml(&yaml_cfg());
        assert_eq!(c.fingerprint(), c.fingerprint());
    }

    #[test]
    fn fingerprint_changes_with_command() {
        let mut yaml = yaml_cfg();
        let c1 = McpRuntimeConfig::from_yaml(&yaml);
        if let agent_config::McpServerYaml::Stdio { command, .. } =
            yaml.servers.get_mut("a").unwrap()
        {
            *command = "different".into();
        }
        let c2 = McpRuntimeConfig::from_yaml(&yaml);
        assert_ne!(c1.fingerprint(), c2.fingerprint());
    }

    #[test]
    fn fingerprint_excludes_timeouts() {
        let yaml1 = yaml_cfg();
        let mut yaml2 = yaml_cfg();
        yaml2.call_timeout = Duration::from_millis(1);
        yaml2.session_ttl = Duration::from_secs(1);
        let c1 = McpRuntimeConfig::from_yaml(&yaml1);
        let c2 = McpRuntimeConfig::from_yaml(&yaml2);
        assert_eq!(c1.fingerprint(), c2.fingerprint());
    }

    #[test]
    fn fingerprint_independent_of_server_insertion_order() {
        let mut a = YamlMap::new();
        a.insert(
            "x".into(),
            agent_config::McpServerYaml::Stdio {
                command: "cx".into(),
                args: vec![],
                env: YamlMap::new(),
                cwd: None,
                log_level: None,
                context_passthrough: None,
            },
        );
        a.insert(
            "y".into(),
            agent_config::McpServerYaml::Stdio {
                command: "cy".into(),
                args: vec![],
                env: YamlMap::new(),
                cwd: None,
                log_level: None,
                context_passthrough: None,
            },
        );
        let mut b = YamlMap::new();
        b.insert(
            "y".into(),
            agent_config::McpServerYaml::Stdio {
                command: "cy".into(),
                args: vec![],
                env: YamlMap::new(),
                cwd: None,
                log_level: None,
                context_passthrough: None,
            },
        );
        b.insert(
            "x".into(),
            agent_config::McpServerYaml::Stdio {
                command: "cx".into(),
                args: vec![],
                env: YamlMap::new(),
                cwd: None,
                log_level: None,
                context_passthrough: None,
            },
        );

        let c1 = McpRuntimeConfig::from_yaml(&agent_config::McpConfig {
            servers: a,
            ..Default::default()
        });
        let c2 = McpRuntimeConfig::from_yaml(&agent_config::McpConfig {
            servers: b,
            ..Default::default()
        });
        assert_eq!(c1.fingerprint(), c2.fingerprint());
    }

    #[test]
    fn fingerprint_differs_between_stdio_and_http_same_name() {
        let mut a = YamlMap::new();
        a.insert(
            "x".into(),
            agent_config::McpServerYaml::Stdio {
                command: "cmd".into(),
                args: vec![],
                env: YamlMap::new(),
                cwd: None,
                log_level: None,
                context_passthrough: None,
            },
        );
        let mut b = YamlMap::new();
        b.insert(
            "x".into(),
            agent_config::McpServerYaml::StreamableHttp {
                url: "https://x.example/".into(),
                headers: YamlMap::new(),
                log_level: None,
                context_passthrough: None,
            },
        );
        let c1 = McpRuntimeConfig::from_yaml(&agent_config::McpConfig {
            servers: a,
            ..Default::default()
        });
        let c2 = McpRuntimeConfig::from_yaml(&agent_config::McpConfig {
            servers: b,
            ..Default::default()
        });
        assert_ne!(c1.fingerprint(), c2.fingerprint());
    }

    #[test]
    fn apply_extension_root_substitutes_stdio_fields() {
        let mut env = YamlMap::new();
        env.insert(
            "DB_PATH".to_string(),
            "${EXTENSION_ROOT}/data/db".to_string(),
        );
        let server = agent_config::McpServerYaml::Stdio {
            command: "${EXTENSION_ROOT}/bin/geo".into(),
            args: vec!["--root".into(), "${EXTENSION_ROOT}/assets".into()],
            env,
            cwd: Some("${EXTENSION_ROOT}".into()),
            log_level: None,
            context_passthrough: None,
        };
        let out = apply_extension_root(
            server,
            std::path::Path::new("/ext/weather"),
            "weather",
            "1.2.3",
        );
        match out {
            agent_config::McpServerYaml::Stdio {
                command,
                args,
                env,
                cwd,
                ..
            } => {
                assert_eq!(command, "/ext/weather/bin/geo");
                assert_eq!(args[1], "/ext/weather/assets");
                assert_eq!(env.get("DB_PATH").unwrap(), "/ext/weather/data/db");
                assert_eq!(cwd.as_deref(), Some("/ext/weather"));
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn apply_extension_root_substitutes_id_version_and_agent_version() {
        let mut env = YamlMap::new();
        env.insert(
            "X-ID".to_string(),
            "${EXTENSION_ID}:${EXTENSION_VERSION}".to_string(),
        );
        env.insert("X-AGENT".to_string(), "${AGENT_VERSION}".to_string());
        let server = agent_config::McpServerYaml::Stdio {
            command: "${EXTENSION_ID}".into(),
            args: vec!["${EXTENSION_VERSION}".into(), "${AGENT_VERSION}".into()],
            env,
            cwd: None,
            log_level: Some("${EXTENSION_ID}/${EXTENSION_VERSION}".into()),
            context_passthrough: None,
        };
        let out = apply_extension_root(
            server,
            std::path::Path::new("/ext/weather"),
            "weather",
            "1.2.3",
        );
        match out {
            agent_config::McpServerYaml::Stdio {
                command,
                args,
                env,
                log_level,
                ..
            } => {
                assert_eq!(command, "weather");
                assert_eq!(args[0], "1.2.3");
                assert_eq!(args[1], env!("CARGO_PKG_VERSION"));
                assert_eq!(env.get("X-ID").unwrap(), "weather:1.2.3");
                assert_eq!(env.get("X-AGENT").unwrap(), env!("CARGO_PKG_VERSION"));
                assert_eq!(log_level.as_deref(), Some("weather/1.2.3"));
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn apply_extension_root_leaves_url_but_substitutes_headers() {
        let mut headers = YamlMap::new();
        headers.insert("X-Root".into(), "${EXTENSION_ROOT}/tokens".into());
        let server = agent_config::McpServerYaml::StreamableHttp {
            url: "https://example.com/mcp".into(),
            headers,
            log_level: None,
            context_passthrough: None,
        };
        let out = apply_extension_root(server, std::path::Path::new("/ext/x"), "weather", "1.2.3");
        match out {
            agent_config::McpServerYaml::StreamableHttp { url, headers, .. } => {
                assert_eq!(url, "https://example.com/mcp");
                assert_eq!(headers.get("X-Root").unwrap(), "/ext/x/tokens");
            }
            _ => panic!("expected StreamableHttp"),
        }
    }

    #[test]
    fn from_yaml_with_extensions_namespaces_and_merges() {
        use std::path::PathBuf;
        let yaml = yaml_cfg();
        let mut ext_servers = YamlMap::new();
        ext_servers.insert(
            "inside".into(),
            agent_config::McpServerYaml::Stdio {
                command: "${EXTENSION_ROOT}/bin/x".into(),
                args: vec![],
                env: YamlMap::new(),
                cwd: None,
                log_level: None,
                context_passthrough: None,
            },
        );
        let decl = ExtensionServerDecl {
            ext_id: "weather".into(),
            ext_version: "1.0.0".into(),
            ext_root: PathBuf::from("/ext/weather"),
            servers: ext_servers,
        };
        let c = McpRuntimeConfig::from_yaml_with_extensions(&yaml, &[decl]);
        let names: Vec<&str> = c.servers.iter().map(|s| s.name()).collect();
        assert!(names.contains(&"weather.inside"));
        // yaml has "a" and "b"; extension added one more.
        assert_eq!(c.servers.len(), 3);
    }

    #[test]
    fn from_yaml_with_extensions_yaml_wins_on_collision() {
        use std::path::PathBuf;
        let mut yaml_servers = YamlMap::new();
        yaml_servers.insert(
            "weather.local".to_string(),
            agent_config::McpServerYaml::Stdio {
                command: "/yaml/cmd".into(),
                args: vec![],
                env: YamlMap::new(),
                cwd: None,
                log_level: None,
                context_passthrough: None,
            },
        );
        let yaml = agent_config::McpConfig {
            servers: yaml_servers,
            ..Default::default()
        };
        let mut ext_servers = YamlMap::new();
        ext_servers.insert(
            "local".into(),
            agent_config::McpServerYaml::Stdio {
                command: "/ext/cmd".into(),
                args: vec![],
                env: YamlMap::new(),
                cwd: None,
                log_level: None,
                context_passthrough: None,
            },
        );
        let decl = ExtensionServerDecl {
            ext_id: "weather".into(),
            ext_version: "1.0.0".into(),
            ext_root: PathBuf::from("/ext/weather"),
            servers: ext_servers,
        };
        let c = McpRuntimeConfig::from_yaml_with_extensions(&yaml, &[decl]);
        assert_eq!(c.servers.len(), 1);
        match &c.servers[0] {
            McpServerRuntimeConfig::Stdio(s) => assert_eq!(s.command, "/yaml/cmd"),
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn strict_root_paths_rejects_escaping_extension_command() {
        use std::path::PathBuf;
        let mut yaml = yaml_cfg();
        yaml.strict_root_paths = true;
        let mut ext_servers = YamlMap::new();
        // Absolute command outside ext_root → escapes.
        ext_servers.insert(
            "evil".into(),
            agent_config::McpServerYaml::Stdio {
                command: "/etc/init.d/evil".into(),
                args: vec![],
                env: YamlMap::new(),
                cwd: None,
                log_level: None,
                context_passthrough: None,
            },
        );
        let decl = ExtensionServerDecl {
            ext_id: "weather".into(),
            ext_version: "1.0.0".into(),
            ext_root: PathBuf::from("/ext/weather"),
            servers: ext_servers,
        };
        let c = McpRuntimeConfig::from_yaml_with_extensions(&yaml, &[decl]);
        let names: Vec<&str> = c.servers.iter().map(|s| s.name()).collect();
        assert!(
            !names.contains(&"weather.evil"),
            "strict_root_paths must drop the escaping server"
        );
        // yaml's own servers still present.
        assert_eq!(c.servers.len(), 2);
    }

    #[test]
    fn strict_root_paths_off_still_loads_with_warn() {
        use std::path::PathBuf;
        let yaml = yaml_cfg(); // default strict_root_paths=false
        let mut ext_servers = YamlMap::new();
        ext_servers.insert(
            "evil".into(),
            agent_config::McpServerYaml::Stdio {
                command: "/etc/init.d/evil".into(),
                args: vec![],
                env: YamlMap::new(),
                cwd: None,
                log_level: None,
                context_passthrough: None,
            },
        );
        let decl = ExtensionServerDecl {
            ext_id: "weather".into(),
            ext_version: "1.0.0".into(),
            ext_root: PathBuf::from("/ext/weather"),
            servers: ext_servers,
        };
        let c = McpRuntimeConfig::from_yaml_with_extensions(&yaml, &[decl]);
        let names: Vec<&str> = c.servers.iter().map(|s| s.name()).collect();
        assert!(
            names.contains(&"weather.evil"),
            "default behavior must still load the escaping server (warn-only)"
        );
    }

    #[test]
    fn from_yaml_with_extensions_expands_manifest_env_placeholders() {
        use std::path::PathBuf;
        std::env::set_var("MCP_EXT_CMD", "/bin/env");
        std::env::set_var("MCP_EXT_TOKEN", "abc123");

        let yaml = yaml_cfg();
        let mut ext_servers = YamlMap::new();
        ext_servers.insert(
            "local".into(),
            agent_config::McpServerYaml::Stdio {
                command: "${MCP_EXT_CMD}".into(),
                args: vec!["--token=${MCP_EXT_TOKEN}".into()],
                env: YamlMap::new(),
                cwd: None,
                log_level: Some("${MCP_EXT_TOKEN}".into()),
                context_passthrough: None,
            },
        );
        let decl = ExtensionServerDecl {
            ext_id: "weather".into(),
            ext_version: "1.0.0".into(),
            ext_root: PathBuf::from("/ext/weather"),
            servers: ext_servers,
        };

        let c = McpRuntimeConfig::from_yaml_with_extensions(&yaml, &[decl]);
        let local = c
            .servers
            .iter()
            .find(|s| s.name() == "weather.local")
            .expect("weather.local must be merged");
        match local {
            McpServerRuntimeConfig::Stdio(s) => {
                assert_eq!(s.command, "/bin/env");
                assert_eq!(s.args, vec!["--token=abc123".to_string()]);
                assert_eq!(s.log_level.as_deref(), Some("abc123"));
            }
            _ => panic!("expected stdio"),
        }

        std::env::remove_var("MCP_EXT_CMD");
        std::env::remove_var("MCP_EXT_TOKEN");
    }

    #[test]
    fn apply_manifest_env_placeholders_fail_open_when_missing_var() {
        std::env::remove_var("MCP_EXT_NOT_SET");
        let in_server = agent_config::McpServerYaml::Stdio {
            command: "${MCP_EXT_NOT_SET}".into(),
            args: vec![],
            env: YamlMap::new(),
            cwd: None,
            log_level: None,
            context_passthrough: None,
        };
        let out = apply_manifest_env_placeholders(in_server, "weather.local");
        match out {
            agent_config::McpServerYaml::Stdio { command, .. } => {
                assert_eq!(command, "${MCP_EXT_NOT_SET}");
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn detect_relative_stdio_warns_without_extension_root_placeholder() {
        let s = agent_config::McpServerYaml::Stdio {
            command: "bin/local-server".into(),
            args: vec![],
            env: YamlMap::new(),
            cwd: None,
            log_level: None,
            context_passthrough: None,
        };
        let got = detect_relative_stdio_without_root_placeholder(
            &s,
            std::path::Path::new("/ext/weather"),
        )
        .expect("must warn for relative command");
        assert_eq!(got.0, "bin/local-server");
        assert_eq!(got.1, "/ext/weather/bin/local-server");
    }

    #[test]
    fn detect_relative_stdio_ignores_absolute_and_root_placeholder() {
        let abs = agent_config::McpServerYaml::Stdio {
            command: "/usr/bin/mcp-server".into(),
            args: vec![],
            env: YamlMap::new(),
            cwd: None,
            log_level: None,
            context_passthrough: None,
        };
        assert!(detect_relative_stdio_without_root_placeholder(
            &abs,
            std::path::Path::new("/ext/x")
        )
        .is_none());

        let templated = agent_config::McpServerYaml::Stdio {
            command: "${EXTENSION_ROOT}/bin/mcp-server".into(),
            args: vec![],
            env: YamlMap::new(),
            cwd: None,
            log_level: None,
            context_passthrough: None,
        };
        assert!(detect_relative_stdio_without_root_placeholder(
            &templated,
            std::path::Path::new("/ext/x")
        )
        .is_none());
    }

    #[test]
    fn fingerprint_varies_with_header_values() {
        let mut base = YamlMap::new();
        base.insert(
            "x".into(),
            agent_config::McpServerYaml::StreamableHttp {
                url: "https://x.example/".into(),
                headers: YamlMap::from([("A".into(), "1".into())]),
                log_level: None,
                context_passthrough: None,
            },
        );
        let mut bumped = YamlMap::new();
        bumped.insert(
            "x".into(),
            agent_config::McpServerYaml::StreamableHttp {
                url: "https://x.example/".into(),
                headers: YamlMap::from([("A".into(), "2".into())]),
                log_level: None,
                context_passthrough: None,
            },
        );
        let c1 = McpRuntimeConfig::from_yaml(&agent_config::McpConfig {
            servers: base,
            ..Default::default()
        });
        let c2 = McpRuntimeConfig::from_yaml(&agent_config::McpConfig {
            servers: bumped,
            ..Default::default()
        });
        assert_ne!(c1.fingerprint(), c2.fingerprint());
    }
}
